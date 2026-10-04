use std::collections::BTreeMap;

use ironquill_core::{
    Agent, ChatModel, ChatRequest, ContextUse, Delegate, DelegateEvent, DelegateReply,
    DelegateRequest, Effort, Message, ModelId, Pricing, TokenCount, ToolSpec, Usage, Usd,
};
use ironquill_tools::{Check, CheckFailure, CheckReport, Toolbox};

use crate::config::{AgentConfig, Member, Pair};
use crate::delegate;
use crate::error::AgentError;
use crate::event::Event;

/// For one task with no person in the loop: `ironquill do`, and the brief a
/// stronger model gets when it takes over.
const TASK_PROMPT: &str = "You are a careful software engineer working in a project through tools. \
Make the smallest change that completes the task. Read a file before editing it. \
Edit existing files with `replace`, not `write_file`. \
Find code with `search` and `outline`, then read only the lines you need. \
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

/// Codex reads and edits through commands, in a sandbox without network.
const CODEX_PROMPT: &str = "This task was handed to you by ironquill, which runs the project's \
checks after you finish and sends you any failure. Use commands to read the project and make your \
changes, not to install anything. Make the smallest change that completes the task. When you are \
done, end with a short report: what you changed and why, and anything left to do. Write the report \
in the language the task is written in.";

/// For a model of the team the first model handed a task to.
const MEMBER_PROMPT: &str = "You are a careful software engineer. Another assistant handed you one \
task in a project, which you work on through tools; you cannot run commands. Do the task and \
nothing else. Find code with `search` and `outline`, then read only the lines you need: every \
line read is paid for again on each later turn. Read a file before editing it, and edit existing files with `replace`. Do not ask \
questions. When you are done, reply with a short report: what you found or changed, and anything \
left to do.";

/// For a member that cannot use tools: everything is in the task.
const ANSWER_PROMPT: &str = "You are a careful software engineer. Another assistant asked you \
the question below, with everything you need in it; you cannot read files or run anything. \
Answer it directly and briefly.";

/// For the planner, choosing what to read from the map of the project.
const SCOUT_PROMPT: &str = "Before planning, say which code you need to see, from the map: one \
line per excerpt, as `path:start-end`, at most 12 excerpts and about 600 lines in all. Reply with \
those lines only, or `none` if the map is enough. ironquill reads them for you.";

/// Excerpts the planner may ask for, and lines in all, so that it reads
/// what matters rather than the project.
const MAX_EXCERPTS: usize = 12;
const MAX_EXCERPT_LINES: usize = 800;

/// For the planner, which reads nothing itself.
const PLAN_PROMPT: &str = "You are the architect of a change in a software project. You see \
the code you ask for, not the whole project. Another model will implement your plan with tools; it follows instructions \
well but should not have to make design decisions. Write a precise plan: the files to change; for \
each, the functions to add or change, with their signatures and exact behaviour; the edge cases; \
the tests to add or adjust. Give short code where precision matters. If the brief lacks \
something essential, say what the implementer must read first. Be concise: every word is paid \
for. Write in the language of the request. If nothing needs to change, or no change to the code \
can help, reply with `STOP` on the first line, then the reason, and nothing else.";

/// For the planner, in every message where it decides: a way to end the
/// work rather than go on for nothing.
const STOP_RULE: &str = "If nothing needs to change, or no change to the code can help (for \
instance the checks fail for a reason outside the code), reply with `STOP` on the first line, \
then the reason, and nothing else.";

/// For Claude Code or Codex when they plan or review: their tools that
/// write are turned off.
const AGENT_PLAN_PROMPT: &str = "You plan and review; another model writes the code. Your \
tools that change files are turned off: do not try to edit anything. Read what you need, then \
answer as asked: the lines to read, the plan, or the review.";

/// How long an agent's session stays worth resuming: past it, the
/// provider's prompt cache has expired, and resuming would write the whole
/// session to the cache again, at its dearest rate. A new session is told
/// the conversation instead.
const THREAD_WARM_SECS: u64 = 5 * 60;

/// The most of the earlier conversation an agent or a planner is told, in
/// bytes, the latest kept.
/// How much said since the summary makes it worth updating: below that,
/// the messages themselves are short enough to send as they are.
const SUMMARY_AFTER_BYTES: usize = 8_000;
/// At most this much of the conversation goes into one update of the summary.
const SUMMARY_INPUT_BYTES: usize = 60_000;
const SUMMARY_PROMPT: &str = "You keep the summary of a conversation between a person and \
     coding assistants, for an assistant who joins it later and has not seen it. Keep what the \
     person wants and decided, what was done and which files changed, what failed or is left \
     to do, and what was learned about the project. Leave out greetings and the details of \
     tool calls. At most 400 words. Reply with the summary only.";
/// At most this much of the first request is kept when the conversation is
/// cut and has no summary.
const FIRST_REQUEST_BYTES: usize = 4_000;
const CATCH_UP_BYTES: usize = 24_000;

/// For the first model, once the plan is there.
const IMPLEMENT_PROMPT: &str = "Implement the plan above, exactly. The code the planner read is \
above too: do not read it again, read only what is missing. Where the plan is unclear, choose the \
simplest reading. You cannot run tests or commands: as soon as you stop calling tools, ironquill \
runs the project's checks and tells you how they went. When you are done, reply with one short \
sentence.";

/// For the planner, once the checks pass: the person will not review it.
const REVIEW_PROMPT: &str = "The checks pass. Review the work before it goes to the person, who \
will not review it. Check it against the request and your plan: every part of the request is \
done and can be used, in the interface too when the request is about it; no code is left \
unused; no module uses another's private helpers; the documentation says what changed in \
behaviour or API; the new behaviour is tested; nothing unrelated changed. Reply with exactly `OK` \
if nothing needs changing. Otherwise reply with a numbered list of concrete changes, each naming \
the file and what to do, and nothing else.";

/// How many rounds of fixes a review may ask for before the work is
/// handed over with what is left said.
const REVIEW_FIXES: usize = 2;

/// The most of a diff a review reads, in bytes.
const REVIEW_DIFF_BYTES: usize = 40_000;

/// How many times the planner may revise its plan after the checks fail.
const MAX_REVISIONS: usize = 2;

/// The tool through which the first model hands a task to the team.
const DELEGATE_TOOL: &str = "delegate";

/// For a conversation with a person.
const CHAT_PROMPT: &str = "You are a careful software engineer helping a person with the project \
in the current directory. You can read and edit its files through tools; you cannot run commands. \
Reply in the language the person writes in. Talk normally and answer questions directly. \
Only change files when the person asks for a change. \
If you need to ask the person something, ask it and end your reply there: do not call any tool \
in that reply, and do not act on a guess of the answer. They will reply in their next message. \
The project's files are listed below: use the list instead of listing directories, and do not \
try to read binary files. To find code, use `search` and `outline` first, then read only the \
lines you need with `read_file` and a range: every line read is paid for again on each later \
turn. When you change files, make the smallest change that does the job, \
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
    /// The request spent its budget before it was done; the first model
    /// said where it stopped and asked what to do.
    OverBudget {
        /// The budget it had.
        budget: Usd,
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
    /// What the conversation said since the delegate last took part, put
    /// before its next request so that it does not miss it.
    catch_up: String,
    /// How hard the model of the current step should think; the
    /// configuration's, unless a step sets its own.
    effort: Option<Effort>,
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
    /// Each agent's own session. It lasts for the whole conversation, across
    /// requests to other models and restarts, until it is reset.
    #[serde(default)]
    agents: BTreeMap<Agent, Thread>,
    /// A model's summary of the conversation, for those who start afresh.
    #[serde(default)]
    summary: Option<Summary>,
}

/// A summary of the conversation and how far it goes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Summary {
    text: String,
    /// The number of messages of the conversation it covers.
    covers: usize,
}

/// A delegate's session and how much of the conversation it has seen.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Thread {
    session: String,
    /// The number of messages of the conversation the delegate knows about.
    seen: usize,
    /// When it was last used, in seconds since 1970.
    #[serde(default)]
    used: u64,
}

