//! The project read as a graph: folders and files, and the edges between.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::imports;

/// Beyond this a file is not read: its lines are not counted and its
/// imports not looked for. A generated file that big says nothing useful.
const MAX_READ: u64 = 512 * 1024;

/// What a source file is written in, as far as the drawing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// `.rs`
    Rust,
    /// `.py`
    Python,
    /// `.ts`, `.tsx`
    TypeScript,
    /// `.js`, `.jsx`, `.mjs`, `.cjs`
    JavaScript,
    /// `.md`
    Markdown,
    /// Settings: `.toml`, `.json`, `.yaml`, `.yml`
    Config,
    /// Anything else.
    Other,
}

impl Language {
    /// The language of a file, by its extension.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => Self::Rust,
            Some("py" | "pyi") => Self::Python,
            Some("ts" | "tsx") => Self::TypeScript,
            Some("js" | "jsx" | "mjs" | "cjs") => Self::JavaScript,
            Some("md") => Self::Markdown,
            Some("toml" | "json" | "yaml" | "yml") => Self::Config,
            _ => Self::Other,
        }
    }
}

/// What a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A folder.
    Folder,
    /// A file.
    File {
        /// What it is written in.
        language: Language,
        /// How many lines it has; 0 when it was too big to read.
        lines: usize,
    },
}

/// A folder or a file of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Its path from the project's root, with `/` between names; empty for
    /// the root itself.
    pub path: String,
    /// What it is.
    pub kind: NodeKind,
    /// How deep it sits: the root is 0.
    pub depth: usize,
}

impl Node {
    /// Its name: the last part of its path, or `.` for the root.
    #[must_use]
    pub fn name(&self) -> &str {
        self.path
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or(".")
    }
}

/// How two nodes are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// A folder holds a folder or a file.
    Contains,
    /// A file imports another.
    Imports,
}

/// An edge between two nodes, by their place in [`CodeMap::nodes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    /// The folder that holds, or the file that imports.
    pub from: usize,
    /// What is held, or imported.
    pub to: usize,
    /// Which.
    pub kind: EdgeKind,
}

/// The project as a graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeMap {
    /// The folders and files, the root first, parents before what they
    /// hold.
    pub nodes: Vec<Node>,
    /// What holds what, and what imports what.
    pub edges: Vec<Edge>,
    /// How many files were left out to stay under the limit.
    pub left_out: usize,
}

impl CodeMap {
    /// The node at `path`, from the project's root.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<usize> {
        let path = path.trim_start_matches("./").trim_end_matches('/');
        self.nodes.iter().position(|n| n.path == path)
    }
}

/// Reads the project at `root`: the files git does not ignore, `limit` at
/// most, and the folders that hold them.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::create_dir(dir.path().join("src")).unwrap();
/// std::fs::write(dir.path().join("src/lib.rs"), "mod a;\n").unwrap();
/// std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
/// let map = ironquill_codemap::map(dir.path(), 100);
/// let lib = map.find("src/lib.rs").unwrap();
/// let a = map.find("src/a.rs").unwrap();
/// assert!(map.edges.iter().any(|e| e.from == lib && e.to == a));
/// ```
#[must_use]
pub fn map(root: &Path, limit: usize) -> CodeMap {
    let mut files: Vec<PathBuf> = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .collect();
    // Sorted, so that the same project always gives the same map.
    files.sort();
    let left_out = files.len().saturating_sub(limit);
    files.truncate(limit);

    let mut map = CodeMap {
        nodes: vec![Node {
            path: String::new(),
            kind: NodeKind::Folder,
            depth: 0,
        }],
        edges: Vec::new(),
        left_out,
    };
    let mut index: HashMap<String, usize> = HashMap::from([(String::new(), 0)]);
    let mut sources: Vec<(usize, String)> = Vec::new();
    for file in &files {
        let parts: Vec<String> = file
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let mut parent = 0;
        for depth in 1..parts.len() {
            let folder = parts[..depth].join("/");
            parent = *index.entry(folder.clone()).or_insert_with(|| {
                map.nodes.push(Node {
                    path: folder,
                    kind: NodeKind::Folder,
                    depth,
                });
                map.edges.push(Edge {
                    from: parent,
                    to: map.nodes.len() - 1,
                    kind: EdgeKind::Contains,
                });
                map.nodes.len() - 1
            });
        }
        let path = parts.join("/");
        let language = Language::of(file);
        let full = root.join(file);
        let text = std::fs::metadata(&full)
            .ok()
            .filter(|m| m.len() <= MAX_READ)
            .and_then(|_| std::fs::read_to_string(&full).ok());
        let lines = text.as_deref().map_or(0, |t| t.lines().count());
        map.nodes.push(Node {
            path: path.clone(),
            kind: NodeKind::File { language, lines },
            depth: parts.len(),
        });
        let node = map.nodes.len() - 1;
        map.edges.push(Edge {
            from: parent,
            to: node,
            kind: EdgeKind::Contains,
        });
        index.insert(path, node);
        if let Some(text) = text {
            sources.push((node, text));
        }
    }

    let crates = imports::rust_crates(root, &map);
    for (node, text) in sources {
        let path = map.nodes[node].path.clone();
        let NodeKind::File { language, .. } = map.nodes[node].kind else {
            continue;
        };
        for target in imports::of(&path, language, &text, &map, &crates) {
            let edge = Edge {
                from: node,
                to: target,
                kind: EdgeKind::Imports,
            };
            if target != node && !map.edges.contains(&edge) {
                map.edges.push(edge);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn folders_hold_their_files_and_the_root_holds_all() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a/b.py", "x = 1\ny = 2\n");
        write(dir.path(), "README.md", "hi\n");
        let map = map(dir.path(), 100);
        let names: Vec<&str> = map.nodes.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(names, ["", "README.md", "src", "src/a", "src/a/b.py"]);
        let b = map.find("src/a/b.py").unwrap();
        assert_eq!(
            map.nodes[b].kind,
            NodeKind::File {
                language: Language::Python,
                lines: 2
            }
        );
        assert_eq!(map.nodes[b].name(), "b.py");
        assert_eq!(map.nodes[0].name(), ".");
        // Every node but the root is held by exactly one folder.
        for node in 1..map.nodes.len() {
            let holders = map
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Contains && e.to == node)
                .count();
            assert_eq!(holders, 1, "{}", map.nodes[node].path);
        }
    }

    #[test]
    fn what_git_ignores_stays_out_and_the_limit_is_said() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "target/\n");
        write(dir.path(), "target/big.rs", "");
        write(dir.path(), "a.rs", "");
        write(dir.path(), "b.rs", "");
        let map = map(dir.path(), 1);
        assert!(map.find("target/big.rs").is_none());
        assert_eq!(map.left_out, 1);
        assert_eq!(map.nodes.len(), 2);
    }
}
