use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use ironquill_core::{
    ContextUse, Delegate, DelegateEvent, DelegateReply, DelegateRequest, TokenCount, Usage, Usd,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::error::LlmError;

/// Tools Claude Code may not use when ironquill hands it a task. Commands are
/// ironquill's to run: its checks judge the change, so the agent must not
/// run them, or work around them, itself.
const DISALLOWED_TOOLS: &str = "Bash";

/// How much of Claude Code's error output is kept for the message.
const STDERR_LIMIT: usize = 4_000;

/// The Claude Code command line, run as an agent tasks are handed to.
///
/// It runs the `claude` program installed on the machine, as published, with
/// the person's own login: ironquill never reads or stores credentials. Each
/// task is a fresh Claude Code session that knows nothing of ironquill's
/// conversation; a follow-up, such as a failing check, continues that session.
#[derive(Debug, Clone)]
pub struct ClaudeCode {
    program: PathBuf,
}

impl ClaudeCode {
    /// Claude Code at `program`.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Claude Code as found on the `PATH`, if it is installed.
    pub fn find() -> Option<Self> {
        find_program("claude").map(Self::new)
    }

    fn command(&self, request: &DelegateRequest) -> Command {
        let mut command = Command::new(&self.program);
        command
            .arg("-p")
            .arg(&request.prompt)
            .args(["--output-format", "stream-json", "--verbose"])
            // Text arrives as it is written, not one message at a time.
            .arg("--include-partial-messages")
            // Edits inside the project need no approval; anything else that
            // would ask is refused, since nobody is there to answer.
            .args(["--permission-mode", "acceptEdits"])
            .args(["--disallowedTools", DISALLOWED_TOOLS])
            // No MCP server: the person's own connectors (mail, calendars)
            // have nothing to do with a task in this project.
            .arg("--strict-mcp-config")
            .args(["--append-system-prompt", &request.instructions])
            .current_dir(&request.directory)
            // Claude Code waits a few seconds for piped input otherwise.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A stopped request stops the agent too.
            .kill_on_drop(true);
        if !request.model.is_empty() {
            command.args(["--model", &request.model]);
        }
        if let Some(session) = &request.resume {
            command.args(["--resume", session]);
        }
        command
    }
}

/// `name` as found on the `PATH`, if it is there.
pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

impl Delegate for ClaudeCode {
    type Error = LlmError;

    async fn run(
        &self,
        request: &DelegateRequest,
        on_event: &mut (dyn FnMut(DelegateEvent) + Send),
    ) -> Result<DelegateReply, LlmError> {
        let mut child = self
            .command(request)
            .spawn()
            .map_err(|source| LlmError::Spawn {
                program: self.program.display().to_string(),
                source,
            })?;
        let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(LlmError::Delegate("Claude Code gave no output".into()));
        };

        let mut parser = StreamParser::default();
        let read_events = async {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                parser.feed(&line, on_event);
            }
        };
        // Read the errors at the same time, so that a full pipe can never
        // block the agent.
        let read_errors = async {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        };
        let ((), errors) = tokio::join!(read_events, read_errors);
        let status = child.wait().await.ok();

        parser.finish().map_err(|reason| {
            let errors: String = errors.trim().chars().take(STDERR_LIMIT).collect();
            let detail = match (reason, errors.is_empty()) {
                (Some(reason), _) => reason,
                (None, false) => errors,
                (None, true) => match status {
                    Some(status) => format!("Claude Code stopped without a result ({status})"),
                    None => "Claude Code stopped without a result".into(),
                },
            };
            LlmError::Delegate(detail)
        })
    }
}

/// Turns Claude Code's `stream-json` lines into events, one line at a time.
#[derive(Debug, Default)]
struct StreamParser {
    /// Tool calls waiting for their result, by call id.
    pending: HashMap<String, (String, Value)>,
    /// Whether the text of the current message is arriving in pieces, in which
    /// case the complete message must not show it a second time.
    streamed: bool,
    reply: Option<DelegateReply>,
    error: Option<String>,
    /// Tokens sent on the latest call to the model: how full the context is.
    last_input: Option<u64>,
}

