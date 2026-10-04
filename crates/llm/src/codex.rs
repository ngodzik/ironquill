use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use ironquill_core::{Delegate, DelegateEvent, DelegateReply, DelegateRequest, TokenCount, Usage};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::error::LlmError;

/// How much of Codex's error output is kept for the message.
const STDERR_LIMIT: usize = 4_000;

/// The Codex command line, run as an agent tasks are handed to.
///
/// It runs the `codex` program installed on the machine, as published, with
/// the person's own login: ironquill never reads or stores credentials.
/// Codex reads and edits files by running commands, so it runs them in its
/// own sandbox: it may write inside the project only, without network, and
/// is never asked for an approval, since nobody is there to give it.
#[derive(Debug, Clone)]
pub struct Codex {
    program: PathBuf,
}

impl Codex {
    /// Codex at `program`.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Codex as found on the `PATH`, if it is installed.
    pub fn find() -> Option<Self> {
        crate::claude_code::find_program("codex").map(Self::new)
    }

    /// The models Codex offers to pick from, as it lists them; empty when
    /// it cannot say.
    pub fn models(&self) -> Vec<String> {
        let Ok(output) = std::process::Command::new(&self.program)
            .args(["debug", "models"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        listed_models(&String::from_utf8_lossy(&output.stdout))
    }

    fn command(&self, request: &DelegateRequest) -> Command {
        let mut command = Command::new(&self.program);
        command.arg("exec");
        if let Some(session) = &request.resume {
            command.arg("resume");
            command.arg(session);
        }
        command
            .arg("--json")
            .args(["-c", "sandbox_mode=\"workspace-write\""])
            .args(["-c", "sandbox_workspace_write.network_access=false"])
            .args(["-c", "approval_policy=\"never\""])
            // No MCP server: the person's own connectors (mail, calendars)
            // have nothing to do with a task in this project.
            .args(["-c", "mcp_servers={}"])
            .arg("-c")
            .arg(format!(
                "developer_instructions={}",
                toml_string(&request.instructions)
            ))
            .arg("--skip-git-repo-check")
            .current_dir(&request.directory)
            // Codex reads the prompt from piped input otherwise.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A stopped request stops the agent too.
            .kill_on_drop(true);
        if !request.model.is_empty() {
            command.args(["-m", &request.model]);
        }
        if let Some(effort) = request.effort {
            command
                .arg("-c")
                .arg(format!("model_reasoning_effort=\"{effort}\""));
        }
        // Last, so that a prompt starting with `-` is not read as a flag.
        command.arg("--").arg(&request.prompt);
        command
    }
}

impl Delegate for Codex {
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
            return Err(LlmError::Delegate("Codex gave no output".into()));
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
                    Some(status) => format!("Codex stopped without a result ({status})"),
                    None => "Codex stopped without a result".into(),
                },
            };
            LlmError::Delegate(detail)
        })
    }
}

/// Turns the lines of `codex exec --json` into events, one line at a time.
#[derive(Debug, Default)]
struct StreamParser {
    session: Option<String>,
    /// The latest message: the report, once the turn is over.
    text: String,
    usage: Usage,
    completed: bool,
    error: Option<String>,
    /// What files held before a change began, to show what it changed.
    before: HashMap<String, String>,
}

