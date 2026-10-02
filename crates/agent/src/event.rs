use ironquill_core::{ModelId, Usage, Usd};
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
}
