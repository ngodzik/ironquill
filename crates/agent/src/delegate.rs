//! What a delegated agent's tool calls mean in ironquill's terms.

use std::path::Path;

use ironquill_tools::{DiffLine, ToolSummary, line_diff};
use serde_json::Value;

/// A delegated tool call as ironquill shows and counts it.
pub(crate) struct ToolReport {
    /// The file it acted on, relative to the project when inside it.
    pub(crate) path: Option<String>,
    pub(crate) outcome: Result<ToolSummary, String>,
    /// The file it changed, to be checked.
    pub(crate) changed: Option<String>,
}

/// Translates one of an agent's tool calls: `Read`, `Edit`, `MultiEdit`,
/// `Write` and `Delete` map onto ironquill's own summaries, any other tool is
/// shown by name with what it was given. Codex's changes come in these
/// names too.
pub(crate) fn report(
    name: &str,
    input: &Value,
    output: &Result<String, String>,
    root: &Path,
) -> ToolReport {
    let path = ["file_path", "notebook_path", "path"]
        .iter()
        .find_map(|key| input[*key].as_str())
        .map(|p| relative(p, root));
    let text = match output {
        Ok(text) => text,
        Err(error) => {
            return ToolReport {
                path,
                outcome: Err(error.clone()),
                changed: None,
            };
        }
    };
    let field = |key: &str| input[key].as_str().unwrap_or_default();
    let changed = |path: &Option<String>, created: bool, diff: Vec<DiffLine>| ToolReport {
        path: path.clone(),
        changed: path.clone(),
        outcome: Ok(ToolSummary::Changed {
            path: path.clone().unwrap_or_default(),
            created,
            diff,
        }),
    };
    match name {
        "Read" => ToolReport {
            outcome: Ok(ToolSummary::Read {
                path: path.clone().unwrap_or_default(),
                lines: text.lines().count(),
            }),
            path,
            changed: None,
        },
        "Edit" => changed(
            &path,
            false,
            line_diff(field("old_string"), field("new_string")),
        ),
        "MultiEdit" => {
            let diff = input["edits"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|edit| {
                    line_diff(
                        edit["old_string"].as_str().unwrap_or_default(),
                        edit["new_string"].as_str().unwrap_or_default(),
                    )
                })
                .collect();
            changed(&path, false, diff)
        }
        "Write" => {
            // Claude Code says "File created" for a new file and "updated"
            // for one it overwrote.
            let created = text.to_lowercase().contains("created");
            changed(&path, created, line_diff("", field("content")))
        }
        // Codex deletes files; Claude Code has no such tool.
        "Delete" => ToolReport {
            outcome: Ok(ToolSummary::Ran {
                label: format!("Delete({})", path.clone().unwrap_or_default()),
                lines: 0,
            }),
            changed: path.clone(),
            path,
        },
        _ => {
            let argument = ["pattern", "path", "query", "url", "command", "description"]
                .iter()
                .find_map(|key| input[*key].as_str())
                .unwrap_or_default();
            ToolReport {
                outcome: Ok(ToolSummary::Ran {
                    label: format!("{name}({argument})"),
                    lines: text.lines().count(),
                }),
                path,
                changed: None,
            }
        }
    }
}

fn relative(path: &str, root: &Path) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn edits_become_changes_with_their_diff_and_a_relative_path() {
        let report = report(
            "Edit",
            &json!({"file_path": "/p/src/a.rs", "old_string": "x = 1", "new_string": "x = 2"}),
            &Ok("ok".into()),
            Path::new("/p"),
        );
        assert_eq!(report.changed.as_deref(), Some("src/a.rs"));
        assert!(matches!(
            report.outcome,
            Ok(ToolSummary::Changed { ref path, created: false, ref diff }) if path == "src/a.rs" && diff.len() == 2
        ));
    }

    #[test]
    fn a_new_file_written_is_created() {
        let report = report(
            "Write",
            &json!({"file_path": "/p/b.py", "content": "print(1)\n"}),
            &Ok("File created successfully at: /p/b.py".into()),
            Path::new("/p"),
        );
        assert!(matches!(
            report.outcome,
            Ok(ToolSummary::Changed { created: true, .. })
        ));
    }

    #[test]
    fn other_tools_are_shown_by_name_and_change_nothing() {
        let report = report(
            "Grep",
            &json!({"pattern": "fn main"}),
            &Ok("a.rs:1\nb.rs:2".into()),
            Path::new("/p"),
        );
        assert!(report.changed.is_none());
        assert!(matches!(
            report.outcome,
            Ok(ToolSummary::Ran { ref label, lines: 2 }) if label == "Grep(fn main)"
        ));
    }

    #[test]
    fn a_failed_call_is_an_error_and_changes_nothing() {
        let report = report(
            "Edit",
            &json!({"file_path": "/p/a.rs"}),
            &Err("old_string not found".into()),
            Path::new("/p"),
        );
        assert!(report.changed.is_none());
        assert_eq!(report.outcome, Err("old_string not found".into()));
    }
}