impl StreamParser {
    fn feed(&mut self, line: &str, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event["type"].as_str() {
            Some("thread.started") => self.session = Some(text(&event["thread_id"])),
            Some("item.started") if event["item"]["type"] == "file_change" => {
                for path in changed_paths(&event["item"]) {
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    self.before.insert(path, content);
                }
            }
            Some("item.completed") => self.item(&event["item"], on_event),
            Some("turn.completed") => {
                let usage = &event["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                // Cached input is part of the input, as OpenAI counts it.
                self.usage += Usage {
                    input: TokenCount(count("input_tokens")),
                    output: TokenCount(count("output_tokens")),
                };
                self.completed = true;
            }
            Some("turn.failed") => self.error = Some(message(&text(&event["error"]["message"]))),
            _ => {}
        }
    }

    fn item(&mut self, item: &Value, on_event: &mut (dyn FnMut(DelegateEvent) + Send)) {
        match item["type"].as_str() {
            Some("agent_message") => {
                self.text = text(&item["text"]);
                on_event(DelegateEvent::TextStart);
                on_event(DelegateEvent::Text(self.text.clone()));
            }
            Some("command_execution") => {
                let output = text(&item["aggregated_output"]);
                on_event(DelegateEvent::Tool {
                    name: "Bash".into(),
                    input: json!({"command": unwrap_shell(&text(&item["command"]))}),
                    output: match item["exit_code"].as_i64() {
                        Some(0) => Ok(output),
                        _ => Err(output),
                    },
                });
            }
            Some("file_change") => {
                let failed = item["status"] == "failed";
                for change in item["changes"].as_array().into_iter().flatten() {
                    let path = text(&change["path"]);
                    let before = self.before.remove(&path).unwrap_or_default();
                    let after = std::fs::read_to_string(&path).unwrap_or_default();
                    let (name, input) = match change["kind"].as_str() {
                        Some("add") => ("Write", json!({"file_path": path, "content": after})),
                        Some("delete") => ("Delete", json!({"file_path": path})),
                        _ => (
                            "Edit",
                            json!({"file_path": path, "old_string": before, "new_string": after}),
                        ),
                    };
                    let output = if failed {
                        Err("the change failed".to_owned())
                    } else if name == "Write" {
                        Ok("File created".to_owned())
                    } else {
                        Ok(String::new())
                    };
                    on_event(DelegateEvent::Tool {
                        name: name.into(),
                        input,
                        output,
                    });
                }
            }
            _ => {}
        }
    }

    /// The reply, or why there is none: `Some` reason when Codex said what
    /// went wrong, `None` when it said nothing at all.
    fn finish(self) -> Result<DelegateReply, Option<String>> {
        if let Some(error) = self.error {
            return Err(Some(error));
        }
        match (self.session, self.completed) {
            (Some(session), true) => Ok(DelegateReply {
                text: self.text,
                session,
                usage: self.usage,
                estimate: None,
                context: None,
            }),
            _ => Err(None),
        }
    }
}

/// The models a `codex debug models` catalog lists for people to pick.
fn listed_models(catalog: &str) -> Vec<String> {
    let Ok(catalog) = serde_json::from_str::<Value>(catalog) else {
        return Vec::new();
    };
    catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| m["slug"].as_str().map(str::to_owned))
        .collect()
}

fn changed_paths(item: &Value) -> impl Iterator<Item = String> {
    item["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|change| text(&change["path"]))
}

/// The command a shell was asked to run: Codex reports
/// `/usr/bin/zsh -lc "cat a.py"` for `cat a.py`.
fn unwrap_shell(command: &str) -> String {
    let inner = command
        .split_once(" -lc ")
        .or_else(|| command.split_once(" -c "))
        .filter(|(shell, _)| Path::new(shell).is_absolute() || !shell.contains(' '))
        .map_or(command, |(_, inner)| inner);
    match inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(quoted) => quoted.replace("\\\"", "\""),
        None => inner.to_owned(),
    }
}

