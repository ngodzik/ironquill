use ironquill_core::{
    ChatModel, ChatRequest, ContextUse, Delegate, DelegateEvent, DelegateRequest, Message, ModelId,
    TokenCount, Usage, Usd,
};
use ironquill_tools::{Check, CheckFailure, CheckReport, Toolbox};

use crate::config::AgentConfig;
use crate::delegate;
use crate::error::AgentError;
use crate::event::Event;

/// For one task with no person in the loop: `ironquill do`, and the brief a
/// stronger model gets when it takes over.
const TASK_PROMPT: &str = "You are a careful software engineer working in a project through tools. \
Make the smallest change that completes the task. Read a file before editing it. \
Edit existing files with `replace`, not `write_file`. \
You cannot run commands: when you stop calling tools, the project's checks run automatically \
and you will be shown any failure. Do not ask questions; when you are done, reply with one \
short sentence saying what you changed.";

/// For a task handed to an agent such as Claude Code. It gets the task alone,
/// not ironquill's conversation, and works in its own session.
const DELEGATE_PROMPT: &str = "This task was handed to you by ironquill, which runs the project's \
checks after you finish and sends you any failure. You cannot run shell commands; do not try to \
work around that. Make the smallest change that completes the task, reading files before editing \
them. When you are done, end with a short report: what you changed and why, and anything left to \
do. Write the report in the language the task is written in.";

/// For a conversation with a person.
const CHAT_PROMPT: &str = "You are a careful software engineer helping a person with the project \
in the current directory. You can read and edit its files through tools; you cannot run commands. \
Reply in the language the person writes in. Talk normally and answer questions directly. \
Only change files when the person asks for a change. \
If you need to ask the person something, ask it and end your reply there: do not call any tool \
in that reply, and do not act on a guess of the answer. They will reply in their next message. \
The project's files are listed below: use the list instead of listing directories, and do not \
try to read binary files. When you change files, make the smallest change that does the job, \
read a file before editing it, and edit existing files with `replace`. When you stop calling \
tools after changing files, the project's checks run automatically and you will be shown any \
failure. Be brief. You may use Markdown.";

/// How a request ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The model answered without changing any file, so there was nothing to check.
    Answered,
    /// The model changed files and no check is configured, so nothing judged them.
    Unchecked,
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

/// What a request produced and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// How it ended.
    pub verdict: Verdict,
    /// Tokens for this request.
    pub usage: Usage,
    /// Cost for this request, counting only the turns whose cost the provider
    /// reported.
    pub cost: Usd,
    /// Whether every turn's cost was reported, so that `cost` is the full bill.
    pub cost_complete: bool,
    /// Whether part of the work ran on a subscription, which `cost` leaves out.
    pub subscription: bool,
    /// How full the context was on the request's last call, when known.
    pub context: Option<ContextUse>,
    /// Files written or edited, relative to the workspace root.
    pub changed: Vec<String>,
}

struct Ledger {
    usage: Usage,
    cost: Usd,
    cost_complete: bool,
    subscription: bool,
    context: Option<ContextUse>,
}

impl Ledger {
    fn new() -> Self {
        Self {
            usage: Usage::default(),
            cost: Usd::default(),
            cost_complete: true,
            subscription: false,
            context: None,
        }
    }

    fn outcome(self, verdict: Verdict, toolbox: &Toolbox) -> Outcome {
        Outcome {
            verdict,
            usage: self.usage,
            cost: self.cost,
            cost_complete: self.cost_complete,
            subscription: self.subscription,
            context: self.context,
            changed: toolbox.changed().map(str::to_owned).collect(),
        }
    }
}

/// Everything one attempt needs besides the conversation, gathered so that
/// the functions below do not take nine arguments.
struct Ctx<'a, M, D, O> {
    model: &'a M,
    delegate: &'a D,
    config: &'a AgentConfig,
    toolbox: &'a mut Toolbox,
    ledger: Ledger,
    observe: O,
    /// The delegate session to continue, and after a delegated attempt the
    /// session it ended in.
    thread: Option<String>,
}

enum Attempt {
    Answered,
    Unchecked,
    Passed,
    Failed,
}