/// Seconds since 1970, now.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
            agents: BTreeMap::new(),
            summary: None,
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
            catch_up: String::new(),
            effort: config.effort,
        };
        let verdict = match self.work(&mut ctx, text, context).await {
            Err(AgentError::OverBudget) => {
                let budget = config.budget.unwrap_or_default();
                (ctx.observe)(Event::OverBudget {
                    spent: ctx.ledger.cost,
                    budget,
                });
                // A tool call cut short has no result; the request must not
                // send it.
                self.messages = crate::context::sanitize(std::mem::take(&mut self.messages));
                self.explain_budget(&mut ctx, budget).await?;
                Verdict::OverBudget { budget }
            }
            verdict => verdict?,
        };
        if !matches!(verdict, Verdict::OverBudget { .. }) {
            self.summarize(&mut ctx).await;
        }
        Ok(ctx.ledger.outcome(verdict, ctx.toolbox))
    }

    /// Brings the summary of the conversation up to date, for an agent or a
    /// planner that will start afresh, when enough was said since it was
    /// written. The cheapest model of the team with a known price writes it,
    /// at a low effort; it is left as it was when that would pass the budget
    /// or the model fails, since the messages themselves still say it all.
    async fn summarize<M: ChatModel, D, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
    ) {
        let config = ctx.config;
        let models = || {
            config
                .tiers
                .iter()
                .chain(config.team.iter().map(|m| &m.model))
        };
        // Only an agent or a pair's planner ever starts afresh from it.
        if config.pair.is_none() && !models().any(|m| m.delegate().is_some()) {
            return;
        }
        if self.unsummarized(self.messages.len()).len() >= SUMMARY_AFTER_BYTES {
            self.update_summary(ctx, self.messages.len()).await;
        }
    }

    /// What the conversation said before message `to` that its summary does
    /// not cover.
    fn unsummarized(&self, to: usize) -> String {
        let covers = self.summary.as_ref().map_or(1, |s| s.covers);
        catch_up_all(&self.messages[covers.min(to)..to])
    }

    /// Updates the summary to cover the messages before `to`, with the
    /// cheapest model of the team with a known price, at a low effort.
    /// Returns whether it did.
    async fn update_summary<M: ChatModel, D, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
        to: usize,
    ) -> bool {
        let config = ctx.config;
        let models = || {
            config
                .tiers
                .iter()
                .chain(config.team.iter().map(|m| &m.model))
        };
        let since = self.unsummarized(to);
        let reference = Usage {
            input: TokenCount(10_000),
            output: TokenCount(1_000),
        };
        let mut cheapest: Option<(ModelId, Pricing)> = None;
        for model in models().filter(|m| m.delegate().is_none()) {
            let Some(pricing) = ctx.model.pricing(model).await else {
                continue;
            };
            if cheapest
                .as_ref()
                .is_none_or(|(_, p)| pricing.cost(&reference).0 < p.cost(&reference).0)
            {
                cheapest = Some((model.clone(), pricing));
            }
        }
        let Some((model, pricing)) = cheapest else {
            return false;
        };
        let previous = self.summary.as_ref().map_or("(none yet)", |s| &s.text);
        let request = ChatRequest {
            model: model.clone(),
            messages: vec![
                Message::system(SUMMARY_PROMPT),
                Message::user(format!(
                    "The summary so far:\n{previous}\n\nThe conversation since:\n{}\n\nWrite \
                     the updated summary.",
                    latest(&since, SUMMARY_INPUT_BYTES)
                )),
            ],
            tools: Vec::new(),
            effort: Some(Effort::Low),
        };
        if within_budget(ctx, Some(pricing), &request).is_err() {
            return false;
        }
        (ctx.observe)(Event::Step {
            number: 0,
            of: 0,
            name: "Summarizing the conversation, for those who join it".into(),
            model: Some(model.clone()),
            effort: Some(Effort::Low),
        });
        let Ok(response) = ctx.model.complete(&request).await else {
            return false;
        };
        ctx.ledger.usage += response.usage;
        match response.cost {
            Some(cost) => ctx.ledger.cost += cost,
            None => ctx.ledger.cost_complete = false,
        }
        (ctx.observe)(Event::Turn {
            model,
            usage: response.usage,
            cost: response.cost,
            subscription: false,
            context: None,
        });
        let text = response.content.unwrap_or_default().trim().to_owned();
        if text.is_empty() {
            return false;
        }
        self.summary = Some(Summary { text, covers: to });
        true
    }

    /// What a model starting afresh is told of the conversation before
    /// message `to`: its summary and what was said since. When what the
    /// summary leaves out would be cut, the summary is brought up to date
    /// first, so that the start of the conversation is never lost; failing
    /// that, the first request is kept with the latest part.
    async fn recap<M: ChatModel, D, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
        to: usize,
    ) -> String {
        if self.unsummarized(to).len() > CATCH_UP_BYTES {
            self.update_summary(ctx, to).await;
        }
        match &self.summary {
            Some(summary) if summary.covers <= to => {
                let since = catch_up(&self.messages[summary.covers..to]);
                let since = if since.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n\nWhat was said since:\n\n{since}")
                };
                format!(
                    "A summary of the conversation so far:\n{}{since}\n\n",
                    summary.text
                )
            }
            _ => {
                let all = catch_up_all(&self.messages[1.min(to)..to]);
                if all.len() <= CATCH_UP_BYTES {
                    return all;
                }
                // What the conversation is about, then where it is now.
                let first = self.messages[1.min(to)..to].iter().find_map(|m| match m {
                    Message::User(text) => Some(text.as_str()),
                    _ => None,
                });
                let first = first.map_or_else(String::new, |text| {
                    format!(
                        "The first request:\n{}\n\n",
                        head(text, FIRST_REQUEST_BYTES)
                    )
                });
                format!("{first}{}", latest(&all, CATCH_UP_BYTES))
            }
        }
    }

    /// Works on the request just added, from the first model on.
    async fn work<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
        text: &str,
        context: &str,
    ) -> Result<Verdict, AgentError> {
        if let Some(pair) = ctx.config.pair.clone() {
            return self.work_in_pair(ctx, text, context, &pair).await;
        }
        let Some(first) = ctx.config.tiers.first() else {
            return Err(AgentError::Config("at least one model is needed"));
        };
        let mut failure = None;

        // An agent continues its own session whichever of its models is
        // picked, and is told what was said without it meanwhile.
        let agent = first.delegate().map(|(agent, _)| agent);
        if let Some(agent) = agent {
            // A cold session starts afresh and is told the conversation.
            let thread = self.warm(agent).map(|t| (t.session.clone(), t.seen));
            let request = self.messages.len() - 1;
            ctx.catch_up = match &thread {
                Some((_, seen)) => catch_up(&self.messages[(*seen).min(request)..request]),
                None => self.recap(ctx, request).await,
            };
            ctx.thread = thread.map(|(session, _)| session);
        }
        let first_attempt = attempt(ctx, first, &mut self.messages, &mut failure, true).await;
        if let (Some(agent), Some(session)) = (agent, ctx.thread.take()) {
            self.keep_thread(agent, session);
        }

        match first_attempt? {
            Attempt::Answered => return Ok(Verdict::Answered),
            Attempt::Unchecked => {
                return Ok(Verdict::Unchecked);
            }
            Attempt::Passed => {
                return Ok(Verdict::Passed {
                    model: first.clone(),
                });
            }
            Attempt::Failed => {}
        }

        for pair in ctx.config.tiers.windows(2) {
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
            ctx.catch_up.clear();
            if let Attempt::Passed =
                attempt(ctx, to, &mut brief_messages, &mut failure, false).await?
            {
                let changed = ctx.toolbox.changed().collect::<Vec<_>>().join(", ");
                self.note(format!(
                    "(The checks kept failing, so {to} took over and made them pass. \
                     Files changed: {changed}.)"
                ));
                return Ok(Verdict::Passed { model: to.clone() });
            }
        }

        let last = failure
            .as_ref()
            .map_or_else(String::new, |f| format!(" Last failure: `{}`.", f.command));
        self.note(format!(
            "(The checks still fail after every model tried.{last})"
        ));
        Ok(Verdict::GaveUp { failure })
    }

    /// Works on the request as an architect and an editor: the first model
    /// gathers the code that matters, the planner writes a plan from it, the
    /// first model implements it and the checks judge. When they keep
    /// failing, the planner gets the failure and revises its plan.
    async fn work_in_pair<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
        text: &str,
        context: &str,
        pair: &Pair,
    ) -> Result<Verdict, AgentError> {
        let Some(coder) = ctx.config.tiers.first().cloned() else {
            return Err(AgentError::Config("at least one model is needed"));
        };
        let planner = &pair.planner;

        // The planner sees the map of the project, a parser's work, and
        // says which code it needs; ironquill reads it, no model does.
        (ctx.observe)(Event::Step {
            number: 1,
            of: 6,
            name: "Choosing the code to read".into(),
            model: Some(planner.clone()),
            effort: Some(pair.planner_effort),
        });
        let root = ctx.toolbox.workspace().root().to_owned();
        let map = ironquill_tools::project_map(&root);
        // An agent planner continues its warm session; any planner is told
        // the conversation it has not seen, so that a follow-up makes sense.
        let planner_agent = planner.delegate().map(|(agent, _)| agent);
        let thread = planner_agent
            .and_then(|agent| self.warm(agent))
            .map(|t| (t.session.clone(), t.seen));
        let request_at = self.messages.len() - 1;
        let earlier = match &thread {
            Some((_, seen)) => catch_up(&self.messages[(*seen).min(request_at)..request_at]),
            None => self.recap(ctx, request_at).await,
        };
        let mut planner_session = thread.map(|(session, _)| session);
        let earlier = if earlier.trim().is_empty() {
            String::new()
        } else {
            format!("The conversation so far:\n{earlier}\n")
        };
        let mut planning = vec![
            Message::system(format!(
                "{PLAN_PROMPT}{}",
                identity(
                    planner,
                    false,
                    &AgentConfig {
                        team: Vec::new(),
                        ..ctx.config.clone()
                    }
                )
            )),
            Message::user(format!(
                "{earlier}The request:\n{text}\n\nThe map of the project, its definitions with \
                 their lines:\n{map}\n\n{context}\n\n{SCOUT_PROMPT} {STOP_RULE}{}",
                spent_so_far(ctx)
            )),
        ];
        let wanted = ask_planner(
            ctx,
            planner,
            pair.planner_effort,
            &mut planning,
            &mut planner_session,
        )
        .await?;
        if stops(&wanted) {
            return Ok(self.stop_pair(planner, planner_session, &wanted, ctx));
        }

        (ctx.observe)(Event::Step {
            number: 2,
            of: 6,
            name: "Reading the code asked for".into(),
            model: None,
            effort: None,
        });
        let excerpts = read_excerpts(ctx, &wanted);

        (ctx.observe)(Event::Step {
            number: 3,
            of: 6,
            name: "Planning".into(),
            model: Some(planner.clone()),
            effort: Some(pair.planner_effort),
        });
        planning.push(Message::user(format!(
            "The code you asked for:\n{excerpts}\n\nNow write the plan.{}",
            spent_so_far(ctx)
        )));
        let mut plan = ask_planner(
            ctx,
            planner,
            pair.planner_effort,
            &mut planning,
            &mut planner_session,
        )
        .await?;
        if stops(&plan) {
            return Ok(self.stop_pair(planner, planner_session, &plan, ctx));
        }

        (ctx.observe)(Event::Step {
            number: 4,
            of: 6,
            name: "Coding".into(),
            model: Some(coder.clone()),
            effort: Some(pair.coder_effort),
        });
        // The coder implements it in a conversation of its own: the plan and
        // the request, not everything said before, which every call would
        // resend. The checks judge.
        ctx.effort = Some(pair.coder_effort);
        let system = self
            .messages
            .first()
            .cloned()
            .unwrap_or_else(|| Message::system(CHAT_PROMPT));
        let mut work = vec![
            system,
            Message::user(format!(
                "{text}\n\nThe code the planner read, as it is now:\n{excerpts}"
            )),
            Message::Assistant {
                content: Some(format!("Plan by {planner}:\n{plan}")),
                tool_calls: Vec::new(),
            },
            Message::user(format!("{IMPLEMENT_PROMPT}{}", spent_so_far(ctx))),
        ];
        let mut failure = None;
        // Why the pair ended early, for the account kept in the conversation.
        let mut ended = String::new();
        let result: Result<Verdict, AgentError> = 'work: {
            for revision in 0..=MAX_REVISIONS {
                let edits = ctx.toolbox.edits();
                let step = match attempt(ctx, &coder, &mut work, &mut failure, false).await {
                    Ok(step) => step,
                    Err(e) => break 'work Err(e),
                };
                match step {
                    Attempt::Passed => {
                        break 'work Ok(Verdict::Passed {
                            model: coder.clone(),
                        });
                    }
                    Attempt::Unchecked => break 'work Ok(Verdict::Unchecked),
                    Attempt::Answered => break 'work Ok(Verdict::Answered),
                    Attempt::Failed => {}
                }
                let Some(f) = failure.as_ref() else { break };
                // The coder changed nothing: another plan would not help,
                // the checks fail without the code being touched.
                if ctx.toolbox.edits() == edits {
                    ended = format!(
                        " {coder} made no change, and `{}` fails without any: it may fail for a \
                         reason outside the code, which /check can set right.",
                        f.command
                    );
                    break 'work Ok(nothing_or_gave_up(ctx.toolbox, failure.clone()));
                }
                if revision == MAX_REVISIONS {
                    break;
                }
                (ctx.observe)(Event::Step {
                    number: 3,
                    of: 6,
                    name: format!("Revising the plan: `{}` fails", f.command),
                    model: Some(planner.clone()),
                    effort: Some(pair.planner_effort),
                });
                let changed = ctx.toolbox.changed().collect::<Vec<_>>().join(", ");
                planning.push(Message::user(format!(
                    "{coder} followed the plan, but the checks fail.\n\n{}\n\nFiles it \
                     changed: {changed}\n\nRevise the plan: say what to change now, briefly. \
                     If you need to see code first, reply only with `path:start-end` lines, as \
                     before, and ironquill will read them for you. {STOP_RULE}{}",
                    describe(f),
                    spent_so_far(ctx)
                )));
                plan = match ask_planner(
                    ctx,
                    planner,
                    pair.planner_effort,
                    &mut planning,
                    &mut planner_session,
                )
                .await
                {
                    Ok(plan) => plan,
                    Err(e) => break 'work Err(e),
                };
                // It asked to see code: ironquill reads it, then it revises.
                if !looks_like_a_plan(&plan) && !excerpt_requests(&plan).is_empty() {
                    let excerpts = read_excerpts(ctx, &plan);
                    planning.push(Message::user(format!(
                        "The code you asked for, as it is now:\n{excerpts}\n\nNow revise the \
                         plan.{}",
                        spent_so_far(ctx)
                    )));
                    plan = match ask_planner(
                        ctx,
                        planner,
                        pair.planner_effort,
                        &mut planning,
                        &mut planner_session,
                    )
                    .await
                    {
                        Ok(plan) => plan,
                        Err(e) => break 'work Err(e),
                    };
                }
                if stops(&plan) {
                    ended = format!(" {planner} stopped: {}", plan.trim());
                    break 'work Ok(nothing_or_gave_up(ctx.toolbox, failure.clone()));
                }
                ctx.effort = Some(pair.coder_effort);
                (ctx.observe)(Event::Step {
                    number: 4,
                    of: 6,
                    name: "Coding the revised plan".into(),
                    model: Some(coder.clone()),
                    effort: Some(pair.coder_effort),
                });
                work.push(Message::user(format!(
                    "Revised plan by {planner}:\n{plan}\n\n{IMPLEMENT_PROMPT}{}",
                    spent_so_far(ctx)
                )));
            }
            Ok(Verdict::GaveUp {
                failure: failure.clone(),
            })
        };

        // The planner reviews what was done, from the diff, and the coder
        // fixes what it finds: nobody should have to review it after.
        let mut result = result;
        let mut reviewed = String::new();
        if matches!(result, Ok(Verdict::Passed { .. } | Verdict::Unchecked)) {
            for round in 0..=REVIEW_FIXES {
                (ctx.observe)(Event::Step {
                    number: 5,
                    of: 6,
                    name: "Reviewing the work".into(),
                    model: Some(planner.clone()),
                    effort: Some(pair.planner_effort),
                });
                let changed: Vec<String> = ctx.toolbox.changed().map(str::to_owned).collect();
                let diff = ironquill_tools::changes_text(&root, &changed, REVIEW_DIFF_BYTES).await;
                planning.push(Message::user(format!(
                    "{REVIEW_PROMPT}\n\nThe request:\n{text}\n\nWhat changed:\n{diff}{}",
                    spent_so_far(ctx)
                )));
                let review = match ask_planner(
                    ctx,
                    planner,
                    pair.planner_effort,
                    &mut planning,
                    &mut planner_session,
                )
                .await
                {
                    Ok(review) => review,
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                };
                if approves(&review) {
                    reviewed = " The review approved it.".into();
                    break;
                }
                if round == REVIEW_FIXES {
                    reviewed = format!(" The review still asks for:\n{review}");
                    break;
                }
                (ctx.observe)(Event::Step {
                    number: 6,
                    of: 6,
                    name: "Fixing what the review found".into(),
                    model: Some(coder.clone()),
                    effort: Some(pair.coder_effort),
                });
                ctx.effort = Some(pair.coder_effort);
                work.push(Message::user(format!(
                    "A review of your work by {planner} asks for these changes:\n{review}\n\n\
                     Make them.{}",
                    spent_so_far(ctx)
                )));
                let mut failed = None;
                match attempt(ctx, &coder, &mut work, &mut failed, false).await {
                    Ok(Attempt::Passed) => {
                        result = Ok(Verdict::Passed {
                            model: coder.clone(),
                        })
                    }
                    Ok(Attempt::Unchecked | Attempt::Answered) => result = Ok(Verdict::Unchecked),
                    Ok(Attempt::Failed) => {
                        result = Ok(Verdict::GaveUp { failure: failed });
                        reviewed = " The checks failed after the review's fixes.".into();
                        break;
                    }
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
        }

        // The conversation keeps what was decided and done, briefly.
        let changed = ctx.toolbox.changed().collect::<Vec<_>>().join(", ");
        let report = last_reply(&work).unwrap_or_default();
        let ending = match &result {
            Ok(Verdict::GaveUp { failure }) => format!(
                " The checks still fail after {MAX_REVISIONS} revised plans{}.",
                failure
                    .as_ref()
                    .map_or_else(String::new, |f| format!(": `{}`", f.command))
            ),
            Err(AgentError::OverBudget) => " The budget ran out before the end.".to_owned(),
            _ => String::new(),
        };
        self.note(format!(
            "(Worked in a pair: {planner} planned, {coder} implemented.{ended}{ending}{reviewed}\n\nPlan:\n{plan}\n\n\
             {coder}: {report}\nFiles changed: {})",
            if changed.is_empty() { "none" } else { &changed }
        ));
        self.keep_planner(planner, planner_session);
        result
    }

    /// Ends a pair the planner stopped before any code: its reason goes to
    /// the conversation, and its session is kept.
    fn stop_pair<M, D, O>(
        &mut self,
        planner: &ModelId,
        session: Option<String>,
        reply: &str,
        ctx: &Ctx<'_, M, D, O>,
    ) -> Verdict {
        self.keep_planner(planner, session);
        self.note(format!(
            "(Worked in a pair: {planner} stopped before any code: {})",
            reply.trim()
        ));
        nothing_or_gave_up(ctx.toolbox, None)
    }

    /// Keeps an agent planner's session, to continue it next time.
    fn keep_planner(&mut self, planner: &ModelId, session: Option<String>) {
        if let (Some((agent, _)), Some(session)) = (planner.delegate(), session) {
            self.keep_thread(agent, session);
        }
    }

    /// Hands the word back to the person once the budget is spent: the first
    /// model of the provider says where the work stopped and asks what to do.
    async fn explain_budget<M: ChatModel, D, O: FnMut(Event) + Send>(
        &mut self,
        ctx: &mut Ctx<'_, M, D, O>,
        budget: Usd,
    ) -> Result<(), AgentError> {
        let spent = ctx.ledger.cost;
        let Some(model) = ctx.config.tiers.iter().find(|m| m.delegate().is_none()) else {
            self.note(format!(
                "(The budget of {budget} for this request is spent: {spent}. The work stopped here.)"
            ));
            return Ok(());
        };
        self.messages.push(Message::user(format!(
            "(ironquill: the budget of {budget} for this request is spent, {spent} so far, so the \
             work stopped here. Do not call any tool. In the language of the request, explain \
             briefly what was done, what is left and what made it cost this much, then ask what \
             to do next: for example go on with a bigger budget (/budget <dollars>), take \
             another approach, or stop.)"
        )));
        let request = ChatRequest {
            model: model.clone(),
            messages: self.messages.clone(),
            tools: Vec::new(),
            effort: ctx.effort,
        };
        // Saying where things stand may cost a tenth of the budget, no more:
        // a conversation too long for that gets ironquill's own words.
        let likely = likely_cost(ctx.model.pricing(model).await, &request);
        if likely.0 > budget.0 * 0.1 {
            self.messages.pop();
            let text = format!(
                "The budget of {budget} for this request is reached, {spent} spent: the work \
                 stopped before a call that would have gone past it. Say how to go on, or raise \
                 the budget with /budget."
            );
            (ctx.observe)(Event::Said {
                model: model.clone(),
                text: text.clone(),
            });
            self.note(format!("({text})"));
            return Ok(());
        }
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
        (ctx.observe)(Event::Turn {
            model: model.clone(),
            usage: response.usage,
            cost: response.cost,
            subscription: false,
            context: None,
        });
        if let Some(text) = &response.content {
            (ctx.observe)(Event::Said {
                model: model.clone(),
                text: text.clone(),
            });
        }
        self.messages.push(Message::Assistant {
            content: response.content,
            tool_calls: Vec::new(),
        });
        Ok(())
    }

    /// The conversation as a document to edit; see [`Session::apply_text`].
    pub fn to_text(&self) -> String {
        crate::context::to_text(&self.messages)
    }

    /// Replaces the conversation with an edited document. The next request
    /// is sent with exactly that.
    ///
    /// # Errors
    ///
    /// A sentence for the person when the document cannot be read; the
    /// conversation is then left as it was.
    pub fn apply_text(&mut self, text: &str) -> Result<(), String> {
        self.messages = crate::context::sanitize(crate::context::from_text(text)?);
        for thread in self.agents.values_mut() {
            thread.seen = thread.seen.min(self.messages.len());
        }
        // The edited system message holds whatever context the person kept;
        // a summary could tell what was taken out.
        self.context_added = true;
        self.summary = None;
        Ok(())
    }

    /// A rough size of what the next request will send, in tokens.
    pub fn approx_tokens(&self) -> u64 {
        crate::context::approx_tokens(&self.messages)
    }

    /// The session of `agent` worth resuming: one used in the last minutes,
    /// whose cache still holds.
    fn warm(&self, agent: Agent) -> Option<&Thread> {
        self.agents
            .get(&agent)
            .filter(|t| now_secs().saturating_sub(t.used) <= THREAD_WARM_SECS)
    }

    /// Keeps the session `agent` ended in, as knowing everything said so far.
    fn keep_thread(&mut self, agent: Agent, session: String) {
        let thread = Thread {
            session,
            seen: self.messages.len(),
            used: now_secs(),
        };
        self.agents.insert(agent, thread);
    }

    /// The session of `agent` its next request would continue, if any.
    pub fn delegate_session(&self, agent: Agent) -> Option<&str> {
        self.agents.get(&agent).map(|t| t.session.as_str())
    }

    /// Ends the session of `agent`: its next request starts from nothing.
    pub fn forget_delegate(&mut self, agent: Agent) {
        self.agents.remove(&agent);
    }

    /// Writes what happened outside the conversation into it.
    fn note(&mut self, text: String) {
        self.messages.push(Message::Assistant {
            content: Some(text),
            tool_calls: Vec::new(),
        });
    }

    /// Puts the conversation in the shape providers accept before it is sent:
    /// a stopped request can leave a call without its result, an edit can
    /// leave either alone. Providers refuse a conversation where they are.
    fn settle(&mut self) {
        self.messages = crate::context::sanitize(std::mem::take(&mut self.messages));
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
        catch_up: String::new(),
        effort: config.effort,
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
        let result = attempt(&mut ctx, tier, &mut messages, &mut failure, false).await;
        if let (Err(AgentError::OverBudget), Some(budget)) = (&result, config.budget) {
            (ctx.observe)(Event::OverBudget {
                spent: ctx.ledger.cost,
                budget,
            });
            return Ok(ctx
                .ledger
                .outcome(Verdict::OverBudget { budget }, ctx.toolbox));
        }
        match result? {
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
    if let Some((agent, model)) = tier.delegate() {
        return attempt_delegated(ctx, tier, agent, model, messages, failure, may_answer).await;
    }
    // The edits made when the checks last failed: with none since, running
    // them again would only fail the same way.
    let mut failed_at = None;
    for round in 0..ctx.config.rounds_per_tier {
        if round > 0
            && let Some(f) = failure.as_ref()
        {
            messages.push(Message::user(format!(
                "The checks failed.\n\n{}",
                describe(f)
            )));
        }

        let role = if may_answer { Role::Lead } else { Role::Member };
        let finished = converse(ctx, tier, messages, role).await?;

        if may_answer && round == 0 && ctx.toolbox.changed().next().is_none() {
            return Ok(Attempt::Answered);
        }
        // With nothing to judge the change there is nothing to retry or
        // escalate on: the model's word is all there is.
        if checks_now(ctx).is_empty() {
            return Ok(Attempt::Unchecked);
        }
        if failed_at == Some(ctx.toolbox.edits()) {
            break;
        }

        match check(ctx).await? {
            None => return Ok(Attempt::Passed),
            Some(f) => {
                *failure = Some(f);
                failed_at = Some(ctx.toolbox.edits());
            }
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
/// The agent gets the last message of `messages`, after what it missed of
/// the conversation, and works in its own session. A failing check goes back
/// into that session. Its reports are added to `messages`, so that the models
/// of the conversation know what was done.
async fn attempt_delegated<M, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    tier: &ModelId,
    agent: Agent,
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
    // A session to continue, when the delegate already took part in the
    // conversation; otherwise it starts fresh.
    let mut session = ctx.thread.take();
    let catch_up = std::mem::take(&mut ctx.catch_up);
    if !catch_up.is_empty() {
        prompt = format!(
            "Meanwhile the conversation went on with another assistant:\n\n{catch_up}\n\
             The request now:\n\n{prompt}"
        );
    }

    let mut failed_at = None;
    for round in 0..ctx.config.rounds_per_tier {
        if round > 0
            && let Some(f) = failure.as_ref()
        {
            prompt = format!("The checks failed.\n\n{}", describe(f));
            messages.push(Message::user(prompt.clone()));
        }
        let request = DelegateRequest {
            agent,
            effort: ctx.effort,
            model: model.to_owned(),
            prompt: prompt.clone(),
            instructions: agent_instructions(
                match agent {
                    Agent::ClaudeCode => DELEGATE_PROMPT,
                    Agent::Codex => CODEX_PROMPT,
                },
                ctx.config,
            ),
            resume: session.clone(),
            directory: root.clone(),
            read_only: false,
        };

        let reply = run_agent(ctx, tier, &request).await?;
        messages.push(Message::Assistant {
            content: Some(reply.text.clone()),
            tool_calls: Vec::new(),
        });
        session = Some(reply.session);
        ctx.thread = session.clone();

        if may_answer && round == 0 && ctx.toolbox.changed().next().is_none() {
            return Ok(Attempt::Answered);
        }
        if checks_now(ctx).is_empty() {
            return Ok(Attempt::Unchecked);
        }
        // Nothing changed since the checks failed: they would fail again.
        if failed_at == Some(ctx.toolbox.edits()) {
            break;
        }
        match check(ctx).await? {
            None => return Ok(Attempt::Passed),
            Some(f) => {
                *failure = Some(f);
                failed_at = Some(ctx.toolbox.edits());
            }
        }
    }
    Ok(Attempt::Failed)
}

/// Runs one request of an agent, showing what it does as it goes and
/// counting what it used.
async fn run_agent<M, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    tier: &ModelId,
    request: &DelegateRequest,
) -> Result<DelegateReply, AgentError> {
    let root = request.directory.clone();
    // What was counted as it came: for this request, that is the truth.
    let mut live_calls = 0_u32;
    let mut unpriced = false;
    let mut live_cost = Usd(0.0);
    let reply = {
        let Ctx {
            delegate,
            toolbox,
            observe,
            ledger,
            ..
        } = &mut *ctx;
        let mut on_event = |event: DelegateEvent| {
            let event = match event {
                // Each message of the agent counts at once, so that the
                // cost moves while it works.
                DelegateEvent::Usage {
                    usage,
                    cost,
                    billed,
                } => {
                    live_calls += 1;
                    unpriced |= billed && cost.is_none();
                    ledger.usage += usage;
                    let cost = cost.filter(|_| billed);
                    if let Some(cost) = cost {
                        live_cost += cost;
                        ledger.cost += cost;
                    } else {
                        ledger.subscription = true;
                    }
                    Event::Turn {
                        model: tier.clone(),
                        usage,
                        cost,
                        subscription: !billed,
                        context: None,
                    }
                }
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
            .run(request, &mut on_event)
            .await
            .map_err(|e| AgentError::Model(Box::new(e)))?
    };

    // The calls counted as they came are what this request used. The
    // agent's own total covers its whole session, earlier requests included
    // when it was resumed: it may only stand in for what could not be
    // counted, and only for a session that began with this request.
    let resumed = request.resume.is_some();
    let rest = if live_calls > 0 {
        Usage::default()
    } else {
        reply.usage
    };
    let cost = if !reply.billed {
        ctx.ledger.subscription = true;
        None
    } else if live_calls > 0 && !unpriced {
        Some(Usd(0.0))
    } else if !resumed && let Some(estimate) = reply.estimate {
        Some(Usd((estimate.0 - live_cost.0).max(0.0)))
    } else {
        // Part of it was not priced and the session's total cannot say.
        ctx.ledger.cost_complete = false;
        None
    };
    ctx.ledger.usage += rest;
    if let Some(cost) = cost {
        ctx.ledger.cost += cost;
    }
    ctx.ledger.context = reply.context.or(ctx.ledger.context);
    // Nothing left to add when every call was counted as it came.
    let left = rest.input.0 + rest.output.0 > 0
        || cost.is_some_and(|c| c.0 > 0.0)
        || reply.context.is_some();
    if left {
        (ctx.observe)(Event::Turn {
            model: tier.clone(),
            usage: rest,
            cost,
            subscription: !reply.billed,
            context: reply.context,
        });
    }
    Ok(reply)
}

/// Tool results a compaction keeps at most: the latest ones are what the
/// model is working on. It keeps fewer when that is not enough.
const KEEP_RESULTS: usize = 4;

/// Output tokens counted for a call before it is made: a reply that edits
/// code is rarely longer.
const OUTPUT_RESERVE: u64 = 2_000;

/// What `request` will likely cost at `pricing`: what it sends, by a rough
/// count, and a reply of [`OUTPUT_RESERVE`] tokens.
fn likely_cost(pricing: Option<Pricing>, request: &ChatRequest) -> Usd {
    let Some(pricing) = pricing else {
        return Usd(0.0);
    };
    let tools: usize = request
        .tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
        .sum();
    let input = crate::context::approx_tokens(&request.messages) + (tools / 4) as u64;
    pricing.cost(&Usage {
        input: TokenCount(input),
        output: TokenCount(OUTPUT_RESERVE),
    })
}

/// Stops before a call that would take the request past its budget: what
/// was spent, plus what the call will likely cost when the price is known.
/// Each call resends the whole conversation, so with an expensive model one
/// call alone can cost more than what is left.
fn within_budget<M, D, O>(
    ctx: &Ctx<'_, M, D, O>,
    pricing: Option<Pricing>,
    request: &ChatRequest,
) -> Result<(), AgentError> {
    match ctx.config.budget {
        Some(budget) if ctx.ledger.cost.0 + likely_cost(pricing, request).0 > budget.0 => {
            Err(AgentError::OverBudget)
        }
        _ => Ok(()),
    }
}

/// How a pair that ended early ends: an answer when no file changed, else
/// a failure with the files as they are.
fn nothing_or_gave_up(toolbox: &Toolbox, failure: Option<CheckFailure>) -> Verdict {
    if toolbox.changed().next().is_none() {
        Verdict::Answered
    } else {
        Verdict::GaveUp { failure }
    }
}

/// Whether the planner chose to stop: `STOP` as the first word of its
/// reply, alone or followed by its reason.
fn stops(reply: &str) -> bool {
    let first = reply
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let first = first.trim().trim_start_matches(['*', '`', '#', ' ']);
    if !first
        .get(..4)
        .is_some_and(|w| w.eq_ignore_ascii_case("stop"))
    {
        return false;
    }
    let after = &first[4..];
    after.is_empty() || after.starts_with([' ', ':', '*', '`', '.', '-', '\u{2014}'])
}

/// Whether a review found nothing to change: it answered `OK`.
fn approves(review: &str) -> bool {
    let review = review.trim().trim_matches(['`', '*', '.', '!']).trim();
    review.eq_ignore_ascii_case("ok")
}

/// Whether a planner's reply is a plan rather than only a list of code to
/// see: most of its lines are something else than `path:start-end`.
fn looks_like_a_plan(reply: &str) -> bool {
    let lines = reply.lines().filter(|l| !l.trim().is_empty()).count();
    lines > excerpt_requests(reply).len() * 2 + 1
}

/// The excerpts a planner asked for, as `path:start-end` lines.
fn excerpt_requests(text: &str) -> Vec<(String, usize, usize)> {
    text.lines()
        .filter_map(|line| {
            let line = line
                .trim()
                .trim_start_matches(['-', '*', ' '])
                .trim_matches('`')
                .trim();
            let (path, range) = line.rsplit_once(':')?;
            let (start, end) = range.trim().split_once('-')?;
            let (start, end) = (start.trim().parse().ok()?, end.trim().parse().ok()?);
            (!path.is_empty() && start >= 1 && end >= start)
                .then(|| (path.trim().to_owned(), start, end))
        })
        .take(MAX_EXCERPTS)
        .collect()
}

/// Reads what the planner asked for with the read tool, in the sandbox,
/// shown as reads. No model is involved: the lines are copied as they are.
fn read_excerpts<M, D, O: FnMut(Event)>(ctx: &mut Ctx<'_, M, D, O>, wanted: &str) -> String {
    let mut out = Vec::new();
    let mut lines = 0;
    for (n, (path, start, end)) in excerpt_requests(wanted).into_iter().enumerate() {
        let end = end.min(start + (MAX_EXCERPT_LINES.saturating_sub(lines)).max(1) - 1);
        if lines >= MAX_EXCERPT_LINES {
            out.push(format!(
                "({path}:{start}-{end} not read: enough lines already)"
            ));
            continue;
        }
        let call = ironquill_core::ToolCall {
            id: format!("excerpt-{n}"),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": path, "start": start, "end": end}).to_string(),
        };
        let result = ctx.toolbox.call(&call);
        (ctx.observe)(Event::Tool {
            name: call.name.clone(),
            path: Some(path.clone()),
            outcome: result
                .as_ref()
                .map(|o| o.summary.clone())
                .map_err(ToString::to_string),
        });
        match result {
            Ok(output) => {
                lines += output.for_model.lines().count();
                out.push(format!("{path}\n{}", output.for_model));
            }
            Err(e) => out.push(format!("{path}:{start}-{end}: {e}")),
        }
    }
    if out.is_empty() {
        "(nothing was asked for)".into()
    } else {
        out.join("\n\n")
    }
}

/// The latest thing a model wrote in `messages`.
fn last_reply(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| match m {
        Message::Assistant {
            content: Some(text),
            ..
        } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    })
}

/// A line saying what the request has cost so far, and its budget, so that
/// each model knows where things stand when it gets a message.
fn spent_so_far<M, D, O>(ctx: &Ctx<'_, M, D, O>) -> String {
    match ctx.config.budget {
        Some(budget) => format!(
            "\n\n(This request has cost {} so far, of a budget of {budget}.)",
            ctx.ledger.cost
        ),
        None => format!("\n\n(This request has cost {} so far.)", ctx.ledger.cost),
    }
}

/// Asks the planner for its plan, or its revision: one call without tools,
/// its conversation kept in `planning` so that it follows the thread. An
/// agent such as Claude Code gets the latest message and reads what it
/// wants itself.
async fn ask_planner<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    planner: &ModelId,
    effort: Effort,
    planning: &mut Vec<Message>,
    session: &mut Option<String>,
) -> Result<String, AgentError> {
    ctx.effort = Some(effort);
    if let Some((agent, model)) = planner.delegate() {
        // One session for the whole pair, so that it knows the request, the
        // code it read and the plan it wrote; and it may only read.
        let prompt = match planning.last() {
            Some(Message::User(text)) => text.clone(),
            _ => String::new(),
        };
        let request = DelegateRequest {
            agent,
            effort: ctx.effort,
            model: model.to_owned(),
            prompt,
            instructions: agent_instructions(
                &format!("{PLAN_PROMPT}\n\n{AGENT_PLAN_PROMPT}"),
                ctx.config,
            ),
            resume: session.clone(),
            directory: ctx.toolbox.workspace().root().to_owned(),
            read_only: true,
        };
        let reply = run_agent(ctx, planner, &request).await?;
        *session = Some(reply.session);
        let text = reply.text;
        planning.push(Message::Assistant {
            content: Some(text.clone()),
            tool_calls: Vec::new(),
        });
        return Ok(text);
    }
    let request = ChatRequest {
        model: planner.clone(),
        messages: planning.clone(),
        tools: Vec::new(),
        effort: ctx.effort,
    };
    let pricing = ctx.model.pricing(planner).await;
    within_budget(ctx, pricing, &request)?;
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
    (ctx.observe)(Event::Turn {
        model: planner.clone(),
        usage: response.usage,
        cost: response.cost,
        subscription: false,
        context: None,
    });
    let text = response.content.unwrap_or_default();
    (ctx.observe)(Event::Said {
        model: planner.clone(),
        text: text.clone(),
    });
    planning.push(Message::Assistant {
        content: Some(text.clone()),
        tool_calls: Vec::new(),
    });
    Ok(text)
}

/// Who the model is, added to its instructions on every call: models
/// otherwise guess, and a conversation that went through several of them
/// misleads them further.
fn identity(model: &ModelId, leads: bool, config: &AgentConfig) -> String {
    let team = &config.team;
    let mut text = format!(
        "\n\nYou are the model `{model}`, used through ironquill, a coding agent in the \
         person's terminal. When asked which model you are, say `{model}`. Earlier replies in \
         this conversation may come from other models the person picked; they are not you."
    );
    let others: Vec<&Member> = team.iter().filter(|m| &m.model != model).collect();
    if leads && !others.is_empty() {
        let names: Vec<String> = others.iter().map(|m| format!("`{}`", m.model)).collect();
        text.push_str(&format!(
            " The person gave you a team, {}, that you can hand tasks to with the `delegate` \
             tool; you remain the one who answers. Before working on a request, check whether \
             a member is better suited to it, from what the tool says each is good at, and \
             hand it over when one is. When a member did the work, say which.",
            names.join(", ")
        ));
    }
    if let Some(rules) = &config.project_rules {
        text.push_str(&format!(
            "\n\nThe project's own instructions for coding agents, from its files; follow \
             them, and those scoped to some files only for those files:\n{}",
            rules.trim()
        ));
    }
    if let Some(instructions) = &config.instructions {
        text.push_str(&person_says(instructions));
    }
    text
}

/// The person's own instructions, as added to a model's.
fn person_says(instructions: &str) -> String {
    format!(
        "\n\nThe person's own instructions, for every project; they come before the \
         general ones above when the two differ:\n{}",
        instructions.trim()
    )
}

/// Instructions for an agent such as Claude Code: `base`, and the person's
/// own when there are some.
fn agent_instructions(base: &str, config: &AgentConfig) -> String {
    match &config.instructions {
        Some(instructions) => format!("{base}{}", person_says(instructions)),
        None => base.to_owned(),
    }
}

/// The `delegate` tool, offering the team to `lead`; `None` when nobody
/// else is in it.
fn delegate_spec(team: &[Member], lead: &ModelId) -> Option<ToolSpec> {
    let members: Vec<&Member> = team.iter().filter(|m| &m.model != lead).collect();
    if members.is_empty() {
        return None;
    }
    let list: String = members
        .iter()
        .map(|m| {
            let mut line = format!("- {}", m.model);
            for part in [m.note.as_str(), m.about.as_str()] {
                if !part.is_empty() {
                    line.push_str(" — ");
                    line.push_str(part);
                }
            }
            if !m.tools {
                line.push_str(
                    " — It cannot use tools: it reads no file and changes nothing, so put \
                     everything it needs in the task, such as a question with the code it is about.",
                );
            }
            line.push('\n');
            line
        })
        .collect();
    let ids: Vec<&str> = members.iter().map(|m| m.model.as_str()).collect();
    Some(ToolSpec {
        name: DELEGATE_TOOL.into(),
        description: format!(
            "Hand one task to another model of the team and get its report back. It works on \
             the same files with the same tools, but does not see this conversation: write a \
             task that says everything it needs. Choose by what each member is good at, as \
             described below: hand over what another model does better or more cheaply, such \
             as a hard change to a stronger model or a long read to a cheaper one, and do the \
             rest yourself. Every request has a budget, so mind the prices.\nTeam:\n{list}"
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "model": {"type": "string", "enum": ids},
                "task": {"type": "string", "description": "What to do, complete in itself."}
            },
            "required": ["model", "task"]
        }),
    })
}

/// Runs a `delegate` call: the member works on the task, and its report,
/// with the files it changed, is what the calling model gets back.
async fn hand_over<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    from: &ModelId,
    arguments: &str,
) -> Result<String, AgentError> {
    #[derive(serde::Deserialize)]
    struct Args {
        model: String,
        task: String,
    }
    let Ok(args) = serde_json::from_str::<Args>(arguments) else {
        return Ok("error: expected {\"model\": ..., \"task\": ...}".into());
    };
    let Some(member) = ctx
        .config
        .team
        .iter()
        .find(|m| m.model.as_str() == args.model && &m.model != from)
        .cloned()
    else {
        return Ok(format!("error: {} is not in the team", args.model));
    };
    let to = member.model;
    (ctx.observe)(Event::Delegating {
        from: from.clone(),
        to: to.clone(),
        task: args.task.clone(),
    });
    let before: Vec<String> = ctx.toolbox.changed().map(str::to_owned).collect();

    let report = if let Some((agent, model)) = to.delegate() {
        let request = DelegateRequest {
            agent,
            effort: ctx.effort,
            model: model.to_owned(),
            prompt: args.task,
            instructions: agent_instructions(
                match agent {
                    Agent::ClaudeCode => DELEGATE_PROMPT,
                    Agent::Codex => CODEX_PROMPT,
                },
                ctx.config,
            ),
            resume: None,
            directory: ctx.toolbox.workspace().root().to_owned(),
            read_only: false,
        };
        run_agent(ctx, &to, &request).await?.text
    } else if !member.tools {
        answer_only(ctx, &to, args.task).await?
    } else {
        let mut messages = vec![Message::system(MEMBER_PROMPT), Message::user(args.task)];
        let finished = converse(ctx, &to, &mut messages, Role::Member).await?;
        let last = messages.iter().rev().find_map(|m| match m {
            Message::Assistant {
                content: Some(text),
                ..
            } if !text.is_empty() => Some(text.clone()),
            _ => None,
        });
        let mut report = last.unwrap_or_else(|| "(no report)".into());
        if !finished {
            report.push_str("\n(It ran out of turns before it was done.)");
        }
        report
    };

    let changed: Vec<&str> = ctx
        .toolbox
        .changed()
        .filter(|path| !before.iter().any(|b| b == path))
        .collect();
    let changed = if changed.is_empty() {
        "none".to_owned()
    } else {
        changed.join(", ")
    };
    Ok(format!(
        "{to} reports:\n{report}\n\nFiles it changed: {changed}"
    ))
}

