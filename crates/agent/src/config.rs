use ironquill_core::ModelId;
use ironquill_tools::Check;

use crate::error::AgentError;

/// How a session escalates and what judges it. Built with [`AgentConfig::builder`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub(crate) tiers: Vec<ModelId>,
    pub(crate) rounds_per_tier: u32,
    pub(crate) max_turns: u32,
    pub(crate) checks: Vec<Check>,
}

impl AgentConfig {
    /// Starts a configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use ironquill_agent::AgentConfig;
    /// use ironquill_core::ModelId;
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
        }
    }
}

/// Collects an [`AgentConfig`] and validates it at [`build`](Self::build).
#[derive(Debug, Clone)]
pub struct AgentConfigBuilder {
    tiers: Vec<ModelId>,
    rounds_per_tier: u32,
    max_turns: u32,
    checks: Vec<Check>,
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

    /// Validates and builds.
    ///
    /// # Errors
    ///
    /// [`AgentError::Config`] without a model, without a check, or with a
    /// zero round or turn budget.
    pub fn build(self) -> Result<AgentConfig, AgentError> {
        if self.tiers.is_empty() {
            return Err(AgentError::Config("at least one model is needed"));
        }
        // Without a check nothing judges the change, and the whole point of
        // the loop is that the model's own opinion of its work is not enough.
        if self.checks.is_empty() {
            return Err(AgentError::Config("at least one check is needed"));
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
        })
    }
}