/// A conversation with a person: history is kept from one request to the next.
///
/// Each request is answered by the first model. If it changed files, the
/// checks run; failures go back to it, and if its rounds run out, the stronger
/// models take over from a short brief, as in [`run`]. What they did is then
/// written into the conversation, so the next request knows.
///
/// It can be saved and loaded with serde, so that a conversation can be
/// resumed later with the model seeing everything it saw before.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    messages: Vec<Message>,
    context_added: bool,
    /// The delegate's own session while consecutive requests go to the same
    /// delegate model, so that Claude Code follows the conversation from the
    /// moment it was picked. A request to any other model ends it.
    #[serde(default)]
    delegate_thread: Option<(ModelId, String)>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// An empty conversation.
    pub fn new() -> Self {
        Self {
            messages: vec![Message::system(CHAT_PROMPT)],
            context_added: false,
            delegate_thread: None,
        }
    }

    /// Sends one request and works on it until it has a verdict.
    ///
    /// `context` is added to the conversation once, on the first request.
    ///
    /// # Errors
    ///
    /// As [`run`].
    #[allow(clippy::too_many_arguments)]
    pub async fn send<M: ChatModel, D: Delegate>(
        &mut self,
        model: &M,
        delegate: &D,
        toolbox: &mut Toolbox,
        config: &AgentConfig,
        text: &str,
        context: &str,
        observe: impl FnMut(Event) + Send,
    ) -> Result<Outcome, AgentError> {
        self.settle();
        if !self.context_added && !context.is_empty() {
            if let Some(Message::System(prompt)) = self.messages.first_mut() {
                prompt.push_str("\n\n");
                prompt.push_str(context);
            }
            self.context_added = true;
        }
        toolbox.reset_changes();
        self.messages.push(Message::user(text));

        let mut ctx = Ctx {
            model,
            delegate,
            config,
            toolbox,
            ledger: Ledger::new(),
            observe,
            thread: None,
        };
        let Some(first) = config.tiers.first() else {
            return Err(AgentError::Config("at least one model is needed"));
        };
        let mut failure = None;

        // Claude Code continues its own session for as long as requests keep
        // going to it; a request to another model, or to another Claude
        // model, starts it afresh next time.
        ctx.thread = match &self.delegate_thread {
            Some((model, session)) if model == first => Some(session.clone()),
            _ => None,
        };
        let first_attempt = attempt(&mut ctx, first, &mut self.messages, &mut failure, true).await;
        self.delegate_thread = match (first.delegate(), ctx.thread.take()) {
            (Some(_), Some(session)) => Some((first.clone(), session)),
            _ => None,
        };

        match first_attempt? {
            Attempt::Answered => return Ok(ctx.ledger.outcome(Verdict::Answered, ctx.toolbox)),
            Attempt::Unchecked => {
                return Ok(ctx.ledger.outcome(Verdict::Unchecked, ctx.toolbox));
            }
            Attempt::Passed => {
                let verdict = Verdict::Passed {
                    model: first.clone(),
                };
                return Ok(ctx.ledger.outcome(verdict, ctx.toolbox));
            }
            Attempt::Failed => {}
        }

        for pair in config.tiers.windows(2) {
            let [from, to] = pair else { continue };
            (ctx.observe)(Event::Escalating {
                from: from.clone(),
                to: to.clone(),
            });
            let mut brief_messages = vec![
                Message::system(TASK_PROMPT),
                Message::user(brief(text, context, failure.as_ref(), ctx.toolbox)),
            ];
            ctx.thread = None;
            if let Attempt::Passed =
                attempt(&mut ctx, to, &mut brief_messages, &mut failure, false).await?
            {
                let changed = ctx.toolbox.changed().collect::<Vec<_>>().join(", ");
                self.note(format!(
                    "(The checks kept failing, so {to} took over and made them pass. \
                     Files changed: {changed}.)"
                ));
                let verdict = Verdict::Passed { model: to.clone() };
                return Ok(ctx.ledger.outcome(verdict, ctx.toolbox));
            }
        }

        let last = failure
            .as_ref()
            .map_or_else(String::new, |f| format!(" Last failure: `{}`.", f.command));
        self.note(format!(
            "(The checks still fail after every model tried.{last})"
        ));
        Ok(ctx.ledger.outcome(Verdict::GaveUp { failure }, ctx.toolbox))
    }

    /// Writes what happened outside the conversation into it.
    fn note(&mut self, text: String) {
        self.messages.push(Message::Assistant {
            content: Some(text),
            tool_calls: Vec::new(),
        });
    }

    /// Drops a tool request left without its results, as a stopped request
    /// leaves it. Providers refuse a conversation where a call has no answer.
    fn settle(&mut self) {
        let Some(index) = self.messages.iter().rposition(
            |m| matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        ) else {
            return;
        };
        let Message::Assistant { tool_calls, .. } = &self.messages[index] else {
            return;
        };
        let answered = self.messages[index + 1..]
            .iter()
            .filter(|m| matches!(m, Message::Tool { .. }))
            .count();
        if answered < tool_calls.len() {
            self.messages.truncate(index);
        }
    }
}

