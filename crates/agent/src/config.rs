use ironquill_core::{Effort, ModelId, Usd};
use ironquill_tools::Check;

use crate::error::AgentError;

/// How a session escalates and what judges it. Built with [`AgentConfig::builder`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub(crate) tiers: Vec<ModelId>,
    pub(crate) rounds_per_tier: u32,
    pub(crate) max_turns: u32,
    pub(crate) checks: Vec<Check>,
    pub(crate) team: Vec<Member>,
    pub(crate) budget: Option<Usd>,
    pub(crate) compact_at: u64,
    pub(crate) effort: Option<Effort>,
    pub(crate) pair: Option<Pair>,
    pub(crate) detect_checks: bool,
    pub(crate) instructions: Option<String>,
    pub(crate) project_rules: Option<String>,
}

/// Planning by a stronger model, coding by the first one, as an architect
/// and an editor: the planner thinks hard on what the coder gathered, the
/// coder does the reading and the writing.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    /// The model that plans; it reads nothing itself.
    pub planner: ModelId,
    /// How hard it thinks.
    pub planner_effort: Effort,
    /// How hard the first model thinks while it gathers and codes.
    pub coder_effort: Effort,
}

/// A model the first one may hand a task to, with what it should know to
/// choose it, such as its price and what it is good at.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// The model, or an agent such as `claude-code/opus`.
    pub model: ModelId,
    /// A few words on it, shown to the person too: price, context.
    pub note: String,
    /// What it is good at, as its provider describes it, for the model
    /// choosing.
    pub about: String,
    /// Whether it can call tools. One that cannot reads and edits nothing:
    /// it only answers what the task itself says.
    pub tools: bool,
    /// Its intelligence index from Artificial Analysis, out of 100, when
    /// known: what picks a planner by default.
    pub score: Option<f64>,
    /// Dollars per input token, when known: what picks a planner when no
    /// score is.
    pub price: Option<f64>,
}

impl Member {
    /// A member that calls tools, known by `note` only.
    pub fn new(model: ModelId, note: impl Into<String>) -> Self {
        Self {
            model,
            note: note.into(),
            about: String::new(),
            tools: true,
            score: None,
            price: None,
        }
    }
}

impl AgentConfig {
    /// Starts a configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use ironquill_agent::AgentConfig;
    /// use ironquill_core::{ModelId, Usd};
    /// use ironquill_tools::Check;
    ///
    /// let config = AgentConfig::builder()
    ///     .tier(ModelId::new("cheap/model")?)
    ///     .tier(ModelId::new("strong/model")?)
    ///     .check(Check::parse("cargo check").unwrap())
    ///     .build()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn builder() -> AgentConfigBuilder {
        AgentConfigBuilder {
            tiers: Vec::new(),
            rounds_per_tier: 2,
            max_turns: 30,
            checks: Vec::new(),
            team: Vec::new(),
            budget: None,
            compact_at: COMPACT_AT,
            effort: Some(Effort::High),
            pair: None,
            detect_checks: false,
            instructions: None,
            project_rules: None,
        }
    }

    /// The project's own instructions for coding agents, from its
    /// CLAUDE.md, AGENTS.md and rules, read again before each request.
    pub fn with_project_rules(mut self, rules: Option<String>) -> Self {
        self.project_rules = rules.filter(|r| !r.trim().is_empty());
        self
    }

    /// The person's own instructions for every model, read again before each
    /// request so that an edit counts at once; `None` or blank adds none.
    pub fn with_instructions(mut self, instructions: Option<String>) -> Self {
        self.instructions = instructions.filter(|i| !i.trim().is_empty());
        self
    }
}

/// Tokens of conversation past which old tool results are dropped, unless
/// half the model's context is less.
pub const COMPACT_AT: u64 = 40_000;

