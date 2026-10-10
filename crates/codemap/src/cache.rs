//! What was read, kept while the repositories have not changed: their
//! fingerprint is each one's commit, a hash of its uncommitted changes and
//! of the list of its untracked files, and the version of the reading
//! itself. And the linked repositories brought up to date, only where that
//! cannot lose anything.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::repos::Repos;
use crate::terraform::git;

/// Raised whenever what is read, or how, changes: older entries are then
/// read again.
const FORMAT: u32 = 1;

/// The repositories' state, as a string.
#[must_use]
pub fn fingerprint(repos: &Repos) -> String {
    let mut parts = vec![format!("format {FORMAT}")];
    for repo in &repos.repos {
        let head = git(&repo.root, &["rev-parse", "HEAD"]).unwrap_or_default();
        let diff = git(&repo.root, &["diff", "HEAD"]).unwrap_or_default();
        let untracked =
            git(&repo.root, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
        parts.push(format!(
            "{} {} {:016x} {:016x}",
            repo.name,
            head.trim(),
            fnv(diff.as_bytes()),
            fnv(untracked.as_bytes())
        ));
    }
    format!("{:016x}", fnv(parts.join("\n").as_bytes()))
}

/// FNV-1a: stable from one build to the next, unlike the standard hasher.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[derive(Serialize, Deserialize)]
struct Entry<T> {
    fingerprint: String,
    value: T,
}

/// A folder of cached entries.
#[derive(Debug, Clone)]
pub struct Cache {
    folder: PathBuf,
}

impl Cache {
    /// The entries kept in `folder`.
    #[must_use]
    pub fn new(folder: &Path) -> Self {
        Self {
            folder: folder.to_owned(),
        }
    }

    fn file(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.folder
            .join(format!("{safe}-{:08x}.json", fnv(key.as_bytes()) as u32))
    }

    /// The entry `key`, when kept with this `fingerprint`.
    #[must_use]
    pub fn get<T: DeserializeOwned>(&self, key: &str, fingerprint: &str) -> Option<T> {
        let text = std::fs::read_to_string(self.file(key)).ok()?;
        let entry: Entry<T> = serde_json::from_str(&text).ok()?;
        (entry.fingerprint == fingerprint).then_some(entry.value)
    }

    /// Keeps `value` as the entry `key`, written whole or not at all.
    ///
    /// # Errors
    ///
    /// When the folder or the file cannot be written.
    pub fn put<T: Serialize>(
        &self,
        key: &str,
        fingerprint: &str,
        value: &T,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.folder)?;
        let file = self.file(key);
        let partial = file.with_extension("json.partial");
        let text = serde_json::to_string(&Entry {
            fingerprint: fingerprint.to_owned(),
            value,
        })
        .map_err(std::io::Error::other)?;
        std::fs::write(&partial, text)?;
        std::fs::rename(partial, file)
    }
}

/// What updating a linked repository did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Update {
    /// Brought forward by this many commits.
    Updated(usize),
    /// Already up to date.
    UpToDate,
    /// Left as it is: on another branch, this many commits behind its
    /// main one.
    OtherBranch {
        /// The branch it is on.
        branch: String,
        /// How far its main branch has gone ahead of it.
        behind: usize,
    },
    /// Left as it is: it has changes not committed.
    LocalChanges,
    /// Left as it is: it has commits its remote does not.
    Diverged,
    /// It could not be fetched.
    NotFetched(String),
}

impl std::fmt::Display for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Updated(n) => write!(f, "updated, {n} commit{}", if *n == 1 { "" } else { "s" }),
            Self::UpToDate => write!(f, "up to date"),
            Self::OtherBranch { branch, behind } => {
                write!(f, "left as is, on branch {branch}, {behind} behind main")
            }
            Self::LocalChanges => write!(f, "left as is: it has local changes"),
            Self::Diverged => write!(f, "left as is: it has diverged from its remote"),
            Self::NotFetched(why) => write!(f, "could not fetch: {why}"),
        }
    }
}

/// A repository's remote main branch: what `origin/HEAD` points at, else
/// `origin/main`, else `origin/master`.
fn remote_main(root: &Path) -> Option<String> {
    if let Ok(head) = git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    ) {
        return head.trim().strip_prefix("refs/remotes/").map(str::to_owned);
    }
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|b| git(root, &["rev-parse", "--verify", "--quiet", b]).is_ok())
        .map(str::to_owned)
}

/// The branch a repository is on, `None` when detached.
#[must_use]
pub fn branch(root: &Path) -> Option<String> {
    git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|b| b.trim().to_owned())
}

/// The linked repositories that are not on their main branch, and the
/// branch each is on: what a banner warns of.
#[must_use]
pub fn off_main(repos: &Repos) -> Vec<(String, String)> {
    repos
        .repos
        .iter()
        .filter(|r| !r.own && r.root.join(".git").exists())
        .filter_map(|r| {
            let main = remote_main(&r.root)?;
            let main = main.split_once('/').map_or(main.as_str(), |(_, b)| b);
            let on = branch(&r.root).unwrap_or_else(|| "a detached commit".to_owned());
            (on != main).then(|| (r.name.clone(), on))
        })
        .collect()
}