/// Runs one task to a verdict, with no person in the loop.
///
/// `context` is handed to the model with the task: anything ironquill can
/// work out for free, such as the list of tracked files, so that the model
/// does not spend turns discovering it.
///
/// # Errors
///
/// [`AgentError::Model`] when the provider fails, [`AgentError::Check`] when a
/// check cannot be started. Failing checks are not an error, see [`Verdict`].
pub async fn run<M: ChatModel, D: Delegate>(
    model: &M,
    delegate: &D,
    toolbox: &mut Toolbox,
    config: &AgentConfig,
    task: &str,
    context: &str,
    observe: impl FnMut(Event) + Send,
) -> Result<Outcome, AgentError> {
    let mut ctx = Ctx {
        model,
        delegate,
        config,
        toolbox,
        ledger: Ledger::new(),
        observe,
        thread: None,
    };
    let mut failure: Option<CheckFailure> = None;
    let mut previous: Option<&ModelId> = None;

    for tier in &config.tiers {
        if let Some(from) = previous {
            (ctx.observe)(Event::Escalating {
                from: from.clone(),
                to: tier.clone(),
            });
        }
        previous = Some(tier);

        let mut messages = vec![
            Message::system(TASK_PROMPT),
            Message::user(brief(task, context, failure.as_ref(), ctx.toolbox)),
        ];
        match attempt(&mut ctx, tier, &mut messages, &mut failure, false).await? {
            Attempt::Passed => {
                let verdict = Verdict::Passed {
                    model: tier.clone(),
                };
                return Ok(ctx.ledger.outcome(verdict, ctx.toolbox));
            }
            Attempt::Unchecked => {
                return Ok(ctx.ledger.outcome(Verdict::Unchecked, ctx.toolbox));
            }
            Attempt::Answered | Attempt::Failed => {}
        }
    }

    Ok(ctx.ledger.outcome(Verdict::GaveUp { failure }, ctx.toolbox))
}

/// Gives one model its rounds on a conversation that already holds the request.
///
/// With `may_answer`, a first round that changes no file ends the attempt as
/// an answer: there is nothing to check in a reply to a question.
async fn attempt<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    tier: &ModelId,
    messages: &mut Vec<Message>,
    failure: &mut Option<CheckFailure>,
    may_answer: bool,
) -> Result<Attempt, AgentError> {
    if let Some(model) = tier.delegate() {
        return attempt_delegated(ctx, tier, model, messages, failure, may_answer).await;
    }
    for round in 0..ctx.config.rounds_per_tier {
        if round > 0
            && let Some(f) = failure.as_ref()
        {
            messages.push(Message::user(format!(
                "The checks failed.\n\n{}",
                describe(f)
            )));
        }

        let finished = converse(ctx, tier, messages).await?;

        if may_answer && round == 0 && ctx.toolbox.changed().next().is_none() {
            return Ok(Attempt::Answered);
        }
        // With nothing to judge the change there is nothing to retry or
        // escalate on: the model's word is all there is.
        if ctx.config.checks.is_empty() {
            return Ok(Attempt::Unchecked);
        }

        match check(ctx).await? {
            None => return Ok(Attempt::Passed),
            Some(f) => *failure = Some(f),
        }

        // A model that ran out of turns is going in circles; another round
        // of the same would most likely cost the same for the same result.
        if !finished {
            break;
        }
    }
    Ok(Attempt::Failed)
}

