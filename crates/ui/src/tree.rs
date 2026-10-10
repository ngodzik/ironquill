//! The file tree shown beside the chat.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Directories that hold what a build produced or downloaded, never sources.
const SKIPPED: [&str; 3] = ["target", "node_modules", "__pycache__"];

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Relative to the project root.
    pub path: PathBuf,
    /// The file or folder's name.
    pub name: String,
    /// How deep it is, the project's top level being 0.
    pub depth: usize,
    /// Whether it is a folder.
    pub is_dir: bool,
    /// For the row that stands for the files of a folder left out, showing
    /// only the changes: how many; and opening it shows them, or hides them
    /// again when it reads so.
    pub unchanged: Option<usize>,
}

/// While only changes are shown: the files changed, with git's letter, and
/// the folders shown whole all the same.
#[derive(Debug, Default)]
struct OnlyChanges {
    files: HashMap<String, char>,
    whole: BTreeSet<PathBuf>,
}

/// The project's files as a tree, with the directories the person opened.
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// The first row on screen, written by the view so that a mouse click
    /// can be mapped back to a row.
    offset: Cell<usize>,
    /// What git says of changed files, by path relative to the root.
    git: HashMap<String, char>,
    /// Set while only the changes of a branch are shown.
    only: Option<OnlyChanges>,
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
            only: None,
        };
        tree.refresh();
        tree
    }

    /// The rows shown, folders unfolded.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row selected, by its place in the rows.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Whether this folder is unfolded.
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Git's letter for a file (`M`, `A`, `?`, `D`, `R`), or for a folder the
    /// strongest of its files': changes to tracked files before new files.
    pub fn git_status(&self, path: &Path, is_dir: bool) -> Option<char> {
        let path = path.to_string_lossy();
        // Showing a branch's changes, the letters are against its base.
        let git = self.only.as_ref().map_or(&self.git, |o| &o.files);
        if !is_dir {
            return git.get(path.as_ref()).copied();
        }
        let prefix = format!("{path}/");
        let mut found = None;
        for (file, letter) in git {
            if file.starts_with(&prefix) {
                if *letter != '?' {
                    return Some('M');
                }
                found = Some('?');
            }
        }
        found
    }

    /// Whether only a branch's changes are shown.
    pub fn shows_changes_only(&self) -> bool {
        self.only.is_some()
    }

    /// Shows only `files`, changed with git's letter, and the folders that
    /// hold them, unfolded; the rest of each folder stands as one row.
    pub(crate) fn show_changes(&mut self, files: Vec<(String, char)>) {
        for (path, _) in &files {
            let mut folder = Path::new(path).parent();
            while let Some(f) = folder.filter(|f| !f.as_os_str().is_empty()) {
                self.expanded.insert(f.to_owned());
                folder = f.parent();
            }
        }
        self.only = Some(OnlyChanges {
            files: files.into_iter().collect(),
            whole: BTreeSet::new(),
        });
        self.selected = 0;
        self.refresh();
        // The first changed file chosen, ready to open.
        if let Some(first) = self
            .rows
            .iter()
            .position(|r| !r.is_dir && r.unchanged.is_none())
        {
            self.selected = first;
        }
    }

    /// Shows every file again.
    pub(crate) fn show_everything(&mut self) {
        self.only = None;
        self.refresh();
    }

    /// The first row in view.
    pub fn offset(&self) -> usize {
        self.offset.get()
    }

    /// Records the first row in view, as the view scrolled it.
    pub fn set_offset(&self, offset: usize) {
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
        // Showing only changes: what the branch deleted is listed too, and
        // the rest of the folder, unless shown whole, is counted aside.
        let mut left_out = 0;
        if let Some(only) = &self.only {
            let prefix = relative.to_string_lossy();
            for (path, letter) in &only.files {
                let parent = Path::new(path).parent().unwrap_or(Path::new(""));
                if *letter == 'D' && parent.to_string_lossy() == prefix {
                    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                    if !files.contains(&name) {
                        files.push(name);
                    }
                }
            }
            if !only.whole.contains(relative) {
                let changed = |path: &Path, is_dir: bool| {
                    let path = path.to_string_lossy();
                    if is_dir {
                        let inside = format!("{path}/");
                        only.files.keys().any(|f| f.starts_with(&inside))
                    } else {
                        only.files.contains_key(path.as_ref())
                    }
                };
                let before = dirs.len() + files.len();
                dirs.retain(|d| changed(&relative.join(d), true));
                files.retain(|f| changed(&relative.join(f), false));
                left_out = before - dirs.len() - files.len();
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
                unchanged: None,
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
                unchanged: None,
            });
        }
        if let Some(only) = &self.only {
            if left_out > 0 {
                self.rows.push(Row {
                    path: relative.to_owned(),
                    name: format!("+{left_out} unchanged"),
                    depth,
                    is_dir: false,
                    unchanged: Some(left_out),
                });
            } else if only.whole.contains(relative) {
                self.rows.push(Row {
                    path: relative.to_owned(),
                    name: "− only the changes".to_owned(),
                    depth,
                    is_dir: false,
                    unchanged: Some(0),
                });
            }
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
        // The row standing for a folder's unchanged files shows them, or
        // hides them again.
        if row.unchanged.is_some() {
            if let Some(only) = &mut self.only
                && !only.whole.remove(&row.path)
            {
                only.whole.insert(row.path);
            }
            self.refresh();
            return None;
        }
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
    fn showing_only_changes_keeps_their_folders_and_counts_the_rest() {
        let dir = project();
        let mut tree = FileTree::new(dir.path().to_owned());
        tree.show_changes(vec![
            ("src/lib.rs".into(), 'M'),
            ("src/gone.rs".into(), 'D'),
        ]);
        assert_eq!(
            names(&tree),
            [
                "src",
                "  gone.rs",
                "  lib.rs",
                "  +1 unchanged",
                "+1 unchanged"
            ]
        );
        // The first changed file is chosen; letters are the branch's.
        assert_eq!(tree.rows()[tree.selected()].name, "gone.rs");
        assert_eq!(tree.git_status(Path::new("src"), true), Some('M'));
        assert_eq!(tree.git_status(Path::new("src/gone.rs"), false), Some('D'));

        // src's unchanged files, shown, then hidden again.
        tree.select(3);
        assert_eq!(tree.open(), None);
        assert_eq!(
            names(&tree),
            [
                "src",
                "  bin",
                "  gone.rs",
                "  lib.rs",
                "  − only the changes",
                "+1 unchanged"
            ]
        );
        tree.select(4);
        assert_eq!(tree.open(), None);
        assert_eq!(names(&tree)[3], "  +1 unchanged");

        tree.show_everything();
        assert_eq!(names(&tree), ["src", "  bin", "  lib.rs", "README.md"]);
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
