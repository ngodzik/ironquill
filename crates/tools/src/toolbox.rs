use std::collections::BTreeSet;
use std::fs;

use ironquill_core::{ToolCall, ToolSpec};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::diff::{DiffLine, line_diff};
use crate::error::ToolError;
use crate::workspace::Workspace;

/// What a tool call did, in a form an interface can show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSummary {
    /// A file was read.
    Read {
        /// The file.
        path: String,
        /// How many lines it has.
        lines: usize,
    },
    /// A directory was listed.
    Listed {
        /// The directory.
        path: String,
        /// How many entries it holds.
        entries: usize,
    },
    /// A file was edited, or written over an earlier version.
    Changed {
        /// The file.
        path: String,
        /// Whether the file did not exist before.
        created: bool,
        /// The changed lines.
        diff: Vec<DiffLine>,
    },
}

/// The result of a successful tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// What the model is told.
    pub for_model: String,
    /// What the person is shown.
    pub summary: ToolSummary,
}

/// The tools a model may call, bound to one workspace.
///
/// There is deliberately no shell. A model that could run commands could also
/// run the checks itself, or edit around them; keeping execution on
/// ironquill's side is what makes the verdict of a check trustworthy.
#[derive(Debug)]
pub struct Toolbox {
    workspace: Workspace,
    changed: BTreeSet<String>,
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct ReplaceArgs {
    path: String,
    old: String,
    new: String,
}

impl Toolbox {
    /// A toolbox that reads and writes inside `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            changed: BTreeSet::new(),
        }
    }

    /// The workspace the tools are bound to.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Files written or edited so far, relative to the root, sorted.
    pub fn changed(&self) -> impl Iterator<Item = &str> {
        self.changed.iter().map(String::as_str)
    }

    /// The tools, described for the model.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let path = json!({"type": "string", "description": "Path relative to the project root."});
        vec![
            ToolSpec {
                name: "read_file".into(),
                description: "Read a whole text file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": path},
                    "required": ["path"],
                }),
            },
            ToolSpec {
                name: "list_dir".into(),
                description: "List one directory. Subdirectories end with a slash.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": path},
                    "required": ["path"],
                }),
            },
            ToolSpec {
                name: "replace".into(),
                description: "Replace one exact occurrence of `old` with `new` in a file. \
                    `old` must occur exactly once: include enough surrounding lines. \
                    Prefer this to write_file for any change to an existing file."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": path,
                        "old": {"type": "string"},
                        "new": {"type": "string"},
                    },
                    "required": ["path", "old", "new"],
                }),
            },
            ToolSpec {
                name: "write_file".into(),
                description: "Create a file, or overwrite one entirely.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": path, "content": {"type": "string"}},
                    "required": ["path", "content"],
                }),
            },
        ]
    }

    /// Runs one call and returns what to tell the model.
    ///
    /// # Errors
    ///
    /// Any [`ToolError`]. The agent passes it back to the model as text: a
    /// failed edit is information for the next turn, not a reason to stop.
    pub fn call(&mut self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        match call.name.as_str() {
            "read_file" => {
                let args: PathArgs = parse(call)?;
                let text = self.workspace.read(&args.path)?;
                Ok(ToolOutput {
                    summary: ToolSummary::Read {
                        lines: text.lines().count(),
                        path: args.path,
                    },
                    for_model: text,
                })
            }
            "list_dir" => {
                let args: PathArgs = parse(call)?;
                let listing = self.list(&args.path)?;
                Ok(ToolOutput {
                    summary: ToolSummary::Listed {
                        entries: listing.lines().count(),
                        path: args.path,
                    },
                    for_model: listing,
                })
            }
            "replace" => {
                let args: ReplaceArgs = parse(call)?;
                let before = self.workspace.read(&args.path)?;
                self.workspace.replace(&args.path, &args.old, &args.new)?;
                let after = self.workspace.read(&args.path)?;
                Ok(self.changed_output(args.path, false, &before, &after, "replaced"))
            }
            "write_file" => {
                let args: WriteArgs = parse(call)?;
                let before = self.workspace.read(&args.path).ok();
                self.workspace.write(&args.path, &args.content)?;
                let created = before.is_none();
                Ok(self.changed_output(
                    args.path,
                    created,
                    before.as_deref().unwrap_or(""),
                    &args.content,
                    "written",
                ))
            }
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }

    fn changed_output(
        &mut self,
        path: String,
        created: bool,
        before: &str,
        after: &str,
        for_model: &str,
    ) -> ToolOutput {
        self.changed.insert(path.clone());
        ToolOutput {
            for_model: for_model.into(),
            summary: ToolSummary::Changed {
                path,
                created,
                diff: line_diff(before, after),
            },
        }
    }

    /// Forgets which files were changed, so that the next calls are counted
    /// from here. A conversation does this before each request.
    pub fn reset_changes(&mut self) {
        self.changed.clear();
    }

    fn list(&self, relative: &str) -> Result<String, ToolError> {
        let path = self.workspace.resolve(relative)?;
        if path.is_file() {
            return Err(ToolError::NotADirectory(relative.into()));
        }
        let io = |source| ToolError::Io {
            path: relative.into(),
            source,
        };
        let mut names = Vec::new();
        for entry in fs::read_dir(&path).map_err(io)? {
            let entry = entry.map_err(io)?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().map_err(io)?.is_dir() {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        Ok(names.join("\n"))
    }
}

fn parse<T: DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_str(&call.arguments).map_err(|e| ToolError::InvalidArguments {
        tool: call.name.clone(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn edits_are_tracked_and_reads_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(Workspace::new(dir.path()).unwrap());

        tools
            .call(&call(
                "write_file",
                json!({"path": "src/a.rs", "content": "fn a() {}"}),
            ))
            .unwrap();
        tools
            .call(&call(
                "replace",
                json!({"path": "src/a.rs", "old": "a()", "new": "b()"}),
            ))
            .unwrap();
        let read = tools
            .call(&call("read_file", json!({"path": "src/a.rs"})))
            .unwrap()
            .for_model;

        assert_eq!(read, "fn b() {}");
        assert_eq!(tools.changed().collect::<Vec<_>>(), ["src/a.rs"]);
        assert_eq!(
            tools
                .call(&call("list_dir", json!({"path": "."})))
                .unwrap()
                .for_model,
            "src/"
        );
    }

    #[test]
    fn an_edit_reports_its_diff() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(Workspace::new(dir.path()).unwrap());
        let created = tools
            .call(&call(
                "write_file",
                json!({"path": "a.rs", "content": "one\ntwo"}),
            ))
            .unwrap();
        assert!(matches!(
            created.summary,
            ToolSummary::Changed { created: true, .. }
        ));

        let edited = tools
            .call(&call(
                "replace",
                json!({"path": "a.rs", "old": "two", "new": "2"}),
            ))
            .unwrap();
        let ToolSummary::Changed { created, diff, .. } = edited.summary else {
            panic!("an edit should report a change");
        };
        assert!(!created);
        assert_eq!(
            diff,
            [
                DiffLine::Context("one".into()),
                DiffLine::Removed("two".into()),
                DiffLine::Added("2".into()),
            ]
        );
    }

    #[test]
    fn bad_calls_are_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(Workspace::new(dir.path()).unwrap());
        let bad_json = ToolCall {
            id: "1".into(),
            name: "read_file".into(),
            arguments: "{not json".into(),
        };
        assert!(matches!(
            tools.call(&bad_json),
            Err(ToolError::InvalidArguments { .. })
        ));
        assert!(matches!(
            tools.call(&call("rm_rf", json!({}))),
            Err(ToolError::UnknownTool(_))
        ));
    }
}
