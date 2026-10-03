//! The file tree shown beside the chat.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Directories that hold what a build produced or downloaded, never sources.
const SKIPPED: [&str; 3] = ["target", "node_modules", "__pycache__"];

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// Relative to the project root.
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) depth: usize,
    pub(crate) is_dir: bool,
}

/// The project's files as a tree, with the directories the person opened.
#[derive(Debug)]
pub(crate) struct FileTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// The first row on screen, written by the view so that a mouse click
    /// can be mapped back to a row.
    offset: Cell<usize>,
    /// What git says of changed files, by path relative to the root.
    git: HashMap<String, char>,
}

impl FileTree {
    pub(crate) fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root,
            expanded: BTreeSet::new(),
            rows: Vec::new(),
            selected: 0,
            offset: Cell::new(0),
            git: HashMap::new(),
        };
        tree.refresh();
        tree
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Git's letter for a file (`M`, `A`, `?`, `D`, `R`), or for a folder the
    /// strongest of its files': changes to tracked files before new files.
    pub(crate) fn git_status(&self, path: &Path, is_dir: bool) -> Option<char> {
        let path = path.to_string_lossy();
        if !is_dir {
            return self.git.get(path.as_ref()).copied();
        }
        let prefix = format!("{path}/");
        let mut found = None;
        for (file, letter) in &self.git {
            if file.starts_with(&prefix) {
                if *letter != '?' {
                    return Some('M');
                }
                found = Some('?');
            }
        }
        found
    }

    pub(crate) fn offset(&self) -> usize {
        self.offset.get()
    }

    pub(crate) fn set_offset(&self, offset: usize) {
        self.offset.set(offset);
    }

    /// Reads the directories again, keeping the selection on the same path
    /// when it still exists. Called when the agent may have added files.
    pub(crate) fn refresh(&mut self) {
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        self.git = ironquill_tools::file_status(&self.root);
        self.rows.clear();
        let root = self.root.clone();
        self.read_dir(&root, Path::new(""), 0);
        if let Some(path) = keep
            && let Some(i) = self.rows.iter().position(|r| r.path == path)
        {
            self.selected = i;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn read_dir(&mut self, dir: &Path, relative: &Path, depth: usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() && !SKIPPED.contains(&name.as_str()) => dirs.push(name),
                Ok(t) if t.is_file() => files.push(name),
                _ => {}
            }
        }
        dirs.sort();
        files.sort();
        // Directories first, as in most file trees.
        for name in dirs {
            let path = relative.join(&name);
            let open = self.expanded.contains(&path);
            self.rows.push(Row {
                path: path.clone(),
                name,
                depth,
                is_dir: true,
            });
            if open {
                self.read_dir(
                    &dir.join(path.file_name().unwrap_or_default()),
                    &path,
                    depth + 1,
                );
            }
        }
        for name in files {
            self.rows.push(Row {
                path: relative.join(&name),
                name,
                depth,
                is_dir: false,
            });
        }
    }

    pub(crate) fn move_by(&mut self, delta: i32) {
        let last = self.rows.len().saturating_sub(1) as i64;
        self.selected = (self.selected as i64 + i64::from(delta)).clamp(0, last) as usize;
    }

    pub(crate) fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
        }
    }

    pub(crate) fn select_last(&mut self) {
        self.selected = self.rows.len().saturating_sub(1);
    }

    /// Opens the selected row: a directory is expanded or collapsed, a file
    /// is returned for the caller to show.
    pub(crate) fn open(&mut self) -> Option<PathBuf> {
        let row = self.rows.get(self.selected)?.clone();
        if row.is_dir {
            if !self.expanded.remove(&row.path) {
                self.expanded.insert(row.path);
            }
            self.refresh();
            None
        } else {
            Some(row.path)
        }
    }

    /// Collapses the selected directory, or moves to the parent directory.
    pub(crate) fn close(&mut self) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if row.is_dir && self.expanded.remove(&row.path) {
            self.refresh();
            return;
        }
        if let Some(parent) = row.path.parent().filter(|p| !p.as_os_str().is_empty())
            && let Some(i) = self.rows.iter().position(|r| r.path == parent)
        {
            self.selected = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}\nfn b() {}").unwrap();
        fs::write(dir.path().join("src/bin/x.rs"), "").unwrap();
        fs::write(dir.path().join("README.md"), "").unwrap();
        fs::write(dir.path().join(".env"), "").unwrap();
        dir
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn lists_directories_first_and_skips_noise() {
        let dir = project();
        let tree = FileTree::new(dir.path().to_owned());
        assert_eq!(names(&tree), ["src", "README.md"]);
    }

    #[test]
    fn opening_a_directory_expands_it_and_h_goes_back() {
        let dir = project();
        let mut tree = FileTree::new(dir.path().to_owned());
        assert_eq!(tree.open(), None);
        assert_eq!(names(&tree), ["src", "  bin", "  lib.rs", "README.md"]);

        tree.move_by(2);
        assert_eq!(tree.open(), Some(PathBuf::from("src/lib.rs")));
        tree.close();
        assert_eq!(tree.selected(), 0);
        tree.close();
        assert_eq!(names(&tree), ["src", "README.md"]);
    }
}