/// The readable part of an error Codex passes on from its API, which comes
/// as JSON inside the message.
fn message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| raw.to_owned())
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// `text` as a TOML string, for a `-c key=value` override.
fn toml_string(text: &str) -> String {
    // A JSON string is a valid TOML basic string.
    Value::String(text.to_owned()).to_string()
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

    #[test]
    fn a_run_becomes_events_and_a_reply() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("calc.py");
        let path = file.to_string_lossy().into_owned();
        std::fs::write(&file, "return a - b\n").unwrap();
        let change = format!(
            r#"{{"id":"item_3","type":"file_change","changes":[{{"path":{path:?},"kind":"update"}}],"status":"STATUS"}}"#
        );
        let started = format!(
            r#"{{"type":"item.started","item":{}}}"#,
            change.replace("STATUS", "in_progress")
        );
        let completed = format!(
            r#"{{"type":"item.completed","item":{}}}"#,
            change.replace("STATUS", "completed")
        );

        let mut parser = StreamParser::default();
        let mut events = Vec::new();
        let mut feed = |parser: &mut StreamParser, line: &str| {
            parser.feed(line, &mut |e| events.push(e));
        };
        // The shape Codex 0.160 prints, cut to the fields that matter.
        feed(&mut parser, r#"{"type":"thread.started","thread_id":"t1"}"#);
        feed(&mut parser, r#"{"type":"turn.started"}"#);
        feed(
            &mut parser,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/usr/bin/zsh -lc \"sed -n '1,240p' calc.py\"","aggregated_output":"return a - b\n","exit_code":0,"status":"completed"}}"#,
        );
        feed(&mut parser, &started);
        std::fs::write(&file, "return a + b\n").unwrap();
        feed(&mut parser, &completed);
        feed(
            &mut parser,
            r#"{"type":"item.completed","item":{"id":"item_4","type":"agent_message","text":"Fixed add."}}"#,
        );
        feed(
            &mut parser,
            r#"{"type":"turn.completed","usage":{"input_tokens":55773,"cached_input_tokens":50176,"output_tokens":276}}"#,
        );

        assert_eq!(
            events,
            [
                DelegateEvent::Tool {
                    name: "Bash".into(),
                    input: json!({"command": "sed -n '1,240p' calc.py"}),
                    output: Ok("return a - b\n".into()),
                },
                DelegateEvent::Tool {
                    name: "Edit".into(),
                    input: json!({
                        "file_path": path,
                        "old_string": "return a - b\n",
                        "new_string": "return a + b\n",
                    }),
                    output: Ok(String::new()),
                },
                DelegateEvent::TextStart,
                DelegateEvent::Text("Fixed add.".into()),
            ]
        );
        let reply = parser.finish().unwrap();
        assert_eq!(reply.text, "Fixed add.");
        assert_eq!(reply.session, "t1");
        assert_eq!(reply.usage.input, TokenCount(55_773));
        assert_eq!(reply.usage.output, TokenCount(276));
    }

    #[test]
    fn a_failed_command_and_a_failed_turn_are_errors() {
        let (events, reply) = parse(&[
            r#"{"type":"thread.started","thread_id":"t"}"#,
            r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls nope","aggregated_output":"no such file","exit_code":2}}"#,
            r#"{"type":"turn.failed","error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"The 'x' model is not supported.\"}}"}}"#,
        ]);
        assert!(
            matches!(&events[0], DelegateEvent::Tool { output: Err(e), .. } if e == "no such file")
        );
        assert_eq!(reply, Err(Some("The 'x' model is not supported.".into())));
    }

    #[test]
    fn no_result_at_all_says_so() {
        assert_eq!(parse(&["not json", ""]).1, Err(None));
    }

    #[test]
    fn only_listed_models_are_offered() {
        let catalog = r#"{"models":[{"slug":"gpt-a","visibility":"list"},{"slug":"gpt-hidden","visibility":"hide"}]}"#;
        assert_eq!(listed_models(catalog), ["gpt-a"]);
        assert!(listed_models("oops").is_empty());
    }

    #[test]
    fn the_command_line_sandboxes_codex_and_resumes_its_session() {
        let request = DelegateRequest {
            agent: ironquill_core::Agent::Codex,
            effort: Some(ironquill_core::Effort::Low),
            model: "gpt-5.5".into(),
            prompt: "-fix it".into(),
            instructions: "be \"brief\"".into(),
            resume: Some("s1".into()),
            directory: PathBuf::from("/tmp"),
        };
        let command = Codex::new("codex").command(&request);
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[..3], ["exec", "resume", "s1"]);
        assert_eq!(args[args.len() - 2..], ["--", "-fix it"]);
        for setting in [
            "sandbox_mode=\"workspace-write\"",
            "sandbox_workspace_write.network_access=false",
            "approval_policy=\"never\"",
            "mcp_servers={}",
            "developer_instructions=\"be \\\"brief\\\"\"",
            "model_reasoning_effort=\"low\"",
        ] {
            assert!(args.iter().any(|a| a == setting), "{setting} missing");
        }
        let model = args.iter().position(|a| a == "-m").unwrap();
        assert_eq!(args[model + 1], "gpt-5.5");
    }

    #[test]
    fn shells_are_unwrapped() {
        assert_eq!(unwrap_shell("/usr/bin/zsh -lc \"rg foo\""), "rg foo");
        assert_eq!(unwrap_shell("bash -lc 'ls'"), "'ls'");
        assert_eq!(unwrap_shell("ls -la"), "ls -la");
    }
}
