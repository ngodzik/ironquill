use ironquill_core::{ChatModel, ChatRequest, Message, ModelId, Usage, Usd};
use ironquill_tools::{Check, CheckFailure, CheckReport, Toolbox};

use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::event::Event;

const SYSTEM_PROMPT: &str = "You are a careful software engineer working in a project through tools. \
Make the smallest change that completes the task. Read a file before editing it. \
Edit existing files with `replace`, not `write_file`. \
You cannot run commands: when you stop calling tools, the project's checks run automatically \
and you will be shown any failure. Do not ask questions; when you are done, reply with one \
short sentence saying what you changed.";

/// How a session ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The checks passed after this model's work.
    Passed {
        /// The model whose change passed.
        model: ModelId,
    },
    /// Every model spent its rounds and the checks still fail.
    GaveUp {
        /// The last failure, as the checks reported it.
        failure: Option<CheckFailure>,
    },
}

/// What a session produced and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Whether the change passed.
    pub verdict: Verdict,
    /// Tokens over the whole session.
    pub usage: Usage,
    /// Cost over the whole session, counting only the turns whose cost the
    /// provider reported.
    pub cost: Usd,
    /// Whether every turn's cost was reported, so that `cost` is the full bill.
    pub cost_complete: bool,
    /// Files written or edited, relative to the workspace root.
    pub changed: Vec<String>,
}

struct Ledger {
    usage: Usage,
    cost: Usd,
    cost_complete: bool,
}

/// Runs one task to a verdict.
///
/// `context` is handed to the model with the task: anything ironquill can
/// work out for free, such as the list of tracked files, so that the model
/// does not spend turns discovering it.
///
/// # Errors
///
/// [`AgentError::Model`] when the provider fails, [`AgentError::Check`] when a
/// check cannot be started. Failing checks are not an error, see [`Verdict`].
pub async fn run<M: ChatModel>(
    model: &M,
    toolbox: &mut Toolbox,
    config: &AgentConfig,
    task: &str,
    context: &str,
    mut observe: impl FnMut(Event),
) -> Result<Outcome, AgentError> {
    let mut ledger = Ledger {
        usage: Usage::default(),
        cost: Usd::default(),
        cost_complete: true,
    };
    let mut failure: Option<CheckFailure> = None;
    let mut previous: Option<&ModelId> = None;

    for tier in &config.tiers {
        if let Some(from) = previous {
            observe(Event::Escalating {
                from: from.clone(),
                to: tier.clone(),
            });
        }
        previous = Some(tier);

        let mut messages = vec![
            Message::system(SYSTEM_PROMPT),
            Message::user(brief(task, context, failure.as_ref(), toolbox)),
        ];

        for round in 0..config.rounds_per_tier {
            if round > 0
                && let Some(f) = &failure
            {
                messages.push(Message::user(format!(
                    "The checks failed.\n\n{}",
                    describe(f)
                )));
            }

            let finished = converse(
                model,
                tier,
                toolbox,
                &mut messages,
                config.max_turns,
                &mut ledger,
                &mut observe,
            )
            .await?;

            observe(Event::Checking);
            match Check::run_all(&config.checks, toolbox.workspace().root())
                .await
                .map_err(AgentError::Check)?
            {
                CheckReport::Passed => {
                    observe(Event::Passed);
                    return Ok(outcome(
                        Verdict::Passed {
                            model: tier.clone(),
                        },
                        ledger,
                        toolbox,
                    ));
                }
                CheckReport::Failed(f) => {
                    observe(Event::Failed {
                        command: f.command.clone(),
                    });
                    failure = Some(f);
                }
            }

            // A model that ran out of turns is going in circles; another round
            // of the same would most likely cost the same for the same result.
            if !finished {
                break;
            }
        }
    }

    Ok(outcome(Verdict::GaveUp { failure }, ledger, toolbox))
}

/// Lets the model call tools until it stops or runs out of turns. Returns
/// whether it stopped on its own.
async fn converse<M: ChatModel>(
    model: &M,
    model_id: &ModelId,
    toolbox: &mut Toolbox,
    messages: &mut Vec<Message>,
    max_turns: u32,
    ledger: &mut Ledger,
    observe: &mut impl FnMut(Event),
) -> Result<bool, AgentError> {
    let tools = toolbox.specs();
    for _ in 0..max_turns {
        let request = ChatRequest {
            model: model_id.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
        };
        let response = model
            .complete(&request)
            .await
            .map_err(|e| AgentError::Model(Box::new(e)))?;

        ledger.usage += response.usage;
        match response.cost {
            Some(cost) => ledger.cost += cost,
            None => ledger.cost_complete = false,
        }
        observe(Event::Turn {
            model: model_id.clone(),
            usage: response.usage,
            cost: response.cost,
        });

        messages.push(response.to_message());
        if response.tool_calls.is_empty() {
            return Ok(true);
        }
        for call in &response.tool_calls {
            let result = toolbox.call(call);
            observe(Event::Tool {
                name: call.name.clone(),
                path: path_argument(&call.arguments),
                error: result.as_ref().err().map(ToString::to_string),
            });
            messages.push(Message::Tool {
                call_id: call.id.clone(),
                content: result.unwrap_or_else(|e| format!("error: {e}")),
            });
        }
    }
    Ok(false)
}