/// Hands the task to an agent such as Claude Code, then judges what it did
/// with the checks, as for a model.
///
/// The agent gets the last message of `messages` and nothing before it: it
/// works from the task alone, in its own session. A failing check goes back
/// into that session. Its reports are added to `messages`, so that the models
/// of the conversation know what was done.
async fn attempt_delegated<M, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    tier: &ModelId,
    model: &str,
    messages: &mut Vec<Message>,
    failure: &mut Option<CheckFailure>,
    may_answer: bool,
) -> Result<Attempt, AgentError> {
    let root = ctx.toolbox.workspace().root().to_owned();
    let mut prompt = messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::User(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    // A session to continue, when the conversation was already with this
    // delegate; otherwise the delegate starts fresh.
    let mut session = ctx.thread.take();

    for round in 0..ctx.config.rounds_per_tier {
        if round > 0
            && let Some(f) = failure.as_ref()
        {
            prompt = format!("The checks failed.\n\n{}", describe(f));
            messages.push(Message::user(prompt.clone()));
        }
        let request = DelegateRequest {
            model: model.to_owned(),
            prompt: prompt.clone(),
            instructions: DELEGATE_PROMPT.to_owned(),
            resume: session.clone(),
            directory: root.clone(),
        };

        let reply = {
            let Ctx {
                delegate,
                toolbox,
                observe,
                ..
            } = &mut *ctx;
            let mut on_event = |event: DelegateEvent| {
                let event = match event {
                    DelegateEvent::TextStart => Event::Saying {
                        model: tier.clone(),
                        text: String::new(),
                        new_block: true,
                    },
                    DelegateEvent::Text(text) => Event::Saying {
                        model: tier.clone(),
                        text,
                        new_block: false,
                    },
                    DelegateEvent::Tool {
                        name,
                        input,
                        output,
                    } => {
                        let report = delegate::report(&name, &input, &output, &root);
                        if let Some(path) = report.changed {
                            toolbox.mark_changed(path);
                        }
                        Event::Tool {
                            name,
                            path: report.path,
                            outcome: report.outcome,
                        }
                    }
                };
                observe(event);
            };
            delegate
                .run(&request, &mut on_event)
                .await
                .map_err(|e| AgentError::Model(Box::new(e)))?
        };

        ctx.ledger.usage += reply.usage;
        ctx.ledger.subscription = true;
        ctx.ledger.context = reply.context.or(ctx.ledger.context);
        (ctx.observe)(Event::Turn {
            model: tier.clone(),
            usage: reply.usage,
            cost: None,
            subscription: true,
            context: reply.context,
        });
        messages.push(Message::Assistant {
            content: Some(reply.text.clone()),
            tool_calls: Vec::new(),
        });
        session = Some(reply.session);
        ctx.thread = session.clone();

        if may_answer && round == 0 && ctx.toolbox.changed().next().is_none() {
            return Ok(Attempt::Answered);
        }
        if ctx.config.checks.is_empty() {
            return Ok(Attempt::Unchecked);
        }
        match check(ctx).await? {
            None => return Ok(Attempt::Passed),
            Some(f) => *failure = Some(f),
        }
    }
    Ok(Attempt::Failed)
}

async fn check<M, D, O: FnMut(Event)>(
    ctx: &mut Ctx<'_, M, D, O>,
) -> Result<Option<CheckFailure>, AgentError> {
    (ctx.observe)(Event::Checking {
        commands: ctx.config.checks.iter().map(Check::command).collect(),
    });
    match Check::run_all(&ctx.config.checks, ctx.toolbox.workspace().root())
        .await
        .map_err(AgentError::Check)?
    {
        CheckReport::Passed => {
            (ctx.observe)(Event::Passed);
            Ok(None)
        }
        CheckReport::Failed(f) => {
            (ctx.observe)(Event::Failed {
                command: f.command.clone(),
                excerpt: f.excerpt.clone(),
            });
            Ok(Some(f))
        }
    }
}

/// Lets the model call tools until it stops or runs out of turns. Returns
/// whether it stopped on its own.
async fn converse<M: ChatModel, D, O: FnMut(Event)>(
    ctx: &mut Ctx<'_, M, D, O>,
    model_id: &ModelId,
    messages: &mut Vec<Message>,
) -> Result<bool, AgentError> {
    let tools = ctx.toolbox.specs();
    let window = ctx.model.context_window(model_id).await;
    for _ in 0..ctx.config.max_turns {
        let request = ChatRequest {
            model: model_id.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
        };
        let response = ctx
            .model
            .complete(&request)
            .await
            .map_err(|e| AgentError::Model(Box::new(e)))?;

        ctx.ledger.usage += response.usage;
        match response.cost {
            Some(cost) => ctx.ledger.cost += cost,
            None => ctx.ledger.cost_complete = false,
        }
        let context = window.map(|window| ContextUse {
            used: response.usage.input,
            window: TokenCount(window),
        });
        ctx.ledger.context = context.or(ctx.ledger.context);
        (ctx.observe)(Event::Turn {
            model: model_id.clone(),
            usage: response.usage,
            cost: response.cost,
            subscription: false,
            context,
        });
        if let Some(text) = &response.content {
            (ctx.observe)(Event::Said {
                model: model_id.clone(),
                text: text.clone(),
            });
        }

        messages.push(response.to_message());
        if response.tool_calls.is_empty() {
            return Ok(true);
        }
        for call in &response.tool_calls {
            let result = ctx.toolbox.call(call);
            (ctx.observe)(Event::Tool {
                name: call.name.clone(),
                path: path_argument(&call.arguments),
                outcome: result
                    .as_ref()
                    .map(|o| o.summary.clone())
                    .map_err(ToString::to_string),
            });
            messages.push(Message::Tool {
                call_id: call.id.clone(),
                content: result.map_or_else(|e| format!("error: {e}"), |o| o.for_model),
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

#[cfg(all(test, unix))]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::Mutex;

    use ironquill_core::{ChatResponse, DelegateReply, ToolCall};
    use ironquill_tools::{ToolSummary, Workspace};
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

    /// For tests where nothing is delegated.
    struct NoDelegate;

    impl Delegate for NoDelegate {
        type Error = Infallible;

        async fn run(
            &self,
            _: &DelegateRequest,
            _: &mut (dyn FnMut(DelegateEvent) + Send),
        ) -> Result<DelegateReply, Infallible> {
            panic!("nothing should be delegated in this test")
        }
    }

    /// An agent that writes `done.txt` on the round given, says so, and
    /// remembers each request.
    struct FakeClaude {
        writes_on_round: usize,
        requests: Mutex<Vec<DelegateRequest>>,
    }

    impl Delegate for FakeClaude {
        type Error = Infallible;

        async fn run(
            &self,
            request: &DelegateRequest,
            on_event: &mut (dyn FnMut(DelegateEvent) + Send),
        ) -> Result<DelegateReply, Infallible> {
            let round = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.clone());
                requests.len()
            };
            on_event(DelegateEvent::TextStart);
            on_event(DelegateEvent::Text("Working".into()));
            on_event(DelegateEvent::Text(" on it.".into()));
            if round == self.writes_on_round {
                let path = request.directory.join("done.txt");
                std::fs::write(&path, "ok").unwrap();
                on_event(DelegateEvent::Tool {
                    name: "Write".into(),
                    input: json!({"file_path": path.to_string_lossy(), "content": "ok"}),
                    output: Ok("File created successfully".into()),
                });
            } else {
                let path = request.directory.join("notes.txt");
                std::fs::write(&path, "draft").unwrap();
                on_event(DelegateEvent::Tool {
                    name: "Write".into(),
                    input: json!({"file_path": path.to_string_lossy(), "content": "draft"}),
                    output: Ok("File created successfully".into()),
                });
            }
            Ok(DelegateReply {
                text: format!("Report {round}"),
                session: format!("claude-session-{round}"),
                usage: Usage {
                    input: TokenCount(30_000),
                    output: TokenCount(200),
                },
                estimate: Some(Usd(0.09)),
                context: None,
            })
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
            &NoDelegate,
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
            &NoDelegate,
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

        run(
            &model,
            &NoDelegate,
            &mut toolbox,
            &config(),
            "t",
            "",
            |_| {},
        )
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

        let outcome = run(
            &model,
            &NoDelegate,
            &mut toolbox,
            &config(),
            "t",
            "",
            |_| {},
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome.verdict,
            Verdict::GaveUp { failure: Some(_) }
        ));
        assert_eq!(model.seen.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn without_checks_a_change_is_kept_unchecked() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls("write_file", json!({"path": "a.txt", "content": "x"})),
            says("Wrote a.txt."),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap();
        let mut session = Session::new();

        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "write a.txt",
                "",
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.verdict, Verdict::Unchecked);
        assert_eq!(outcome.changed, ["a.txt"]);
    }

    fn claude_config() -> AgentConfig {
        AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .check(Check::parse("test -f done.txt").unwrap())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_delegated_task_gets_the_request_alone_and_is_checked() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![says("Hi.")]);
        let mut session = Session::new();
        let mut events = Vec::new();

        // A first exchange with the chat model, which Claude must not see.
        session
            .send(
                &model,
                &claude,
                &mut toolbox,
                &config(),
                "hello",
                "FILES",
                |_| {},
            )
            .await
            .unwrap();
        let outcome = session
            .send(
                &model,
                &claude,
                &mut toolbox,
                &claude_config(),
                "create done.txt",
                "FILES",
                |e| events.push(e),
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.verdict,
            Verdict::Passed {
                model: ModelId::new("claude-code/opus").unwrap()
            }
        );
        assert!(outcome.subscription);
        assert_eq!(outcome.cost, Usd(0.0));
        assert_eq!(outcome.changed, ["done.txt"]);

        let requests = claude.requests.lock().unwrap();
        assert_eq!(requests[0].prompt, "create done.txt");
        assert_eq!(requests[0].model, "opus");
        assert_eq!(requests[0].resume, None);

        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                Event::Saying { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "Working on it.");
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Tool { path: Some(p), outcome: Ok(ToolSummary::Changed { created: true, .. }), .. } if p == "done.txt"
        )));
        assert!(session.messages.contains(&Message::Assistant {
            content: Some("Report 1".into()),
            tool_calls: vec![],
        }));
    }

    #[tokio::test]
    async fn consecutive_requests_to_claude_continue_its_session_until_another_model_answers() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![says("Hi from the chat model.")]);
        let mut session = Session::new();
        // No check, so each request is one round.
        let claude_only = AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .build()
            .unwrap();
        let chat_only = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap();

        for (config, text) in [
            (&claude_only, "first"),
            (&claude_only, "second"),
            (&chat_only, "elsewhere"),
            (&claude_only, "third"),
        ] {
            session
                .send(&model, &claude, &mut toolbox, config, text, "", |_| {})
                .await
                .unwrap();
        }

        let requests = claude.requests.lock().unwrap();
        let resumes: Vec<Option<&str>> = requests.iter().map(|r| r.resume.as_deref()).collect();
        assert_eq!(resumes, [None, Some("claude-session-1"), None]);
        assert_eq!(requests[1].prompt, "second");
    }

    #[tokio::test]
    async fn a_failing_check_goes_back_into_the_delegates_own_session() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 2,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![]);
        let mut session = Session::new();

        let outcome = session
            .send(
                &model,
                &claude,
                &mut toolbox,
                &claude_config(),
                "create done.txt",
                "",
                |_| {},
            )
            .await
            .unwrap();

        assert!(matches!(outcome.verdict, Verdict::Passed { .. }));
        let requests = claude.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].resume.as_deref(), Some("claude-session-1"));
        assert!(requests[1].prompt.starts_with("The checks failed."));
    }

    #[tokio::test]
    async fn a_question_is_answered_without_running_checks() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("Hello! What would you like to do?")]);
        let mut session = Session::new();
        let mut events = Vec::new();

        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config(),
                "hello",
                "",
                |e| events.push(e),
            )
            .await
            .unwrap();

        assert_eq!(outcome.verdict, Verdict::Answered);
        assert!(!events.iter().any(|e| matches!(e, Event::Checking { .. })));
    }

    #[tokio::test]
    async fn the_conversation_carries_over_and_context_is_sent_once() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("Hi."), says("You said hello.")]);
        let mut session = Session::new();
        let config = config();

        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "hello",
                "FILES",
                |_| {},
            )
            .await
            .unwrap();
        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "what did I say?",
                "FILES",
                |_| {},
            )
            .await
            .unwrap();

        let seen = model.seen.lock().unwrap();
        let second = &seen[1].messages;
        assert!(second.contains(&Message::user("hello")));
        let Message::System(prompt) = &second[0] else {
            panic!("the conversation should open with the system prompt");
        };
        assert_eq!(prompt.matches("FILES").count(), 1);
    }

    #[tokio::test]
    async fn a_stopped_tool_request_is_dropped_before_the_next_one() {
        let (_dir, mut toolbox) = setup();
        let mut session = Session::new();
        session.messages.push(Message::user("edit it"));
        session.messages.push(Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }],
        });
        let model = Scripted::new(vec![says("Sure.")]);

        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config(),
                "never mind",
                "",
                |_| {},
            )
            .await
            .unwrap();

        let seen = model.seen.lock().unwrap();
        assert!(
            !seen[0].messages.iter().any(
                |m| matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
            )
        );
    }
}