/// Collects an [`AgentConfig`] and validates it at [`build`](Self::build).
#[derive(Debug, Clone)]
pub struct AgentConfigBuilder {
    tiers: Vec<ModelId>,
    rounds_per_tier: u32,
    max_turns: u32,
    checks: Vec<Check>,
    team: Vec<Member>,
    budget: Option<Usd>,
    compact_at: u64,
    effort: Option<Effort>,
    pair: Option<Pair>,
    detect_checks: bool,
    instructions: Option<String>,
    project_rules: Option<String>,
}

impl AgentConfigBuilder {
    /// Adds the next model to escalate to. The first one added is tried first,
    /// so add them from cheapest to strongest.
    pub fn tier(mut self, model: ModelId) -> Self {
        self.tiers.push(model);
        self
    }

    /// How many times one model may try to make the checks pass before the
    /// next one takes over. Defaults to 2.
    pub fn rounds_per_tier(mut self, rounds: u32) -> Self {
        self.rounds_per_tier = rounds;
        self
    }

    /// How many model turns one round may take before it counts as failed.
    /// Bounds the cost of a model that edits in circles. Defaults to 30.
    pub fn max_turns(mut self, turns: u32) -> Self {
        self.max_turns = turns;
        self
    }

    /// Adds a check, run after the ones already added.
    pub fn check(mut self, check: Check) -> Self {
        self.checks.push(check);
        self
    }

    /// Adds a model the first one may hand tasks to, through a `delegate`
    /// tool. Without any, it has no such tool.
    pub fn member(mut self, member: Member) -> Self {
        self.team.push(member);
        self
    }

    /// The most one request may cost. Once it is spent, the work stops and
    /// the first model explains where it is and asks what to do. Turns run
    /// on a subscription do not count.
    pub fn budget(mut self, budget: Usd) -> Self {
        self.budget = Some(budget);
        self
    }

    /// How long the conversation may grow, in tokens, before the results of
    /// old tool calls are dropped, all at once: the conversation is then
    /// resent shorter, and its new start is cached again. Defaults to
    /// [`COMPACT_AT`], or half the model's context when that is less.
    pub fn compact_at(mut self, tokens: u64) -> Self {
        self.compact_at = tokens;
        self
    }

    /// How hard models and agents should think. Defaults to
    /// [`Effort::High`]; `None` leaves it to each provider.
    pub fn effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    /// Works on the request in a pair: `pair.planner` plans, the
    /// first model gathers and codes. Escalation is not used then: when the
    /// checks keep failing, the planner revises its plan instead.
    pub fn pair(mut self, pair: Pair) -> Self {
        self.pair = Some(pair);
        self
    }

    /// Without checks added, finds the project's own each time they are
    /// needed: its test runner, and the tests a model has just written.
    pub fn detect_checks(mut self, detect: bool) -> Self {
        self.detect_checks = detect;
        self
    }

    /// The person's own instructions, given to every model with its own.
    pub fn instructions(mut self, instructions: Option<String>) -> Self {
        self.instructions = instructions.filter(|i| !i.trim().is_empty());
        self
    }

    /// The project's own instructions for coding agents.
    pub fn project_rules(mut self, rules: Option<String>) -> Self {
        self.project_rules = rules.filter(|r| !r.trim().is_empty());
        self
    }

    /// Validates and builds.
    ///
    /// # Errors
    ///
    /// [`AgentError::Config`] without a model, or with a zero round or turn
    /// budget. No check is allowed: changes are then kept as written, and
    /// the verdict says they were not checked.
    pub fn build(self) -> Result<AgentConfig, AgentError> {
        if self.tiers.is_empty() {
            return Err(AgentError::Config("at least one model is needed"));
        }
        if self.rounds_per_tier == 0 || self.max_turns == 0 {
            return Err(AgentError::Config(
                "round and turn budgets must be at least 1",
            ));
        }
        Ok(AgentConfig {
            tiers: self.tiers,
            rounds_per_tier: self.rounds_per_tier,
            max_turns: self.max_turns,
            checks: self.checks,
            team: self.team,
            budget: self.budget,
            compact_at: self.compact_at,
            effort: self.effort,
            pair: self.pair,
            detect_checks: self.detect_checks,
            instructions: self.instructions,
            project_rules: self.project_rules,
        })
    }
}
