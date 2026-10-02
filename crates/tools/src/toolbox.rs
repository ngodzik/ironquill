use std::collections::BTreeSet;
use std::fs;

use ironquill_core::{ToolCall, ToolSpec};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::error::ToolError;
use crate::workspace::Workspace;

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
    pub fn call(&mut self, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            "read_file" => {
                let args: PathArgs = parse(call)?;
                self.workspace.read(&args.path)
            }
            "list_dir" => {
                let args: PathArgs = parse(call)?;
                self.list(&args.path)
            }
            "replace" => {
                let args: ReplaceArgs = parse(call)?;
                self.workspace.replace(&args.path, &args.old, &args.new)?;
                self.changed.insert(args.path);
                Ok("replaced".into())
            }
            "write_file" => {
                let args: WriteArgs = parse(call)?;
                self.workspace.write(&args.path, &args.content)?;
                self.changed.insert(args.path);
                Ok("written".into())
            }
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }

    fn list(&self, relative: &str) -> Result<String, ToolError> {
        let path = self.workspace.resolve(relative)?;
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
            .unwrap();

        assert_eq!(read, "fn b() {}");
        assert_eq!(tools.changed().collect::<Vec<_>>(), ["src/a.rs"]);
        assert_eq!(
            tools.call(&call("list_dir", json!({"path": "."}))).unwrap(),
            "src/"
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
