//! The repositories read together: the project's own, and those its
//! settings link (deployment, infrastructure, modules). A place is a file
//! and a line in one of them.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A repository read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Its folder's name, which places name it by.
    pub name: String,
    /// Its folder.
    pub root: PathBuf,
    /// Whether it is the project's own, which is never changed.
    pub own: bool,
}

/// Where something was read: a file of a repository, and a line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Place {
    /// The repository, by its name.
    pub repo: String,
    /// The file, from the repository's root.
    pub path: String,
    /// The line, from 1; 0 for the whole file.
    pub line: usize,
}

impl Place {
    /// A place in `repo`.
    #[must_use]
    pub fn new(repo: &str, path: &str, line: usize) -> Self {
        Self {
            repo: repo.to_owned(),
            path: path.to_owned(),
            line,
        }
    }

    /// The same file, at `line`.
    #[must_use]
    pub fn at(&self, line: usize) -> Self {
        Self {
            line,
            ..self.clone()
        }
    }
}

impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}:{}", self.repo, self.path, self.line)
        } else {
            write!(f, "{}:{}", self.repo, self.path)
        }
    }
}

/// The repositories read together, the project's own first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repos {
    /// Each repository.
    pub repos: Vec<Repo>,
}

impl Repos {
    /// The project at `root`, and the folders `linked` to it.
    #[must_use]
    pub fn new(root: &Path, linked: &[PathBuf]) -> Self {
        let mut repos = vec![Repo {
            name: name_of(root),
            root: root.to_owned(),
            own: true,
        }];
        for path in linked {
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            if repos.iter().any(|r| r.root == path) {
                continue;
            }
            let mut name = name_of(&path);
            // Two folders of the same name are told apart by their parent.
            if repos.iter().any(|r| r.name == name) {
                let parent = path.parent().map(name_of).unwrap_or_default();
                name = format!("{parent}/{name}");
            }
            repos.push(Repo {
                name,
                root: path,
                own: false,
            });
        }
        Self { repos }
    }

    /// The repository named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.name == name)
    }

    /// The file a place names, on this machine.
    #[must_use]
    pub fn file(&self, place: &Place) -> Option<PathBuf> {
        Some(self.get(&place.repo)?.root.join(&place.path))
    }

    /// The repository holding `path` and the path from its root, the
    /// deepest repository first.
    #[must_use]
    pub fn locate(&self, path: &Path) -> Option<(&Repo, String)> {
        let path = normal(path);
        self.repos
            .iter()
            .filter_map(|r| {
                let rel = path.strip_prefix(normal(&r.root)).ok()?;
                Some((r, rel.to_string_lossy().replace('\\', "/")))
            })
            .max_by_key(|(r, _)| r.root.components().count())
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || "project".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// `path` with `.` and `..` resolved, without asking the file system.
pub(crate) fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The files under `root`, as the repository sees them (ignored files and
/// hidden folders left out), with their path from `root`.
pub(crate) fn files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = ignore::WalkBuilder::new(root)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((e.path().to_owned(), rel))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_found_in_the_deepest_repository() {
        let repos = Repos::new(
            Path::new("/code/app"),
            &[
                PathBuf::from("/code/app/vendor/mods"),
                PathBuf::from("/x/deploy"),
            ],
        );
        let (repo, rel) = repos
            .locate(Path::new("/code/app/vendor/mods/rds/main.tf"))
            .unwrap();
        assert_eq!((repo.name.as_str(), rel.as_str()), ("mods", "rds/main.tf"));
        let (repo, rel) = repos
            .locate(Path::new("/x/deploy/apps/../base/k.yaml"))
            .unwrap();
        assert_eq!(
            (repo.name.as_str(), rel.as_str()),
            ("deploy", "base/k.yaml")
        );
        assert!(repos.locate(Path::new("/elsewhere")).is_none());
        assert_eq!(
            Place::new("deploy", "a.yaml", 3).to_string(),
            "deploy:a.yaml:3"
        );
    }
}
