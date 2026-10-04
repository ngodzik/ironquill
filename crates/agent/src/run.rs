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
for. Write in the language of the request.";

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
}

/// A delegate's session and how much of the conversation it has seen.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Thread {
    session: String,
    /// The number of messages of the conversation the delegate knows about.
    seen: usize,
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
        Ok(ctx.ledger.outcome(verdict, ctx.toolbox))
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
            let thread = self.agents.get(&agent);
            let seen = thread.map_or(1, |t| t.seen);
            ctx.thread = thread.map(|t| t.session.clone());
            let request = self.messages.len() - 1;
            ctx.catch_up = catch_up(&self.messages[seen.min(request)..request]);
        }
        let first_attempt = attempt(ctx, first, &mut self.messages, &mut failure, true).await;
        if let (Some(agent), Some(session)) = (agent, ctx.thread.take()) {
            let seen = self.messages.len();
            self.agents.insert(agent, Thread { session, seen });
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
                "The request:\n{text}\n\nThe map of the project, its definitions with their \
                 lines:\n{map}\n\n{context}\n\n{SCOUT_PROMPT}{}",
                spent_so_far(ctx)
            )),
        ];
        let wanted = ask_planner(ctx, planner, pair.planner_effort, &mut planning).await?;

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
        let mut plan = ask_planner(ctx, planner, pair.planner_effort, &mut planning).await?;

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
        let result: Result<Verdict, AgentError> = 'work: {
            for revision in 0..=MAX_REVISIONS {
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
                if revision == MAX_REVISIONS {
                    break;
                }
                let Some(f) = failure.as_ref() else { break };
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
                     before, and ironquill will read them for you.{}",
                    describe(f),
                    spent_so_far(ctx)
                )));
                plan = match ask_planner(ctx, planner, pair.planner_effort, &mut planning).await {
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
                    plan = match ask_planner(ctx, planner, pair.planner_effort, &mut planning).await
                    {
                        Ok(plan) => plan,
                        Err(e) => break 'work Err(e),
                    };
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
                let review =
                    match ask_planner(ctx, planner, pair.planner_effort, &mut planning).await {
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
            "(Worked in a pair: {planner} planned, {coder} implemented.{ending}{reviewed}\n\nPlan:\n{plan}\n\n\
             {coder}: {report}\nFiles changed: {})",
            if changed.is_empty() { "none" } else { &changed }
        ));
        result
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
        // The edited system message holds whatever context the person kept.
        self.context_added = true;
        Ok(())
    }

    /// A rough size of what the next request will send, in tokens.
    pub fn approx_tokens(&self) -> u64 {
        crate::context::approx_tokens(&self.messages)
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
        match check(ctx).await? {
            None => return Ok(Attempt::Passed),
            Some(f) => *failure = Some(f),
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
    // What was counted as it came, to add only the rest at the end.
    let mut live_usage = Usage::default();
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
                    live_usage += usage;
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

    // The agent's own total settles it: what the messages did not count.
    let rest = Usage {
        input: TokenCount(reply.usage.input.0.saturating_sub(live_usage.input.0)),
        output: TokenCount(reply.usage.output.0.saturating_sub(live_usage.output.0)),
    };
    let cost = if reply.billed {
        let owed = reply.estimate.map_or(live_cost.0, |e| e.0.max(live_cost.0));
        Some(Usd(owed - live_cost.0))
    } else {
        ctx.ledger.subscription = true;
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
) -> Result<String, AgentError> {
    ctx.effort = Some(effort);
    if let Some((agent, model)) = planner.delegate() {
        let prompt = match planning.last() {
            Some(Message::User(text)) => text.clone(),
            _ => String::new(),
        };
        let request = DelegateRequest {
            agent,
            effort: ctx.effort,
            model: model.to_owned(),
            prompt,
            instructions: agent_instructions(PLAN_PROMPT, ctx.config),
            resume: None,
            directory: ctx.toolbox.workspace().root().to_owned(),
        };
        let text = run_agent(ctx, planner, &request).await?.text;
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

    /// Claude Code on an API key: two messages counted as they end, then
    /// its own total.
    struct BilledClaude;

    impl Delegate for BilledClaude {
        type Error = Infallible;

        async fn run(
            &self,
            _: &DelegateRequest,
            on_event: &mut (dyn FnMut(DelegateEvent) + Send),
        ) -> Result<DelegateReply, Infallible> {
            for _ in 0..2 {
                on_event(DelegateEvent::Usage {
                    usage: Usage {
                        input: TokenCount(10_000),
                        output: TokenCount(100),
                    },
                    cost: Some(Usd(0.02)),
                    billed: true,
                });
            }
            Ok(DelegateReply {
                text: "Done.".into(),
                session: "s".into(),
                usage: Usage {
                    input: TokenCount(20_000),
                    output: TokenCount(250),
                },
                estimate: Some(Usd(0.05)),
                context: None,
                billed: true,
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

    #[tokio::test]
    async fn claude_code_on_a_key_costs_as_it_goes_and_settles_at_the_end() {
        let (_dir, mut toolbox) = setup();
        let config = AgentConfig::builder()
            .tier(ModelId::new("claude-code/opus").unwrap())
            .build()
            .unwrap();
        let mut costs = Vec::new();
        let outcome = Session::new()
            .send(
                &Scripted::new(vec![]),
                &BilledClaude,
                &mut toolbox,
                &config,
                "explain",
                "",
                |e| {
                    if let Event::Turn {
                        cost, subscription, ..
                    } = e
                    {
                        costs.push((cost, subscription));
                    }
                },
            )
            .await
            .unwrap();

        // Two messages as they ended, then the rest of its own total.
        assert_eq!(costs.len(), 3);
        assert!(costs.iter().all(|(_, subscription)| !subscription));
        assert_eq!(costs[0].0, Some(Usd(0.02)));
        assert!((costs[2].0.unwrap().0 - 0.01).abs() < 1e-12);
        // Owed, counted once, in full.
        assert!((outcome.cost.0 - 0.05).abs() < 1e-12);
        assert_eq!(outcome.usage.output, TokenCount(250));
        assert!(!outcome.subscription);
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
