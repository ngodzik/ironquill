use ironquill_core::{ModelId, Usd};
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
}

impl Member {
    /// A member that calls tools, known by `note` only.
    pub fn new(model: ModelId, note: impl Into<String>) -> Self {
        Self {
            model,
            note: note.into(),
            about: String::new(),
            tools: true,
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
        }
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
        })
    }
}
