use ironquill_core::{ContextUse, Effort, ModelId, TokenCount, Usage, Usd};
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
    /// A step of a request worked on in a pair begins, or one outside the
    /// numbered steps, such as the summary written at the end.
    Step {
        /// Its place, from 1; 0 outside the numbered steps.
        number: u8,
        /// How many steps there are; 0 outside the numbered steps.
        of: u8,
        /// What it does, such as `Planning`.
        name: String,
        /// The model doing it; `None` when ironquill does it itself.
        model: Option<ModelId>,
        /// How hard that model thinks.
        effort: Option<Effort>,
    },
    /// Something about how an agent works the person should know.
    Notice {
        /// The agent.
        model: ModelId,
        /// What.
        text: String,
    },
    /// An agent's safety checks refused one of its calls: the person may
    /// approve it in their next message.
    Denied {
        /// The agent.
        model: ModelId,
        /// What it wanted to do, such as the command.
        action: String,
        /// Why it was refused, when the agent says.
        reason: String,
    },
    /// A command a model wanted to run was put to the person, who said yes
    /// or no, or could not be asked.
    Held {
        /// The command.
        command: String,
        /// Why it was held.
        reasons: Vec<String>,
        /// Whether it ran.
        approved: bool,
    },
    /// A check was tried before any change, to see whether it can judge one.
    Tried {
        /// The command.
        command: String,
        /// What it did: passes, fails already, or cannot judge, and why.
        outcome: String,
    },
    /// A pair ended: who did what, how it ended, what judged it, and the
    /// files changed, in one line.
    PairEnded {
        /// That line.
        text: String,
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