/// Asks a member that cannot use tools: one call, no tools, its answer as
/// the report.
async fn answer_only<M: ChatModel, D, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    model: &ModelId,
    task: String,
) -> Result<String, AgentError> {
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![
            Message::system(format!(
                "{ANSWER_PROMPT}{}",
                identity(model, false, ctx.config)
            )),
            Message::user(task),
        ],
        tools: Vec::new(),
        effort: ctx.effort,
    };
    let pricing = ctx.model.pricing(model).await;
    within_budget(ctx, pricing, &request)?;
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
    (ctx.observe)(Event::Turn {
        model: model.clone(),
        usage: response.usage,
        cost: response.cost,
        subscription: false,
        context: None,
    });
    let text = response.content.unwrap_or_default();
    (ctx.observe)(Event::Said {
        model: model.clone(),
        text: text.clone(),
    });
    Ok(if text.is_empty() {
        "(no report)".into()
    } else {
        text
    })
}

/// The checks to run now: the ones configured, or, when there are none and
/// the configuration says to look, the project's own as they are now, so
/// that tests just written are run too.
fn checks_now<M, D, O>(ctx: &Ctx<'_, M, D, O>) -> Vec<Check> {
    if ctx.config.checks.is_empty() && ctx.config.detect_checks {
        ironquill_tools::detect_checks(ctx.toolbox.workspace().root())
    } else {
        ctx.config.checks.clone()
    }
}

