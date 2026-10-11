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

/// The project's files as a tree, with the directories the person opened.
///
/// It shows the work: a branch's changes while they are looked at, else
/// the files not committed yet, with the rest of each folder standing as
/// one row, a click away. With nothing to show, it shows everything.
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// The first row on screen, written by the view so that a mouse click
    /// can be mapped back to a row.
    offset: Cell<usize>,
    /// What git says of files not committed, by path relative to the root.
    git: HashMap<String, char>,
    /// A branch's changes with git's letters, while they are looked at.
    only: Option<HashMap<String, char>>,
    /// What the tree shows only: the branch's changes, else the files not
    /// committed; `None` shows everything.
    filter: Option<HashMap<String, char>>,
    /// The folders shown whole all the same, the root being `""`.
    whole: BTreeSet<PathBuf>,
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
            filter: None,
            whole: BTreeSet::new(),
        };
        tree.refresh();
        tree.reveal();
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
        let git = self.only.as_ref().unwrap_or(&self.git);
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
        self.only = Some(files.into_iter().collect());
        self.whole.clear();
        self.selected = 0;
        self.refresh();
        self.reveal();
        // The first changed file chosen, ready to open.
        if let Some(first) = self
            .rows
            .iter()
            .position(|r| !r.is_dir && r.unchanged.is_none())
        {
            self.selected = first;
        }
    }

    /// Leaves the branch's changes: the files not committed show again.
    pub(crate) fn show_everything(&mut self) {
        self.only = None;
        self.whole.clear();
        self.refresh();
        self.reveal();
    }

    /// Opens the folders leading to every file shown as changed.
    pub(crate) fn reveal(&mut self) {
        let paths: Vec<String> = self
            .filter
            .as_ref()
            .map(|f| f.keys().cloned().collect())
            .unwrap_or_default();
        for path in paths {
            self.open_parents(&path);
        }
        self.refresh();
    }

    /// Opens the folders leading to `path` only, as when the agent writes
    /// it: a folder folded by hand elsewhere stays folded.
    pub(crate) fn reveal_one(&mut self, path: &str) {
        self.open_parents(path);
        self.refresh();
    }

    fn open_parents(&mut self, path: &str) {
        let mut folder = Path::new(path).parent();
        while let Some(f) = folder.filter(|f| !f.as_os_str().is_empty()) {
            self.expanded.insert(f.to_owned());
            folder = f.parent();
        }
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
        self.git = universe_tools::file_status(&self.root);
        self.filter = self
            .only
            .clone()
            .or_else(|| (!self.git.is_empty()).then(|| self.git.clone()));
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
        // Showing only the work: what was deleted is listed too, and the
        // rest of the folder, unless shown whole, is counted aside. Every
        // folder of the root stays listed, after those with changes; a
        // folder without any shows all it holds once opened.
        let at_root = relative.as_os_str().is_empty();
        let mut left_out = 0;
        let mut quiet_dirs = Vec::new();
        let mut filtered = false;
        if let Some(filter) = &self.filter {
            let prefix = relative.to_string_lossy();
            for (path, letter) in filter {
                let parent = Path::new(path).parent().unwrap_or(Path::new(""));
                if *letter == 'D' && parent.to_string_lossy() == prefix {
                    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                    if !files.contains(&name) {
                        files.push(name);
                    }
                }
            }
            files.sort();
            let changed = |path: &Path, is_dir: bool| {
                let path = path.to_string_lossy();
                if is_dir {
                    let inside = format!("{path}/");
                    filter.keys().any(|f| f.starts_with(&inside))
                } else {
                    filter.contains_key(path.as_ref())
                }
            };
            let holds_changes = at_root || changed(relative, true);
            if holds_changes && !self.whole.contains(relative) {
                filtered = true;
                let before = dirs.len() + files.len();
                if at_root {
                    (dirs, quiet_dirs) =
                        dirs.into_iter().partition(|d| changed(Path::new(d), true));
                } else {
                    dirs.retain(|d| changed(&relative.join(d), true));
                }
                files.retain(|f| changed(&relative.join(f), false));
                left_out = before - dirs.len() - quiet_dirs.len() - files.len();
            }
        }
        // Directories first, as in most file trees.
        for name in dirs.into_iter().chain(quiet_dirs) {
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
        if left_out > 0 {
            self.rows.push(Row {
                path: relative.to_owned(),
                name: format!("+{left_out} unchanged"),
                depth,
                is_dir: false,
                unchanged: Some(left_out),
            });
        } else if !filtered && self.filter.is_some() && self.whole.contains(relative) {
            self.rows.push(Row {
                path: relative.to_owned(),
                name: "− only the changes".to_owned(),
                depth,
                is_dir: false,
                unchanged: Some(0),
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
        // The row standing for a folder's unchanged files shows them, or
        // hides them again.
        if row.unchanged.is_some() {
            if !self.whole.remove(&row.path) {
                self.whole.insert(row.path);
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

    /// `project()` as a repository, everything committed.
    fn repository() -> tempfile::TempDir {
        let dir = project();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/guide.md"), "").unwrap();
        fs::write(dir.path().join("docs/faq.md"), "").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &["commit", "-q", "-m", "base"],
        ] {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir.path())
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        }
        dir
    }

    #[test]
    fn the_tree_shows_what_is_not_committed_and_keeps_the_root_in_reach() {
        let dir = repository();
        // Nothing changed: everything shows, as before.
        let tree = FileTree::new(dir.path().to_owned());
        assert_eq!(names(&tree), ["docs", "src", "README.md"]);

        // One file changed: its folders open, the folders of the root stay
        // listed after it, the rest of each folder counted.
        fs::write(dir.path().join("src/lib.rs"), "fn a() {}").unwrap();
        let mut tree = FileTree::new(dir.path().to_owned());
        assert_eq!(
            names(&tree),
            ["src", "  lib.rs", "  +1 unchanged", "docs", "+1 unchanged"]
        );
        assert_eq!(tree.git_status(Path::new("src/lib.rs"), false), Some('M'));
        // A folder without changes, opened, shows all it holds.
        tree.select(3);
        assert_eq!(tree.open(), None);
        assert_eq!(
            &names(&tree)[3..],
            ["docs", "  faq.md", "  guide.md", "+1 unchanged"]
        );
        // The root shown whole: everything, in order, and the way back.
        tree.select(6);
        assert_eq!(tree.open(), None);
        assert_eq!(
            names(&tree),
            [
                "docs",
                "  faq.md",
                "  guide.md",
                "src",
                "  lib.rs",
                "  +1 unchanged",
                "README.md",
                "− only the changes"
            ]
        );

        // A file the agent writes opens its own path only: docs, folded by
        // hand, stays folded.
        tree.select(0);
        assert_eq!(tree.open(), None);
        fs::write(dir.path().join("src/bin/x.rs"), "fn main() {}").unwrap();
        tree.reveal_one("src/bin/x.rs");
        assert_eq!(
            names(&tree),
            [
                "docs",
                "src",
                "  bin",
                "    x.rs",
                "  lib.rs",
                "README.md",
                "− only the changes"
            ]
        );
        assert_eq!(tree.git_status(Path::new("src"), true), Some('M'));
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