impl StreamParser {
    fn feed(&mut self, line: &str, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event["type"].as_str() {
            Some("stream_event") => self.stream_event(&event["event"], on_event),
            Some("assistant") => {
                for block in blocks(&event["message"]["content"]) {
                    match block["type"].as_str() {
                        Some("text") if !self.streamed => {
                            on_event(DelegateEvent::TextStart);
                            on_event(DelegateEvent::Text(text(&block["text"])));
                        }
                        Some("tool_use") => {
                            self.pending.insert(
                                text(&block["id"]),
                                (text(&block["name"]), block["input"].clone()),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for block in blocks(&event["message"]["content"]) {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    let Some((name, input)) = self.pending.remove(&text(&block["tool_use_id"]))
                    else {
                        continue;
                    };
                    let content = tool_output(&block["content"]);
                    let output = if block["is_error"].as_bool() == Some(true) {
                        Err(content)
                    } else {
                        Ok(content)
                    };
                    on_event(DelegateEvent::Tool {
                        name,
                        input,
                        output,
                    });
                }
            }
            Some("result") => self.result(&event),
            _ => {}
        }
    }

    fn stream_event(&mut self, event: &Value, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        match event["type"].as_str() {
            Some("message_start") => {
                self.streamed = false;
                let usage = &event["message"]["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                let input = count("input_tokens")
                    + count("cache_read_input_tokens")
                    + count("cache_creation_input_tokens");
                if input > 0 {
                    self.last_input = Some(input);
                }
            }
            Some("content_block_start") if event["content_block"]["type"] == "text" => {
                self.streamed = true;
                on_event(DelegateEvent::TextStart);
            }
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                on_event(DelegateEvent::Text(text(&event["delta"]["text"])));
            }
            _ => {}
        }
    }

    fn result(&mut self, event: &Value) {
        let usage = &event["usage"];
        let count = |key: &str| usage[key].as_u64().unwrap_or(0);
        let reply = DelegateReply {
            text: text(&event["result"]),
            session: text(&event["session_id"]),
            usage: Usage {
                // Claude Code reads most of its context from the cache; it
                // is context all the same, so it counts as input.
                input: TokenCount(
                    count("input_tokens")
                        + count("cache_read_input_tokens")
                        + count("cache_creation_input_tokens"),
                ),
                output: TokenCount(count("output_tokens")),
            },
            estimate: event["total_cost_usd"]
                .as_f64()
                .filter(|c| c.is_finite() && *c >= 0.0)
                .map(Usd),
            context: None,
        };
        // The window of the model used, from the per-model breakdown.
        let window = event["modelUsage"]
            .as_object()
            .and_then(|models| models.values().find_map(|m| m["contextWindow"].as_u64()));
        let reply = DelegateReply {
            context: window.map(|window| ContextUse {
                used: TokenCount(self.last_input.unwrap_or(reply.usage.input.0)),
                window: TokenCount(window),
            }),
            ..reply
        };
        if event["is_error"].as_bool() == Some(true) {
            let reason = if reply.text.is_empty() {
                text(&event["subtype"])
            } else {
                reply.text.clone()
            };
            self.error = Some(reason);
        }
        self.reply = Some(reply);
    }

    /// The reply, or why there is none: `Some` reason when Claude Code said
    /// what went wrong, `None` when it said nothing at all.
    fn finish(self) -> Result<DelegateReply, Option<String>> {
        match (self.reply, self.error) {
            (Some(reply), None) => Ok(reply),
            (_, Some(error)) => Err(Some(error)),
            (None, None) => Err(None),
        }
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn blocks(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

/// A tool result's content: a string, or a list of text blocks.
fn tool_output(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> (Vec<DelegateEvent>, Result<DelegateReply, Option<String>>) {
        let mut parser = StreamParser::default();
        let mut events = Vec::new();
        for line in lines {
            parser.feed(line, &mut |e| events.push(e));
        }
        (events, parser.finish())
    }

    // The shape Claude Code 2.1 prints, cut to the fields that matter.
    const READ_THEN_ANSWER: [&str; 10] = [
        r#"{"type":"system","subtype":"init","model":"claude-opus-5-5"}"#,
        r#"{"type":"stream_event","event":{"type":"message_start"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/p/hello.py"}}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"1\tprint(1)\n"}]}}"#,
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":18000}}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"It prints "}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"1."}}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"It prints 1."}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"It prints 1.","session_id":"s1","total_cost_usd":0.09,"usage":{"input_tokens":4,"output_tokens":171,"cache_read_input_tokens":26705,"cache_creation_input_tokens":10153},"modelUsage":{"claude-opus-5-5":{"contextWindow":1000000}}}"#,
    ];

    #[test]
    fn tools_and_streamed_text_come_out_once_each() {
        let (events, reply) = parse(&READ_THEN_ANSWER);
        assert_eq!(
            events,
            [
                DelegateEvent::Tool {
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "/p/hello.py"}),
                    output: Ok("1\tprint(1)\n".into()),
                },
                DelegateEvent::TextStart,
                DelegateEvent::Text("It prints ".into()),
                DelegateEvent::Text("1.".into()),
            ]
        );
        let reply = reply.unwrap();
        assert_eq!(reply.session, "s1");
        assert_eq!(reply.usage.input, TokenCount(4 + 26_705 + 10_153));
        assert_eq!(reply.estimate, Some(Usd(0.09)));
        // How full the context was on the last call, not the run's total.
        assert_eq!(
            reply.context,
            Some(ContextUse {
                used: TokenCount(18_002),
                window: TokenCount(1_000_000),
            })
        );
    }

    #[test]
    fn text_that_was_not_streamed_still_shows() {
        let (events, _) = parse(&[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
        ]);
        assert_eq!(
            events,
            [
                DelegateEvent::TextStart,
                DelegateEvent::Text("Done.".into())
            ]
        );
    }

    #[test]
    fn a_failed_tool_and_a_failed_run_are_errors() {
        let (events, reply) = parse(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Edit","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true,"content":[{"type":"text","text":"old_string not found"}]}]}}"#,
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"","session_id":"s"}"#,
        ]);
        assert!(
            matches!(&events[0], DelegateEvent::Tool { output: Err(e), .. } if e == "old_string not found")
        );
        assert_eq!(reply, Err(Some("error_max_turns".into())));
    }

    #[test]
    fn no_result_at_all_says_so() {
        assert_eq!(parse(&["not json", ""]).1, Err(None));
    }

    #[test]
    fn the_command_line_hands_over_the_task_and_forbids_commands() {
        let request = DelegateRequest {
            agent: ironquill_core::Agent::ClaudeCode,
            model: "opus".into(),
            prompt: "fix it".into(),
            instructions: "be brief".into(),
            resume: Some("s1".into()),
            directory: PathBuf::from("/tmp"),
        };
        let command = ClaudeCode::new("claude").command(&request);
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(after("-p").as_deref(), Some("fix it"));
        assert_eq!(after("--disallowedTools").as_deref(), Some("Bash"));
        assert_eq!(after("--model").as_deref(), Some("opus"));
        assert_eq!(after("--resume").as_deref(), Some("s1"));
        assert_eq!(after("--output-format").as_deref(), Some("stream-json"));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }
}
