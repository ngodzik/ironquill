use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use ironquill_core::{
    ContextUse, Delegate, DelegateEvent, DelegateReply, DelegateRequest, TokenCount, Usage, Usd,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::error::LlmError;
use crate::prices::{PriceTable, Tokens};

/// Tools refused when Claude Code may only read: it plans or reviews,
/// another model writes.
const READ_ONLY_DISALLOWED: &str = "Bash Edit MultiEdit Write NotebookEdit";

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
    /// List prices, to say what each message costs as it comes.
    prices: Option<Arc<PriceTable>>,
}

impl ClaudeCode {
    /// Claude Code at `program`.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prices: None,
        }
    }

    /// Prices each message as it comes with `prices`, as ccusage does
    /// after the fact.
    #[must_use]
    pub fn with_prices(mut self, prices: Option<Arc<PriceTable>>) -> Self {
        self.prices = prices;
        self
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
            // Its own classifier lets safe actions run and refuses what
            // cannot be undone or leaves the machine; nobody is there to
            // answer a prompt, so what would ask is refused too, and shown,
            // for the person to approve in their next message.
            .args(["--permission-mode", "auto"])
            .args(["--permission-prompts", "none"])
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
        if request.read_only {
            command.args(["--disallowedTools", READ_ONLY_DISALLOWED]);
        }
        if !request.model.is_empty() {
            command.args(["--model", &request.model]);
        }
        if let Some(effort) = request.effort {
            command.args(["--effort", effort.as_str()]);
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

        let mut parser = StreamParser {
            prices: self.prices.clone(),
            ..StreamParser::default()
        };
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
    /// The calls refused so far, by call id, so that each is told once.
    denied: HashSet<String>,
    /// Whether the text of the current message is arriving in pieces, in which
    /// case the complete message must not show it a second time.
    streamed: bool,
    reply: Option<DelegateReply>,
    error: Option<String>,
    /// Tokens sent on the latest call to the model: how full the context is.
    last_input: Option<u64>,
    /// List prices, to price each message.
    prices: Option<Arc<PriceTable>>,
    /// Claude Code runs with an API key rather than a subscription.
    billed: bool,
    /// The model of the message being written, and its input so far.
    model: String,
    tokens: Tokens,
}

impl StreamParser {
    fn feed(&mut self, line: &str, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event["type"].as_str() {
            // How it is paid for: with a key, every token is owed.
            Some("system") if event["subtype"] == "init" => {
                self.billed = event["apiKeySource"]
                    .as_str()
                    .is_some_and(|source| source != "none");
            }
            Some("system") if event["subtype"] == "permission_denied" => {
                self.deny(&event, on_event);
            }
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
            Some("result") => {
                // Refusals not told as they happened.
                for denial in event["permission_denials"].as_array().into_iter().flatten() {
                    self.deny(denial, on_event);
                }
                self.result(&event);
            }
            _ => {}
        }
    }

    fn stream_event(&mut self, event: &Value, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        match event["type"].as_str() {
            Some("message_start") => {
                self.streamed = false;
                let usage = &event["message"]["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                self.model = text(&event["message"]["model"]);
                self.tokens = Tokens {
                    input: count("input_tokens"),
                    cache_read: count("cache_read_input_tokens"),
                    cache_write: count("cache_creation_input_tokens"),
                    output: count("output_tokens"),
                };
                let input = self.tokens.input + self.tokens.cache_read + self.tokens.cache_write;
                if input > 0 {
                    self.last_input = Some(input);
                }
            }
            // The end of a message: what it used, and what that costs.
            Some("message_delta") => {
                if let Some(output) = event["usage"]["output_tokens"].as_u64() {
                    self.tokens.output = output;
                }
                let tokens = std::mem::take(&mut self.tokens);
                let usage = Usage {
                    input: TokenCount(tokens.input + tokens.cache_read + tokens.cache_write),
                    output: TokenCount(tokens.output),
                };
                if usage.input.0 + usage.output.0 > 0 {
                    let cost = self
                        .prices
                        .as_ref()
                        .and_then(|prices| prices.cost(&self.model, tokens));
                    on_event(DelegateEvent::Usage {
                        usage,
                        cost,
                        billed: self.billed,
                    });
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

    /// Tells a refused call once, whichever way Claude Code reported it.
    fn deny(&mut self, denial: &Value, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        let id = text(&denial["tool_use_id"]);
        if !id.is_empty() && !self.denied.insert(id.clone()) {
            return;
        }
        // The call it refused, when its arguments came with the message.
        let pending = self.pending.get(&id).cloned();
        let name = [&denial["tool_name"], &denial["tool"]]
            .into_iter()
            .find_map(Value::as_str)
            .map(str::to_owned)
            .or_else(|| pending.as_ref().map(|(name, _)| name.clone()))
            .unwrap_or_default();
        let input = [&denial["tool_input"], &denial["input"]]
            .into_iter()
            .find(|v| !v.is_null())
            .cloned()
            .or_else(|| pending.map(|(_, input)| input))
            .unwrap_or(Value::Null);
        let reason = ["reason", "message", "decision_reason", "description"]
            .into_iter()
            .find_map(|key| denial[key].as_str())
            .unwrap_or_default()
            .to_owned();
        on_event(DelegateEvent::Denied {
            name,
            input,
            reason,
        });
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
            billed: self.billed,
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
    fn each_message_is_counted_as_it_ends_and_priced_when_billed() {
        let prices = PriceTable::parse(
            r#"{"claude-opus-5-5": {"input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5,
                                   "cache_read_input_token_cost": 2e-7,
                                   "cache_creation_input_token_cost": 5e-6}}"#,
        )
        .unwrap();
        let mut parser = StreamParser {
            prices: Some(Arc::new(prices)),
            ..StreamParser::default()
        };
        let mut events = Vec::new();
        for line in [
            r#"{"type":"system","subtype":"init","apiKeySource":"ANTHROPIC_API_KEY"}"#,
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":1000,"cache_read_input_tokens":100000,"cache_creation_input_tokens":2000,"output_tokens":1}}}}"#,
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":500}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"s","total_cost_usd":0.05,"usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":100000,"cache_creation_input_tokens":2000}}"#,
        ] {
            parser.feed(line, &mut |e| events.push(e));
        }
        let [
            DelegateEvent::Usage {
                usage,
                cost,
                billed,
            },
        ] = events.as_slice()
        else {
            panic!("one message, one usage: {events:?}");
        };
        assert_eq!(usage.input, TokenCount(103_000));
        assert_eq!(usage.output, TokenCount(500));
        assert!((cost.unwrap().0 - 0.044).abs() < 1e-12);
        assert!(billed);
        assert!(parser.finish().unwrap().billed);
    }

    #[test]
    fn a_subscription_is_not_billed() {
        let mut parser = StreamParser::default();
        parser.feed(
            r#"{"type":"system","subtype":"init","apiKeySource":"none"}"#,
            &mut |_| {},
        );
        assert!(!parser.billed);
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
    fn refused_calls_are_told_once_whichever_way_they_come() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git push origin main"}}]}}"#,
            r#"{"type":"system","subtype":"permission_denied","tool_use_id":"t1","tool_name":"Bash","reason":"Pushing needs the person's approval."}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"rm -rf build"}}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"s","permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"git push origin main"}},{"tool_name":"Bash","tool_use_id":"t2","tool_input":{"command":"rm -rf build"}}]}"#,
        ];
        let mut parser = StreamParser::default();
        let mut denied = Vec::new();
        for line in lines {
            parser.feed(line, &mut |e| {
                if let DelegateEvent::Denied { input, reason, .. } = e {
                    denied.push((input["command"].as_str().unwrap().to_owned(), reason));
                }
            });
        }
        assert_eq!(
            denied,
            [
                (
                    "git push origin main".to_owned(),
                    "Pushing needs the person's approval.".to_owned()
                ),
                ("rm -rf build".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn the_command_line_hands_over_the_task_with_its_own_safety_checks() {
        let request = DelegateRequest {
            agent: ironquill_core::Agent::ClaudeCode,
            effort: Some(ironquill_core::Effort::Max),
            model: "opus".into(),
            prompt: "fix it".into(),
            instructions: "be brief".into(),
            resume: Some("s1".into()),
            directory: PathBuf::from("/tmp"),
            read_only: false,
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
        // Its auto mode judges each action; nobody answers prompts.
        assert_eq!(after("--permission-mode").as_deref(), Some("auto"));
        assert_eq!(after("--permission-prompts").as_deref(), Some("none"));
        assert_eq!(after("--disallowedTools"), None);
        assert_eq!(after("--model").as_deref(), Some("opus"));
        assert_eq!(after("--effort").as_deref(), Some("max"));
        assert_eq!(after("--resume").as_deref(), Some("s1"));
        assert_eq!(after("--output-format").as_deref(), Some("stream-json"));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));

        // Planning or reviewing, it may not change a file either.
        let read_only = DelegateRequest {
            read_only: true,
            ..request
        };
        let command = ClaudeCode::new("claude").command(&read_only);
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[at + 1], "Bash Edit MultiEdit Write NotebookEdit");
    }
}
