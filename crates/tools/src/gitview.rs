//! What git knows about the project, for showing changes as they are being
//! made: the last committed version of a file, how the lines on screen differ
//! from it, and which files differ from the last commit.
//!
//! Synchronous on purpose: each call is one short git command, made when a
//! file opens or the file tree refreshes, not in a loop.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};

use similar::{Algorithm, DiffOp, capture_diff_slices};

/// How a line on screen differs from the last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMark {
    /// The line was there, unchanged.
    Same,
    /// The line is new.
    Added,
    /// The line replaces one or more lines of the last commit.
    Changed,
}

/// The lines on screen compared with the last commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineChanges {
    /// One mark per line on screen.
    pub marks: Vec<LineMark>,
    /// Lines of the last commit that are gone, keyed by the line on screen
    /// they were before (the number of lines for the end of the file).
    pub removed: BTreeMap<usize, Vec<String>>,
}

impl LineChanges {
    /// Whether anything differs.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.marks.iter().all(|m| *m == LineMark::Same)
    }
}

/// Compares `current` with `base`, line by line.
///
/// # Examples
///
/// ```
/// use ironquill_tools::{LineMark, line_changes};
///
/// let base = ["a", "b", "c"].map(String::from);
/// let now = ["a", "B", "c", "d"].map(String::from);
/// let changes = line_changes(&base, &now);
/// assert_eq!(changes.marks[1], LineMark::Changed);
/// assert_eq!(changes.marks[3], LineMark::Added);
/// assert_eq!(changes.removed[&1], ["b"]);
/// ```
pub fn line_changes(base: &[String], current: &[String]) -> LineChanges {
    let mut changes = LineChanges {
        marks: vec![LineMark::Same; current.len()],
        removed: BTreeMap::new(),
    };
    for op in capture_diff_slices(Algorithm::Myers, base, current) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert {
                new_index, new_len, ..
            } => mark(&mut changes.marks, new_index, new_len, LineMark::Added),
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => {
                changes
                    .removed
                    .entry(new_index)
                    .or_default()
                    .extend_from_slice(&base[old_index..old_index + old_len]);
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                changes
                    .removed
                    .entry(new_index)
                    .or_default()
                    .extend_from_slice(&base[old_index..old_index + old_len]);
                mark(&mut changes.marks, new_index, new_len, LineMark::Changed);
            }
        }
    }
    changes
}

fn mark(marks: &mut [LineMark], from: usize, len: usize, mark: LineMark) {
    for slot in marks.iter_mut().skip(from).take(len) {
        *slot = mark;
    }
}

/// The lines of `path` (relative to `root`) as of the last commit.
///
/// `None` when `root` is not in a git repository, so that nothing is marked.
/// An empty list for a file git does not know yet: all of it is new.
pub fn committed_lines(root: &Path, path: &Path) -> Option<Vec<String>> {
    lines_at(root, path, "HEAD")
}

/// The lines of the file at `path` as of `rev`, as [`committed_lines`]
/// does for the last commit.
pub fn lines_at(root: &Path, path: &Path, rev: &str) -> Option<Vec<String>> {
    git(root, &["rev-parse", "--is-inside-work-tree"])?;
    let spec = format!("{rev}:./{}", path.to_string_lossy());
    Some(
        git(root, &["show", &spec])
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default(),
    )
}

/// Where the current branch left the main one: the merge base of `HEAD`
/// with `main`, `master` or their remote, when that is not `HEAD` itself.
pub fn branch_base(root: &Path) -> Option<String> {
    let head = git(root, &["rev-parse", "HEAD"])?;
    ["main", "master", "origin/main", "origin/master"]
        .iter()
        .find_map(|branch| git(root, &["merge-base", "HEAD", branch]))
        .map(|base| base.trim().to_owned())
        .filter(|base| *base != head.trim())
}

/// Whether `hash` names a commit of the repository at `root`.
pub fn is_commit(root: &Path, hash: &str) -> bool {
    hash.chars().all(|c| c.is_ascii_hexdigit())
        && git(root, &["cat-file", "-e", &format!("{hash}^{{commit}}")]).is_some()
}

/// What git says of each changed file, relative to `root`: `M` modified,
/// `A` added, `?` not tracked yet, `D` deleted, `R` renamed. Empty outside a
/// repository.
pub fn file_status(root: &Path) -> HashMap<String, char> {
    let Some(prefix) = git(root, &["rev-parse", "--show-prefix"]) else {
        return HashMap::new();
    };
    let prefix = prefix.trim().to_owned();
    let Some(status) = git(root, &["status", "--porcelain=v1", "--untracked-files=all"]) else {
        return HashMap::new();
    };
    status
        .lines()
        .filter_map(|line| {
            let (code, path) = (line.get(..2)?, line.get(3..)?);
            // A rename lists "old -> new"; the file on disk is the new one.
            let path = path.rsplit(" -> ").next()?.trim_matches('"');
            let relative = path.strip_prefix(&prefix)?.to_owned();
            let letter = match code {
                "??" => '?',
                c if c.contains('D') => 'D',
                c if c.contains('R') => 'R',
                c if c.contains('A') => 'A',
                _ => 'M',
            };
            Some((relative, letter))
        })
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        // The repository of the directory given, whatever started ironquill.
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_pure_deletion_shows_where_the_lines_were() {
        let changes = line_changes(&lines("a\nb\nc\nd"), &lines("a\nd"));
        assert!(changes.marks.iter().all(|m| *m == LineMark::Same));
        assert_eq!(changes.removed[&1], ["b", "c"]);
    }

    #[test]
    fn lines_removed_at_the_end_are_keyed_after_the_last_line() {
        let changes = line_changes(&lines("a\nb"), &lines("a"));
        assert_eq!(changes.removed[&1], ["b"]);
    }

    #[test]
    fn unchanged_text_has_no_changes() {
        assert!(line_changes(&lines("a\nb"), &lines("a\nb")).is_empty());
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                // Run from a git hook, these would point at the outer repository.
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.py"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.py"), "x\n").unwrap();
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        dir
    }

    #[test]
    fn the_committed_version_and_the_status_come_from_git() {
        let dir = repo();
        std::fs::write(dir.path().join("src/a.py"), "one\n2\n").unwrap();
        std::fs::write(dir.path().join("new.py"), "y\n").unwrap();

        assert_eq!(
            committed_lines(dir.path(), Path::new("src/a.py")),
            Some(lines("one\ntwo"))
        );
        assert_eq!(
            committed_lines(dir.path(), Path::new("new.py")),
            Some(vec![])
        );

        let status = file_status(dir.path());
        assert_eq!(status.get("src/a.py"), Some(&'M'));
        assert_eq!(status.get("new.py"), Some(&'?'));
        assert_eq!(status.get("b.py"), None);
    }

    #[test]
    fn outside_git_there_is_nothing_to_compare() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x\n").unwrap();
        assert_eq!(committed_lines(dir.path(), Path::new("a.py")), None);
        assert!(file_status(dir.path()).is_empty());
    }
}