/// Brings each linked repository up to date with its remote's main
/// branch, fetching it first, then fast-forwarding only when it is on that
/// branch with nothing of its own; the project's repository is never
/// touched.
#[must_use]
pub fn update_repos(repos: &Repos) -> Vec<(String, Update)> {
    repos
        .repos
        .iter()
        .filter(|r| !r.own && r.root.join(".git").exists())
        .map(|r| (r.name.clone(), update(&r.root)))
        .collect()
}

fn update(root: &Path) -> Update {
    if let Err(e) = git(root, &["fetch", "--quiet", "origin"]) {
        return Update::NotFetched(e);
    }
    let Some(main) = remote_main(root) else {
        return Update::NotFetched("it has no main branch on origin".to_owned());
    };
    let count = |range: &str| {
        git(root, &["rev-list", "--count", range])
            .ok()
            .and_then(|n| n.trim().parse::<usize>().ok())
            .unwrap_or(0)
    };
    let local_main = main.split_once('/').map_or(main.as_str(), |(_, b)| b);
    let on = branch(root);
    if on.as_deref() != Some(local_main) {
        return Update::OtherBranch {
            branch: on.unwrap_or_else(|| "a detached commit".to_owned()),
            behind: count(&format!("HEAD..{main}")),
        };
    }
    if git(root, &["status", "--porcelain"]).is_ok_and(|s| !s.trim().is_empty()) {
        return Update::LocalChanges;
    }
    let behind = count(&format!("HEAD..{main}"));
    if count(&format!("{main}..HEAD")) > 0 {
        return Update::Diverged;
    }
    if behind == 0 {
        return Update::UpToDate;
    }
    match git(root, &["merge", "--ff-only", "--quiet", &main]) {
        Ok(_) => Update::Updated(behind),
        Err(_) => Update::Diverged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::tests::write;

    fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]).unwrap();
        git(
            dir,
            &[
                "-c",
                "user.email=a@example.com",
                "-c",
                "user.name=a",
                "commit",
                "-qm",
                message,
            ],
        )
        .unwrap();
    }

    #[test]
    fn entries_kept_while_the_fingerprint_holds() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("app");
        write(&project, "a.txt", "one");
        git(&project, &["init", "-q"]).unwrap();
        commit(&project, "one");
        let repos = Repos::new(&project, &[]);
        let before = fingerprint(&repos);
        let cache = Cache::new(&dir.path().join("cache"));
        cache.put("deploy", &before, &vec![1, 2]).unwrap();
        assert_eq!(cache.get::<Vec<i32>>("deploy", &before), Some(vec![1, 2]));
        // An untracked file, then a change, each change the fingerprint.
        write(&project, "b.txt", "new");
        let untracked = fingerprint(&repos);
        assert_ne!(untracked, before);
        write(&project, "a.txt", "two");
        assert_ne!(fingerprint(&repos), untracked);
        assert_eq!(cache.get::<Vec<i32>>("deploy", &fingerprint(&repos)), None);
        assert!(
            !dir.path()
                .join("cache")
                .read_dir()
                .unwrap()
                .any(|e| { e.unwrap().path().to_string_lossy().ends_with(".partial") })
        );
    }

    #[test]
    fn only_a_clean_main_branch_is_fast_forwarded() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        write(&origin, "a.txt", "one");
        git(&origin, &["init", "-q", "-b", "main"]).unwrap();
        commit(&origin, "one");
        let clone = |name: &str| {
            let path = dir.path().join(name);
            git(dir.path(), &["clone", "-q", origin.to_str().unwrap(), name]).unwrap();
            path
        };
        let (clean, dirty, branched) = (clone("clean"), clone("dirty"), clone("branched"));
        write(&origin, "a.txt", "two");
        commit(&origin, "two");
        write(&dirty, "a.txt", "mine");
        git(&branched, &["checkout", "-q", "-b", "work"]).unwrap();
        let project = dir.path().join("app");
        std::fs::create_dir_all(&project).unwrap();
        let repos = Repos::new(&project, &[clean.clone(), dirty, branched]);
        let updates = update_repos(&repos);
        assert_eq!(
            updates,
            [
                ("clean".to_owned(), Update::Updated(1)),
                ("dirty".to_owned(), Update::LocalChanges),
                (
                    "branched".to_owned(),
                    Update::OtherBranch {
                        branch: "work".into(),
                        behind: 1
                    }
                ),
            ]
        );
        assert_eq!(std::fs::read_to_string(clean.join("a.txt")).unwrap(), "two");
        assert_eq!(
            off_main(&repos),
            [("branched".to_owned(), "work".to_owned())]
        );
        assert_eq!(update_repos(&repos)[0].1, Update::UpToDate);
    }
}