/// The opening message of a tier: the task, the free context, and, when a
/// weaker model went first, where it left things.
fn brief(task: &str, context: &str, failure: Option<&CheckFailure>, toolbox: &Toolbox) -> String {
    let mut text = format!("Task: {task}\n\n{context}");
    if let Some(f) = failure {
        let changed: Vec<&str> = toolbox.changed().collect();
        text.push_str(&format!(
            "\n\nA previous attempt already edited: {}. Its changes are on disk. \
             The checks fail.\n\n{}",
            if changed.is_empty() {
                "nothing".to_owned()
            } else {
                changed.join(", ")
            },
            describe(f)
        ));
    }
    text
}

fn describe(failure: &CheckFailure) -> String {
    format!(
        "`{}` failed:\n```\n{}\n```",
        failure.command, failure.excerpt
    )
}

fn path_argument(arguments: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()?
        .get("path")?
        .as_str()
        .map(str::to_owned)
}

fn outcome(verdict: Verdict, ledger: Ledger, toolbox: &Toolbox) -> Outcome {
    Outcome {
        verdict,
        usage: ledger.usage,
        cost: ledger.cost,
        cost_complete: ledger.cost_complete,
        changed: toolbox.changed().map(str::to_owned).collect(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::Mutex;

    use ironquill_core::{ChatResponse, TokenCount, ToolCall};
    use ironquill_tools::Workspace;
    use serde_json::json;

    use super::*;

    /// A model that plays back a script and remembers what it was sent.
    struct Scripted {
        answers: Mutex<VecDeque<ChatResponse>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl Scripted {
        fn new(answers: Vec<ChatResponse>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatModel for Scripted {
        type Error = Infallible;

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, Infallible> {
            self.seen.lock().unwrap().push(request.clone());
            Ok(self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("the script ran out"))
        }
    }

    fn says(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.into()),
            tool_calls: vec![],
            usage: Usage {
                input: TokenCount(100),
                output: TokenCount(10),
            },
            cost: Some(Usd(0.001)),
        }
    }

    fn calls(name: &str, arguments: serde_json::Value) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: name.into(),
                arguments: arguments.to_string(),
            }],
            ..says("")
        }
    }

    fn setup() -> (tempfile::TempDir, Toolbox) {
        let dir = tempfile::tempdir().unwrap();
        let toolbox = Toolbox::new(Workspace::new(dir.path()).unwrap());
        (dir, toolbox)
    }

    fn config() -> AgentConfig {
        AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .tier(ModelId::new("strong").unwrap())
            .check(Check::parse("test -f done.txt").unwrap())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn passes_on_the_first_model_when_it_does_the_job() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
        ]);

        let outcome = run(
            &model,
            &mut toolbox,
            &config(),
            "create done.txt",
            "",
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.verdict,
            Verdict::Passed {
                model: ModelId::new("cheap").unwrap()
            }
        );
        assert_eq!(outcome.changed, ["done.txt"]);
        assert_eq!(outcome.usage.input, TokenCount(200));
        assert!(outcome.cost_complete);
    }

    #[tokio::test]
    async fn escalates_with_a_brief_not_the_history() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            // The cheap model claims success twice without doing anything.
            says("Done."),
            says("Done, really."),
            // The strong model does the work.
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
        ]);
        let mut events = Vec::new();

        let outcome = run(
            &model,
            &mut toolbox,
            &config(),
            "create done.txt",
            "",
            |e| events.push(e),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.verdict,
            Verdict::Passed {
                model: ModelId::new("strong").unwrap()
            }
        );
        assert!(events.iter().any(|e| matches!(e, Event::Escalating { .. })));

        let seen = model.seen.lock().unwrap();
        let first_strong = seen.iter().find(|r| r.model.as_str() == "strong").unwrap();
        assert_eq!(first_strong.messages.len(), 2);
        let Message::User(brief) = &first_strong.messages[1] else {
            panic!("the brief should be a user message");
        };
        assert!(brief.contains("`test -f done.txt` failed"));
    }

    #[tokio::test]
    async fn a_failed_tool_call_is_reported_to_the_model() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls("read_file", json!({"path": "../outside"})),
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Done."),
        ]);

        run(&model, &mut toolbox, &config(), "t", "", |_| {})
            .await
            .unwrap();

        let seen = model.seen.lock().unwrap();
        let Some(Message::Tool { content, .. }) = seen[1].messages.last() else {
            panic!("the second request should end with the tool result");
        };
        assert!(content.starts_with("error:") && content.contains("outside the workspace"));
    }

    #[tokio::test]
    async fn gives_up_when_no_model_passes() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("a"), says("b"), says("c"), says("d")]);

        let outcome = run(&model, &mut toolbox, &config(), "t", "", |_| {})
            .await
            .unwrap();

        assert!(matches!(
            outcome.verdict,
            Verdict::GaveUp { failure: Some(_) }
        ));
        assert_eq!(model.seen.lock().unwrap().len(), 4);
    }
}
