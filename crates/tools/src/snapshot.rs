//! Which files a command changed, when it says nothing of it: the project as
//! it was before, compared with the project after.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::process::{Command, Stdio};

use ignore::WalkBuilder;

/// The project's files that may change, each with a hash of what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot(BTreeMap<String, u64>);

impl Snapshot {
    /// The project as it is: in a git repository, the files that differ
    /// from the last commit, which is quick in a large one; elsewhere every
    /// file git would not ignore.
    pub fn take(root: &Path) -> Self {
        let paths = dirty(root).unwrap_or_else(|| all_files(root));
        Self(
            paths
                .into_iter()
                .map(|path| {
                    let hash = content_hash(&root.join(&path));
                    (path, hash)
                })
                .collect(),
        )
    }

    /// The files that changed since this snapshot was taken, in order.
    pub fn changed(&self, root: &Path) -> Vec<String> {
        let now = Self::take(root);
        let mut changed: Vec<String> = now
            .0
            .iter()
            .filter(|(path, hash)| self.0.get(*path) != Some(hash))
            .map(|(path, _)| path.clone())
            .collect();
        // Clean again, as after a commit or a checkout: it changed too.
        changed.extend(
            self.0
                .keys()
                .filter(|path| !now.0.contains_key(*path))
                .cloned(),
        );
        changed.sort();
        changed.dedup();
        changed
    }
}

/// The files git lists as changed, added or untracked, `None` outside a
/// repository.
fn dirty(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(path.to_owned());
        // A rename gives the old path next.
        if (entry.starts_with('R') || entry.starts_with('C'))
            && let Some(old) = entries.next()
        {
            paths.push(old.to_owned());
        }
    }
    Some(paths)
}

/// Every file of the project git would not ignore, relative to its root.
fn all_files(root: &Path) -> Vec<String> {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            e.path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// A hash of what the file at `path` holds; 0 when it is gone.
fn content_hash(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn changes_are_found_in_a_repository_and_outside_one() {
        for repository in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            std::fs::write(root.join("kept.txt"), "a").unwrap();
            std::fs::write(root.join("edited.txt"), "a").unwrap();
            std::fs::write(root.join("gone.txt"), "a").unwrap();
            if repository {
                git(root, &["init", "-q"]);
                git(root, &["add", "."]);
                git(
                    root,
                    &[
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "commit",
                        "-qm",
                        "start",
                    ],
                );
            }
            std::fs::write(root.join("dirty.txt"), "already").unwrap();
            let before = Snapshot::take(root);
            std::fs::write(root.join("edited.txt"), "b").unwrap();
            std::fs::remove_file(root.join("gone.txt")).unwrap();
            std::fs::write(root.join("new.txt"), "c").unwrap();
            assert_eq!(
                before.changed(root),
                ["edited.txt", "gone.txt", "new.txt"],
                "in a repository: {repository}"
            );
        }
    }
}