async fn check<M, D, O: FnMut(Event)>(
    ctx: &mut Ctx<'_, M, D, O>,
) -> Result<Option<CheckFailure>, AgentError> {
    let checks = checks_now(ctx);
    (ctx.observe)(Event::Checking {
        commands: checks.iter().map(Check::command).collect(),
    });
    match Check::run_all(&checks, ctx.toolbox.workspace().root())
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

/// What a model may do in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The model that answers: every tool, and the team.
    Lead,
    /// Working on a task: every tool, no team.
    Member,
}

/// Lets the model call tools until it stops or runs out of turns. Returns
/// whether it stopped on its own. What it may call depends on its `role`.
async fn converse<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    model_id: &ModelId,
    messages: &mut Vec<Message>,
    role: Role,
) -> Result<bool, AgentError> {
    let leads = role == Role::Lead;
    let mut tools = ctx.toolbox.specs();
    if leads && let Some(spec) = delegate_spec(&ctx.config.team, model_id) {
        tools.push(spec);
    }
    let window = ctx.model.context_window(model_id).await;
    let identity = identity(model_id, leads, ctx.config);
    let pricing = ctx.model.pricing(model_id).await;
    let compact_at = window.map_or(ctx.config.compact_at, |w| ctx.config.compact_at.min(w / 2));
    for _ in 0..ctx.config.max_turns {
        let before = crate::context::approx_tokens(messages);
        if before > compact_at {
            // Down to half the threshold, so that the next calls only add
            // to it: each compaction makes the provider's cache start over.
            let mut compacted = messages.clone();
            let mut dropped = 0;
            for keep in (1..=KEEP_RESULTS).rev() {
                dropped += crate::context::compact(&mut compacted, keep);
                if crate::context::approx_tokens(&compacted) <= compact_at / 2 {
                    break;
                }
            }
            // Only when it is worth it: a fifth less at least. A conversation
            // long with words rather than tool results gains little, and
            // would lose its cache on every call.
            let after = crate::context::approx_tokens(&compacted);
            if dropped > 0 && after * 5 <= before * 4 {
                *messages = compacted;
                (ctx.observe)(Event::Compacted {
                    dropped,
                    before: TokenCount(before),
                    after: TokenCount(after),
                });
            }
        }
        let mut sent = messages.clone();
        if let Some(Message::System(prompt)) = sent.first_mut() {
            prompt.push_str(&identity);
        }
        let request = ChatRequest {
            model: model_id.clone(),
            messages: sent,
            tools: tools.clone(),
            effort: ctx.effort,
        };
        within_budget(ctx, pricing, &request)?;
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
            if leads && call.name == DELEGATE_TOOL {
                let report = Box::pin(hand_over(ctx, model_id, &call.arguments)).await?;
                messages.push(Message::Tool {
                    call_id: call.id.clone(),
                    content: report,
                });
                continue;
            }
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

/// What these messages said, as text for a delegate that did not see them:
/// the people's requests and the replies, with the tools named but not their
/// results, which the delegate can read again itself.
fn catch_up(messages: &[Message]) -> String {
    latest(&catch_up_all(messages), CATCH_UP_BYTES)
}

/// The last `bytes` of `out`, from the start of a line, saying that the
/// earlier part is left out.
fn latest(out: &str, bytes: usize) -> String {
    if out.len() <= bytes {
        return out.to_owned();
    }
    // The latest part, from the start of a line that is not blank.
    let from = out.len() - bytes;
    let from = (from..out.len())
        .find(|i| out.as_bytes()[i - 1] == b'\n' && out.as_bytes()[*i] != b'\n')
        .or_else(|| (from..out.len()).find(|i| out.is_char_boundary(*i)))
        .unwrap_or(out.len());
    format!(
        "(the earlier part of the conversation is left out)\n{}",
        &out[from..]
    )
}

/// The first `bytes` of `text` at most, cut at a character.
fn head(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn catch_up_all(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            Message::User(text) => out.push_str(&format!("User: {text}\n\n")),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                for call in tool_calls {
                    out.push_str(&format!("(ran {} {})\n", call.name, call.arguments));
                }
                if let Some(text) = content.as_deref().filter(|t| !t.is_empty()) {
                    out.push_str(&format!("Assistant: {text}\n\n"));
                }
            }
            Message::System(_) | Message::Tool { .. } => {}
        }
    }
    out
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
        pricing: Option<Pricing>,
    }

    impl Scripted {
        fn new(answers: Vec<ChatResponse>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                seen: Mutex::new(Vec::new()),
                pricing: None,
            }
        }

        fn priced(self, input: f64, output: f64) -> Self {
            Self {
                pricing: Some(Pricing::per_token(Usd(input), Usd(output)).unwrap()),
                ..self
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

        async fn pricing(&self, _: &ModelId) -> Option<Pricing> {
            self.pricing
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
                billed: false,
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

    fn pair_config() -> AgentConfig {
        AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .member(Member::new(ModelId::new("strong").unwrap(), ""))
            .check(Check::parse("test -f done.txt").unwrap())
            .budget(Usd(1.0))
            .pair(Pair {
                planner: ModelId::new("strong").unwrap(),
                planner_effort: Effort::Max,
                coder_effort: Effort::Low,
            })
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn the_planner_picks_the_code_to_read_then_the_cheap_model_codes() {
        let (dir, mut toolbox) = setup();
        std::fs::write(
            dir.path().join("app.py"),
            "def f():\n    pass\n\n\ndef g():\n    pass\n",
        )
        .unwrap();
        let model = Scripted::new(vec![
            // The planner sees the map and asks for lines.
            says("- `app.py:1-2`"),
            // It plans from them.
            says("Create done.txt containing ok."),
            // The coder implements.
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
            // The review approves it.
            says("OK"),
        ]);
        let mut session = Session::new();
        // A long conversation before: the coding step must not resend it.
        session.messages.push(Message::user("an earlier question"));
        session.messages.push(Message::Assistant {
            content: Some("an earlier answer".into()),
            tool_calls: vec![],
        });
        let mut events = Vec::new();
        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &pair_config(),
                "make done",
                "",
                |e| {
                    events.push(e);
                },
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.verdict,
            Verdict::Passed {
                model: ModelId::new("cheap").unwrap()
            }
        );
        let seen = model.seen.lock().unwrap();
        // Choosing: the planner, no tools, its effort, the map of the project.
        assert_eq!(seen[0].model.as_str(), "strong");
        assert!(seen[0].tools.is_empty());
        assert_eq!(seen[0].effort, Some(Effort::Max));
        let Some(Message::User(asked)) = seen[0].messages.last() else {
            panic!("the planner gets the request and the map");
        };
        assert!(asked.contains("make done") && asked.contains("L5    def g():"));
        assert!(asked.contains("so far, of a budget of $1.00"));
        // Planning: the lines it asked for, read by ironquill, not the rest.
        let Some(Message::User(code)) = seen[1].messages.last() else {
            panic!("the planner gets the code it asked for");
        };
        assert!(code.contains("(lines 1 to 2 of 6)") && code.contains("def f():"));
        assert!(!code.contains("def g():"));
        // Coding: the cheap model, its own conversation, every tool.
        assert_eq!(seen[2].model.as_str(), "cheap");
        assert_eq!(seen[2].effort, Some(Effort::Low));
        assert_eq!(seen[2].messages.len(), 4);
        assert!(!format!("{:?}", seen[2].messages).contains("an earlier"));
        assert!(seen[2].tools.iter().any(|t| t.name == "write_file"));
        // It gets the code the planner read, so as not to read it again.
        assert!(matches!(
            &seen[2].messages[1],
            Message::User(t) if t.starts_with("make done") && t.contains("def f():")
        ));
        // Who works is announced at each step.
        let steps: Vec<(u8, Option<&str>)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Step { number, model, .. } => {
                    Some((*number, model.as_ref().map(ModelId::as_str)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            steps,
            [
                (1, Some("strong")),
                (2, None),
                (3, Some("strong")),
                (4, Some("cheap")),
                (5, Some("strong"))
            ]
        );
        // The session keeps a short account of it.
        assert!(matches!(
            session.messages.last(),
            Some(Message::Assistant { content: Some(t), .. })
                if t.starts_with("(Worked in a pair: strong planned, cheap implemented. The review approved it.")
                    && t.contains("Files changed: done.txt")
        ));
    }

    #[test]
    fn excerpt_requests_are_read_from_loose_lines() {
        assert_eq!(
            excerpt_requests("Here:\n- `a.py:3-9`\n* b/c.rs:10-12\nnone\nd.py:9-3"),
            [("a.py".to_owned(), 3, 9), ("b/c.rs".to_owned(), 10, 12)]
        );
    }

    #[tokio::test]
    async fn the_review_finds_what_the_tests_miss_and_the_coder_fixes_it() {
        let (dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            says("none"),
            says("Create done.txt and document it in README.md."),
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
            // The checks pass, but the README was forgotten.
            says("1. README.md: say what done.txt is for."),
            calls(
                "write_file",
                json!({"path": "README.md", "content": "done.txt marks it done."}),
            ),
            says("Documented."),
            says("OK"),
        ]);
        let mut events = Vec::new();
        let outcome = Session::new()
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &pair_config(),
                "make done",
                "",
                |e| {
                    events.push(e);
                },
            )
            .await
            .unwrap();

        assert!(matches!(outcome.verdict, Verdict::Passed { .. }));
        assert!(dir.path().join("README.md").exists());
        let seen = model.seen.lock().unwrap();
        // The review reads the changes, not the project.
        let Some(Message::User(review)) = seen[4].messages.last() else {
            panic!("the planner reviews");
        };
        assert!(review.contains("The checks pass. Review the work"));
        assert!(review.contains("done.txt") && review.contains("ok"));
        // The coder gets what the review asks for.
        assert!(matches!(
            seen[5].messages.last(),
            Some(Message::User(t)) if t.contains("1. README.md")
        ));
        let fixes = events
            .iter()
            .filter(|e| matches!(e, Event::Step { number: 6, .. }))
            .count();
        assert_eq!(fixes, 1);
    }

    #[test]
    fn only_a_plain_ok_approves() {
        assert!(approves("OK"));
        assert!(approves(" `ok`.\n"));
        assert!(!approves("OK, but README.md is missing the route."));
    }

    #[tokio::test]
    async fn the_planner_may_look_at_code_before_revising() {
        let (dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            says("none"),
            says("Create notes.txt."),
            calls("write_file", json!({"path": "notes.txt", "content": "a"})),
            says("Done."),
            calls("write_file", json!({"path": "notes.txt", "content": "b"})),
            says("Done again."),
            // Asked to revise, it wants to see what was written first.
            says("notes.txt:1-1"),
            says("1. Wrong file: create done.txt containing ok instead."),
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
            // The review approves it.
            says("OK"),
        ]);
        let outcome = Session::new()
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &pair_config(),
                "make done",
                "",
                |_| {},
            )
            .await
            .unwrap();

        assert!(matches!(outcome.verdict, Verdict::Passed { .. }));
        assert!(dir.path().join("done.txt").exists());
        let seen = model.seen.lock().unwrap();
        // ironquill read what it asked for, then it revised.
        assert_eq!(seen[7].model.as_str(), "strong");
        assert!(matches!(
            seen[7].messages.last(),
            Some(Message::User(t)) if t.contains("(lines 1 to 1 of 1)") && t.contains("Now revise")
        ));
        assert_eq!(seen[8].model.as_str(), "cheap");
    }

    #[tokio::test]
    async fn the_planner_revises_its_plan_when_the_checks_fail() {
        let (dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            says("none"),
            says("Create notes.txt."),
            // Two rounds that miss done.txt.
            calls("write_file", json!({"path": "notes.txt", "content": "a"})),
            says("Done."),
            calls("write_file", json!({"path": "notes.txt", "content": "b"})),
            says("Done again."),
            // The revision, then the fix.
            says("The check wants done.txt: create it."),
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
            // The review approves it.
            says("OK"),
        ]);
        let mut session = Session::new();
        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &pair_config(),
                "make done",
                "",
                |_| {},
            )
            .await
            .unwrap();

        assert!(matches!(outcome.verdict, Verdict::Passed { .. }));
        assert!(dir.path().join("done.txt").exists());
        let seen = model.seen.lock().unwrap();
        // The planner's third call follows its own thread, with the failure.
        let revision = &seen[6];
        assert_eq!(revision.model.as_str(), "strong");
        assert_eq!(revision.messages.len(), 6);
        assert!(matches!(
            revision.messages.last(),
            Some(Message::User(t)) if t.contains("test -f done.txt") && t.contains("Revise the plan")
        ));
    }

    #[tokio::test]
    async fn a_long_conversation_is_compacted_once_past_the_threshold() {
        let (dir, mut toolbox) = setup();
        for name in ["a", "b", "c", "d", "e", "f"] {
            std::fs::write(dir.path().join(format!("{name}.txt")), "word ".repeat(500)).unwrap();
        }
        let mut answers: Vec<ChatResponse> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|n| calls("read_file", json!({"path": format!("{n}.txt")})))
            .collect();
        answers.push(says("Read them all."));
        let model = Scripted::new(answers);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .compact_at(2_500)
            .build()
            .unwrap();
        let mut session = Session::new();
        let mut events = Vec::new();
        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "read all",
                "",
                |e| {
                    events.push(e);
                },
            )
            .await
            .unwrap();

        let compactions: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::Compacted { .. }))
            .collect();
        assert_eq!(compactions.len(), 1, "once, all at once: {compactions:?}");
        let seen = model.seen.lock().unwrap();
        let last = &seen.last().unwrap().messages;
        assert!(crate::context::tests::valid(last));
        let dropped = last
            .iter()
            .filter(|m| matches!(m, Message::Tool { content, .. } if content.starts_with("[Earlier result")))
            .count();
        assert!(dropped >= 1);
        // The latest results are kept whole.
        assert!(matches!(
            &last[last.len() - 1],
            Message::Tool { content, .. } if content.starts_with("word")
        ));
    }

    #[tokio::test]
    async fn a_call_likely_to_pass_the_budget_is_not_made() {
        let (dir, mut toolbox) = setup();
        // A large file makes the second call expensive before it is sent.
        std::fs::write(dir.path().join("big.txt"), "word ".repeat(40_000)).unwrap();
        let model = Scripted::new(vec![
            calls("read_file", json!({"path": "big.txt"})),
            says("Stopped: reading on would pass the budget. Go on?"),
        ])
        // $1 per million tokens in, nothing out: the 50k tokens of the
        // file make the next call about $0.05.
        .priced(1e-6, 0.0);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .budget(Usd(0.04))
            .build()
            .unwrap();
        let mut session = Session::new();
        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "read big.txt",
                "",
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.verdict, Verdict::OverBudget { budget: Usd(0.04) });
        // The call with the file was never made; what stayed under the
        // budget is what was spent.
        assert!(outcome.cost.0 <= 0.04);
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
    }

    #[tokio::test]
    async fn a_spent_budget_stops_the_work_and_the_model_explains() {
        let (_dir, mut toolbox) = setup();
        let costly = |r: ChatResponse| ChatResponse {
            cost: Some(Usd(0.06)),
            ..r
        };
        let model = Scripted::new(vec![
            costly(calls("read_file", json!({"path": "a.txt"}))),
            costly(calls("read_file", json!({"path": "b.txt"}))),
            says("I read two files and stopped. Go on?"),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .budget(Usd(0.10))
            .build()
            .unwrap();
        let mut session = Session::new();
        let mut events = Vec::new();

        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "look around",
                "",
                |e| events.push(e),
            )
            .await
            .unwrap();

        assert_eq!(outcome.verdict, Verdict::OverBudget { budget: Usd(0.10) });
        assert!(events.contains(&Event::OverBudget {
            spent: Usd(0.12),
            budget: Usd(0.10)
        }));
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        // The explanation is asked without tools, after every result.
        assert!(seen[2].tools.is_empty());
        assert!(matches!(
            seen[2].messages.last(),
            Some(Message::User(text)) if text.contains("budget of $0.10")
        ));
        assert!(crate::context::tests::valid(&seen[2].messages));
        assert_eq!(
            session.messages.last(),
            Some(&Message::Assistant {
                content: Some("I read two files and stopped. Go on?".into()),
                tool_calls: vec![],
            })
        );
    }

    #[tokio::test]
    async fn a_member_without_tools_only_answers() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls(
                "delegate",
                json!({"model": "tiny", "task": "Is 2 + 2 = 4?"}),
            ),
            says("Yes."),
            says("tiny says yes."),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .member(Member {
                tools: false,
                ..Member::new(ModelId::new("tiny").unwrap(), "")
            })
            .build()
            .unwrap();
        let mut session = Session::new();
        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "ask tiny",
                "",
                |_| {},
            )
            .await
            .unwrap();

        let seen = model.seen.lock().unwrap();
        let delegate = seen[0].tools.iter().find(|t| t.name == "delegate").unwrap();
        assert!(delegate.description.contains("It cannot use tools"));
        assert_eq!(seen[1].model.as_str(), "tiny");
        assert!(seen[1].tools.is_empty());
        assert!(matches!(
            seen[2].messages.last(),
            Some(Message::Tool { content, .. }) if content.starts_with("tiny reports:\nYes.")
        ));
    }

    #[tokio::test]
    async fn the_first_model_hands_a_task_to_the_team() {
        let (dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls(
                "delegate",
                json!({"model": "strong", "task": "create done.txt"}),
            ),
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created done.txt."),
            says("Done, by the strong model."),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .member(Member::new(ModelId::new("cheap").unwrap(), ""))
            .member(Member {
                about: "Good at hard changes.".into(),
                ..Member::new(ModelId::new("strong").unwrap(), "$3/M in")
            })
            .build()
            .unwrap();
        let mut session = Session::new();
        let mut events = Vec::new();

        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "make done.txt",
                "",
                |e| events.push(e),
            )
            .await
            .unwrap();

        assert_eq!(outcome.verdict, Verdict::Unchecked);
        assert!(dir.path().join("done.txt").exists());
        assert!(events.contains(&Event::Delegating {
            from: ModelId::new("cheap").unwrap(),
            to: ModelId::new("strong").unwrap(),
            task: "create done.txt".into(),
        }));
        let seen = model.seen.lock().unwrap();
        let delegate = seen[0].tools.iter().find(|t| t.name == "delegate").unwrap();
        // The team is offered without the model itself.
        assert_eq!(
            delegate.parameters["properties"]["model"]["enum"],
            json!(["strong"])
        );
        assert!(
            delegate
                .description
                .contains("- strong — $3/M in — Good at hard changes.\n")
        );
        // The member works from the task alone, and cannot hand it on.
        assert_eq!(seen[1].model.as_str(), "strong");
        assert_eq!(
            seen[1].messages.last(),
            Some(&Message::user("create done.txt"))
        );
        assert!(seen[1].tools.iter().all(|t| t.name != "delegate"));
        // Each knows which model it is; only the first knows of the team.
        let system = |i: usize| match &seen[i].messages[0] {
            Message::System(text) => text.clone(),
            other => panic!("expected instructions first, got {other:?}"),
        };
        assert!(system(0).contains("You are the model `cheap`"));
        assert!(system(0).contains("a team, `strong`,"));
        assert!(system(1).contains("You are the model `strong`"));
        assert!(!system(1).contains("team"));
        // What is stored stays as it was: the next model gets its own name.
        assert!(!format!("{:?}", session.messages[0]).contains("You are the model"));
        assert!(matches!(
            seen[3].messages.last(),
            Some(Message::Tool { content, .. })
                if content == "strong reports:\nCreated done.txt.\n\nFiles it changed: done.txt"
        ));
    }

    #[tokio::test]
    async fn tests_a_model_writes_are_found_and_run() {
        let python = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| dir.join("python3").is_file())
        });
        if !python {
            return;
        }
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls(
                "write_file",
                json!({"path": "calc.py", "content": "def add(a, b):\n    return a - b\n"}),
            ),
            calls(
                "write_file",
                json!({
                    "path": "tests/test_calc.py",
                    "content": "import unittest\nfrom calc import add\n\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n"
                }),
            ),
            says("Added add and its test."),
            // The test the model wrote fails: it hears so and fixes the code.
            calls(
                "replace",
                json!({"path": "calc.py", "old": "a - b", "new": "a + b"}),
            ),
            says("Fixed."),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .detect_checks(true)
            .build()
            .unwrap();
        let mut events = Vec::new();
        let outcome = Session::new()
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "add add()",
                "",
                |e| {
                    events.push(e);
                },
            )
            .await
            .unwrap();

        assert!(
            matches!(outcome.verdict, Verdict::Passed { .. }),
            "{:?}",
            outcome.verdict
        );
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Checking { commands } if commands == &["python3 -B -m unittest discover -s tests"]
        )));
        assert!(events.iter().any(|e| matches!(e, Event::Failed { .. })));
    }

    #[tokio::test]
    async fn every_model_gets_the_persons_and_the_projects_instructions() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("Hi.")]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap()
            .with_instructions(Some("Answer in French.".into()))
            .with_project_rules(Some("## CLAUDE.md\nUse tabs.".into()));
        Session::new()
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                "hello",
                "",
                |_| {},
            )
            .await
            .unwrap();
        let seen = model.seen.lock().unwrap();
        let Message::System(system) = &seen[0].messages[0] else {
            panic!("instructions come first");
        };
        assert!(system.contains("The project's own instructions") && system.contains("Use tabs."));
        assert!(
            system.contains("The person's own instructions")
                && system.contains("Answer in French.")
        );
        // Blank instructions add nothing.
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap()
            .with_instructions(Some("  \n".into()));
        assert_eq!(config.instructions, None);
    }

    #[test]
    fn stop_is_read_as_the_first_word_only() {
        for reply in [
            "STOP",
            "STOP: pytest is not installed.",
            "**Stop** — nothing to change.",
            "\n`STOP`\nThe checks fail outside the code.",
            "stop. The code already does it.",
        ] {
            assert!(stops(reply), "{reply}");
        }
        for reply in [
            "Stopwatch first: add it.",
            "1. Stop the server",
            "Do not stop",
        ] {
            assert!(!stops(reply), "{reply}");
        }
    }

    #[test]
    fn a_long_catch_up_keeps_its_latest_part() {
        let messages: Vec<Message> = (0..2_000)
            .map(|i| Message::user(format!("request {i} é")))
            .collect();
        let text = catch_up(&messages);
        assert!(text.len() < CATCH_UP_BYTES + 100);
        assert!(text.starts_with("(the earlier part of the conversation is left out)\nUser: "));
        assert!(text.ends_with("User: request 1999 é\n\n"));
    }

    fn checks_run(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Checking { .. }))
            .count()
    }

    #[tokio::test]
    async fn the_checks_are_not_run_again_when_nothing_changed() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            calls("write_file", json!({"path": "notes.txt", "content": "a"})),
            says("Done."),
            says("I see no way to fix it."),
        ]);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .check(Check::parse("test -f done.txt").unwrap())
            .rounds_per_tier(3)
            .build()
            .unwrap();
        let mut events = Vec::new();
        let outcome = run(&model, &NoDelegate, &mut toolbox, &config, "t", "", |e| {
            events.push(e)
        })
        .await
        .unwrap();

        assert!(matches!(outcome.verdict, Verdict::GaveUp { .. }));
        assert_eq!(checks_run(&events), 1);
        assert_eq!(model.seen.lock().unwrap().len(), 3);
    }

    async fn in_pair(model: &Scripted, session: &mut Session, toolbox: &mut Toolbox) -> Outcome {
        session
            .send(
                model,
                &NoDelegate,
                toolbox,
                &pair_config(),
                "make done",
                "",
                |_| {},
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_planner_may_stop_before_any_code() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("STOP: pytest is not installed here.")]);
        let mut session = Session::new();
        let outcome = in_pair(&model, &mut session, &mut toolbox).await;

        assert!(matches!(outcome.verdict, Verdict::Answered));
        assert_eq!(model.seen.lock().unwrap().len(), 1);
        assert!(matches!(
            session.messages.last(),
            Some(Message::Assistant { content: Some(t), .. })
                if t.contains("stopped before any code: STOP: pytest")
        ));
        // It was told it may.
        let seen = model.seen.lock().unwrap();
        assert!(matches!(&seen[0].messages[1], Message::User(t) if t.contains(STOP_RULE)));
    }

    #[tokio::test]
    async fn the_planner_may_stop_instead_of_revising() {
        let (dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            says("none"),
            says("Create notes.txt."),
            calls("write_file", json!({"path": "notes.txt", "content": "a"})),
            says("Done."),
            calls("write_file", json!({"path": "notes.txt", "content": "b"})),
            says("Done again."),
            says("STOP\nThe check wants a file no plan of mine can make."),
        ]);
        let mut session = Session::new();
        let outcome = in_pair(&model, &mut session, &mut toolbox).await;

        assert!(matches!(outcome.verdict, Verdict::GaveUp { .. }));
        assert!(!dir.path().join("done.txt").exists());
        assert_eq!(model.seen.lock().unwrap().len(), 7);
        assert!(matches!(
            session.messages.last(),
            Some(Message::Assistant { content: Some(t), .. }) if t.contains("strong stopped: STOP")
        ));
    }

    #[tokio::test]
    async fn a_pair_ends_when_the_coder_changes_nothing() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![
            says("none"),
            says("Create done.txt."),
            says("It is already there."),
            says("Still nothing to do."),
        ]);
        let mut session = Session::new();
        let mut events = Vec::new();
        let outcome = session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &pair_config(),
                "make done",
                "",
                |e| events.push(e),
            )
            .await
            .unwrap();

        // No revised plan, no second check: nothing changed to judge.
        assert!(matches!(outcome.verdict, Verdict::Answered));
        assert_eq!(model.seen.lock().unwrap().len(), 4);
        assert_eq!(checks_run(&events), 1);
        assert!(matches!(
            session.messages.last(),
            Some(Message::Assistant { content: Some(t), .. }) if t.contains("cheap made no change")
        ));
    }

    /// An agent that only answers, from a script, as a planner does.
    struct PlanningAgent {
        replies: Mutex<VecDeque<&'static str>>,
        requests: Mutex<Vec<DelegateRequest>>,
    }

    impl PlanningAgent {
        fn new(replies: Vec<&'static str>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl Delegate for PlanningAgent {
        type Error = Infallible;

        async fn run(
            &self,
            request: &DelegateRequest,
            _: &mut (dyn FnMut(DelegateEvent) + Send),
        ) -> Result<DelegateReply, Infallible> {
            let round = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.clone());
                requests.len()
            };
            Ok(DelegateReply {
                text: self.replies.lock().unwrap().pop_front().unwrap().into(),
                session: format!("plan-{round}"),
                usage: Usage::default(),
                estimate: None,
                context: None,
                billed: false,
            })
        }
    }

    fn agent_pair_config() -> AgentConfig {
        AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .member(Member::new(ModelId::new("claude-code/opus").unwrap(), ""))
            .check(Check::parse("test -f done.txt").unwrap())
            .pair(Pair {
                planner: ModelId::new("claude-code/opus").unwrap(),
                planner_effort: Effort::Max,
                coder_effort: Effort::Low,
            })
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn an_agent_plans_in_one_read_only_session_that_follows_the_conversation() {
        let (_dir, mut toolbox) = setup();
        let planner = PlanningAgent::new(vec![
            "none",
            "Create done.txt.",
            "OK",
            "none",
            "Nothing else: STOP would be wrong, so create done.txt again.",
            "OK",
            "none",
            "Write done.txt.",
            "OK",
        ]);
        let model = Scripted::new(vec![
            calls("write_file", json!({"path": "done.txt", "content": "ok"})),
            says("Created."),
            calls("write_file", json!({"path": "done.txt", "content": "ok2"})),
            says("Rewritten."),
            calls("write_file", json!({"path": "done.txt", "content": "ok3"})),
            says("Written."),
        ]);
        let mut session = Session::new();
        let config = agent_pair_config();
        let mut send = async |session: &mut Session, text: &str| {
            session
                .send(&model, &planner, &mut toolbox, &config, text, "", |_| {})
                .await
                .unwrap()
        };

        send(&mut session, "make done").await;
        send(&mut session, "and again").await;
        // Unused for a while: its cache is gone, a new session is cheaper.
        session.agents.get_mut(&Agent::ClaudeCode).unwrap().used = 0;
        send(&mut session, "once more").await;

        let requests = planner.requests.lock().unwrap();
        assert!(requests.iter().all(|r| r.read_only));
        assert!(requests[0].instructions.contains(AGENT_PLAN_PROMPT));
        let resumes: Vec<Option<&str>> = requests.iter().map(|r| r.resume.as_deref()).collect();
        assert_eq!(
            resumes,
            [
                None,
                Some("plan-1"),
                Some("plan-2"),
                // Warm: the same session goes on.
                Some("plan-3"),
                Some("plan-4"),
                Some("plan-5"),
                // Cold: a new one, told the conversation so far.
                None,
                Some("plan-7"),
                Some("plan-8"),
            ]
        );
        assert!(!requests[0].prompt.contains("The conversation so far"));
        // Warm, it knows what was said: nothing is repeated.
        assert!(!requests[3].prompt.contains("The conversation so far"));
        assert!(
            requests[6]
                .prompt
                .starts_with("The conversation so far:\nUser: make done")
        );
        assert!(requests[6].prompt.contains("User: and again"));
    }

    #[tokio::test]
    async fn a_summary_tells_a_new_agent_session_what_was_said() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![says("The person wants done.txt; Claude created it.")])
            .priced(1e-6, 4e-6);
        let config = AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .member(Member::new(ModelId::new("cheap").unwrap(), ""))
            .build()
            .unwrap();
        let long = format!("make done.txt {}", "with care ".repeat(1_000));
        let mut session = Session::new();
        let mut events = Vec::new();
        let first = session
            .send(&model, &claude, &mut toolbox, &config, &long, "", |e| {
                events.push(e)
            })
            .await
            .unwrap();

        // The cheapest priced model wrote it, at a low effort, and it counts.
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Step { name, model: Some(m), effort: Some(Effort::Low), .. }
                if name.starts_with("Summarizing") && m.as_str() == "cheap"
        )));
        assert!((first.cost.0 - 0.001).abs() < 1e-12);
        assert!(matches!(
            &model.seen.lock().unwrap()[0].messages[1],
            Message::User(t) if t.contains("(none yet)") && t.contains("with care")
        ));

        // Its session gone cold, Claude starts a new one from the summary.
        session.agents.get_mut(&Agent::ClaudeCode).unwrap().used = 0;
        session
            .send(&model, &claude, &mut toolbox, &config, "next", "", |_| {})
            .await
            .unwrap();
        let requests = claude.requests.lock().unwrap();
        assert_eq!(requests[1].resume, None);
        assert!(requests[1].prompt.contains(
            "A summary of the conversation so far:\nThe person wants done.txt; Claude created it."
        ));
        assert!(!requests[1].prompt.contains("with care"));
        // Little was said since: it is not written again.
        assert_eq!(model.seen.lock().unwrap().len(), 1);

        // An edited conversation may no longer say what it says.
        session.apply_text(&session.to_text()).unwrap();
        assert!(session.summary.is_none());
    }

    #[tokio::test]
    async fn no_summary_is_written_when_nobody_would_start_from_it() {
        let (_dir, mut toolbox) = setup();
        let model = Scripted::new(vec![says("Sure.")]).priced(1e-6, 4e-6);
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap();
        let long = "explain ".repeat(2_000);
        let mut session = Session::new();
        session
            .send(
                &model,
                &NoDelegate,
                &mut toolbox,
                &config,
                &long,
                "",
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(model.seen.lock().unwrap().len(), 1);
        assert!(session.summary.is_none());
    }

    /// A long chat with `model` alone, then a request to Claude Code, which
    /// starts a new session; returns what Claude was sent.
    async fn long_chat_then_claude(model: &Scripted) -> String {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let alone = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap();
        let with_claude = AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .member(Member::new(ModelId::new("cheap").unwrap(), ""))
            .build()
            .unwrap();
        let mut session = Session::new();
        let first = format!("The topic is the parser. {}", "Some detail. ".repeat(3_000));
        session
            .send(model, &claude, &mut toolbox, &alone, &first, "", |_| {})
            .await
            .unwrap();
        // Nobody would have started from a summary: none was written.
        assert!(session.summary.is_none());
        session
            .send(
                model,
                &claude,
                &mut toolbox,
                &with_claude,
                "now fix it",
                "",
                |_| {},
            )
            .await
            .unwrap();
        claude.requests.lock().unwrap()[0].prompt.clone()
    }

    #[tokio::test]
    async fn a_conversation_too_long_to_send_is_summarized_before_a_new_session() {
        let model = Scripted::new(vec![says("Sure."), says("The person works on the parser.")])
            .priced(1e-6, 4e-6);
        let prompt = long_chat_then_claude(&model).await;
        assert!(
            prompt
                .contains("A summary of the conversation so far:\nThe person works on the parser.")
        );
        assert!(!prompt.contains("Some detail."));
        assert!(prompt.ends_with("now fix it"));
    }

    #[tokio::test]
    async fn without_a_summary_the_first_request_is_kept_with_the_latest_part() {
        // No price known: no model writes a summary.
        let model = Scripted::new(vec![says("Sure.")]);
        let prompt = long_chat_then_claude(&model).await;
        assert!(prompt.contains("The first request:\nThe topic is the parser."));
        assert!(prompt.contains("(the earlier part of the conversation is left out)"));
        assert!(prompt.contains("Assistant: Sure."));
        assert!(prompt.len() < CATCH_UP_BYTES + FIRST_REQUEST_BYTES + 500);
    }

    /// Claude Code on an API key, for the cost tests: the calls it reports
    /// as they end, its session's own total, and whether it was resumed.
    struct KeyedClaude {
        calls: Vec<Option<Usd>>,
        estimate: Option<Usd>,
    }

    impl Delegate for KeyedClaude {
        type Error = Infallible;

        async fn run(
            &self,
            _: &DelegateRequest,
            on_event: &mut (dyn FnMut(DelegateEvent) + Send),
        ) -> Result<DelegateReply, Infallible> {
            for cost in &self.calls {
                on_event(DelegateEvent::Usage {
                    usage: Usage {
                        input: TokenCount(10_000),
                        output: TokenCount(100),
                    },
                    cost: *cost,
                    billed: true,
                });
            }
            Ok(DelegateReply {
                text: "Done.".into(),
                session: "s".into(),
                usage: Usage {
                    input: TokenCount(900_000),
                    output: TokenCount(9_000),
                },
                estimate: self.estimate,
                context: None,
                billed: true,
            })
        }
    }

    async fn keyed(claude: &KeyedClaude, session: &mut Session) -> Outcome {
        let (_dir, mut toolbox) = setup();
        let config = AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .build()
            .unwrap();
        session
            .send(
                &Scripted::new(vec![]),
                claude,
                &mut toolbox,
                &config,
                "explain",
                "",
                |_| {},
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn claude_code_on_a_key_costs_what_its_messages_cost() {
        // Its own total covers the whole session, earlier requests
        // included: the messages counted as they came are what counts.
        let claude = KeyedClaude {
            calls: vec![Some(Usd(0.02)), Some(Usd(0.02))],
            estimate: Some(Usd(0.65)),
        };
        let mut session = Session::new();
        let first = keyed(&claude, &mut session).await;
        assert!((first.cost.0 - 0.04).abs() < 1e-12, "{:?}", first.cost);
        assert!(first.cost_complete && !first.subscription);
        assert_eq!(first.usage.output, TokenCount(200));
        // Resumed, the same: no earlier request billed again.
        let second = keyed(&claude, &mut session).await;
        assert!((second.cost.0 - 0.04).abs() < 1e-12, "{:?}", second.cost);
    }

    #[tokio::test]
    async fn what_could_not_be_priced_falls_back_on_a_new_sessions_total_only() {
        let claude = KeyedClaude {
            calls: vec![Some(Usd(0.02)), None],
            estimate: Some(Usd(0.05)),
        };
        let mut session = Session::new();
        // A new session's total covers this request alone: it fills the gap.
        let new = keyed(&claude, &mut session).await;
        assert!((new.cost.0 - 0.05).abs() < 1e-12, "{:?}", new.cost);
        assert!(new.cost_complete);
        // Resumed, its total cannot say: the cost is marked incomplete.
        let resumed = keyed(&claude, &mut session).await;
        assert!((resumed.cost.0 - 0.02).abs() < 1e-12, "{:?}", resumed.cost);
        assert!(!resumed.cost_complete);
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
    async fn a_delegated_task_hears_what_was_said_before_and_is_checked() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![says("Hi.")]);
        let mut session = Session::new();
        let mut events = Vec::new();

        // A first exchange with the chat model, which Claude is told about.
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
        assert_eq!(
            requests[0].prompt,
            "Meanwhile the conversation went on with another assistant:\n\n\
             User: hello\n\nAssistant: Hi.\n\n\n\
             The request now:\n\ncreate done.txt"
        );
        assert!(!requests[0].prompt.contains("FILES"));
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
    async fn claudes_session_lasts_across_other_models_and_restarts() {
        let (_dir, mut toolbox) = setup();
        let claude = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(vec![says("Hi from the chat model.")]);
        let mut session = Session::new();
        // No check, so each request is one round.
        let only = |model: &str| {
            AgentConfig::builder()
                .tier(ModelId::new(model).unwrap())
                .build()
                .unwrap()
        };
        let mut send = async |session: &mut Session, model_id: &str, text: &str| {
            session
                .send(
                    &model,
                    &claude,
                    &mut toolbox,
                    &only(model_id),
                    text,
                    "",
                    |_| {},
                )
                .await
                .unwrap();
        };

        send(&mut session, "claude-code/opus", "first").await;
        send(&mut session, "claude-code/sonnet", "second").await;
        send(&mut session, "cheap", "elsewhere").await;
        // As `ironquill -c` does after a restart.
        let mut session: Session =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        send(&mut session, "claude-code/opus", "third").await;

        let requests = claude.requests.lock().unwrap();
        let resumes: Vec<Option<&str>> = requests.iter().map(|r| r.resume.as_deref()).collect();
        assert_eq!(
            resumes,
            [None, Some("claude-session-1"), Some("claude-session-2")]
        );
        assert_eq!(requests[1].prompt, "second");
        // Only what Claude missed is repeated to it.
        assert_eq!(
            requests[2].prompt,
            "Meanwhile the conversation went on with another assistant:\n\n\
             User: elsewhere\n\nAssistant: Hi from the chat model.\n\n\n\
             The request now:\n\nthird"
        );

        session.forget_delegate(Agent::ClaudeCode);
        assert_eq!(session.delegate_session(Agent::ClaudeCode), None);
    }

    #[tokio::test]
    async fn each_agent_keeps_its_own_session() {
        let (_dir, mut toolbox) = setup();
        let agents = FakeClaude {
            writes_on_round: 1,
            requests: Mutex::new(Vec::new()),
        };
        let model = Scripted::new(Vec::new());
        let mut session = Session::new();
        for (id, text) in [
            ("claude-code", "first"),
            ("codex/gpt-5.5", "second"),
            ("claude-code", "third"),
        ] {
            let config = AgentConfig::builder()
                .tier(ModelId::new(id).unwrap())
                .build()
                .unwrap();
            session
                .send(&model, &agents, &mut toolbox, &config, text, "", |_| {})
                .await
                .unwrap();
        }

        let requests = agents.requests.lock().unwrap();
        let sent: Vec<(Agent, &str, Option<&str>)> = requests
            .iter()
            .map(|r| (r.agent, r.model.as_str(), r.resume.as_deref()))
            .collect();
        assert_eq!(
            sent,
            [
                (Agent::ClaudeCode, "", None),
                (Agent::Codex, "gpt-5.5", None),
                (Agent::ClaudeCode, "", Some("claude-session-1")),
            ]
        );
        assert!(requests[1].instructions.contains("Use commands"));
        assert!(
            requests[2]
                .prompt
                .contains("User: second\n\nAssistant: Report 2")
        );
        assert_eq!(
            session.delegate_session(Agent::Codex),
            Some("claude-session-2")
        );
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
