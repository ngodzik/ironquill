//! What the agent touched, read from the conversation, for the codebase
//! views to light up.

use ironquill_tools::ToolSummary;
use ironquill_ui::Entry;

/// The files the tool calls of `entries` read or edited, in order, with
/// whether each was edited. A sub-agent's calls count as the agent's.
pub(crate) fn touched(entries: &[Entry]) -> Vec<(&str, bool)> {
    entries
        .iter()
        .filter_map(|entry| {
            let entry = match entry {
                Entry::Member { entry, .. } => entry,
                entry => entry,
            };
            let Entry::Tool { path, outcome, .. } = entry else {
                return None;
            };
            match outcome {
                Ok(ToolSummary::Changed { path, .. }) => Some((path.as_str(), true)),
                Ok(ToolSummary::Read { path, .. }) => Some((path.as_str(), false)),
                _ => path.as_deref().map(|path| (path, false)),
            }
        })
        .collect()
}
