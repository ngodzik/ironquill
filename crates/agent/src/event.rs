use ironquill_core::{ContextUse, ModelId, TokenCount, Usage, Usd};
use ironquill_tools::ToolSummary;

/// Something that happened during a session, for whoever is watching.
///
/// The loop reports and never prints; the command line, and later the TUI,
/// decide what to show.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A model answered one turn.
    Turn {
        /// Which model.
        model: ModelId,
        /// Tokens for this turn alone.
        usage: Usage,
        /// Cost for this turn alone, if the provider reported it.
        cost: Option<Usd>,
        /// Whether the turn ran on a subscription (Claude Code), so that no
        /// cost is owed for it and none should be expected.
        subscription: bool,
        /// How full the model's context was, when its window is known.
        context: Option<ContextUse>,
    },
    /// Text as it is being written, by an agent that reports it in pieces.
    Saying {
        /// Which model or agent.
        model: ModelId,
        /// The next piece of text.
        text: String,
        /// Whether this piece starts a new block of text.
        new_block: bool,
    },
    /// A model wrote text: an explanation, or its closing summary.
    Said {
        /// Which model.
        model: ModelId,
        /// What it wrote.
        text: String,
    },
    /// A tool call was executed.
    Tool {
        /// The tool.
        name: String,
        /// The path it acted on, when it has one.
        path: Option<String>,
        /// What it did, or the error reported back to the model.
        outcome: Result<ToolSummary, String>,
    },
    /// The checks are about to run, in this order.
    Checking {
        /// The command lines.
        commands: Vec<String>,
    },
    /// The checks passed.
    Passed,
    /// A check failed.
    Failed {
        /// The command that failed.
        command: String,
        /// The part of its output worth reading.
        excerpt: String,
    },
    /// The next model takes over.
    Escalating {
        /// The model that gave up.
        from: ModelId,
        /// The model taking over.
        to: ModelId,
    },
    /// The model hands one task to another of the team.
    Delegating {
        /// The model handing it over.
        from: ModelId,
        /// The model doing it.
        to: ModelId,
        /// The task, as the first model wrote it.
        task: String,
    },
    /// Old tool results were dropped from the conversation to resend less.
    Compacted {
        /// Results dropped.
        dropped: usize,
        /// About how many tokens the conversation held before.
        before: TokenCount,
        /// And after.
        after: TokenCount,
    },
    /// The request spent its budget: the work stops here.
    OverBudget {
        /// What the request cost so far.
        spent: Usd,
        /// The budget it had.
        budget: Usd,
    },
}
