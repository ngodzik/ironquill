//! A branch's changes, as a review or a piece of work reads them: the files
//! that differ from where the branch left the main one, found by git. The
//! tree shows only them, a file opens folded to its changes, and a review
//! may read but not change anything.

use std::path::Path;

use ironquill_tools::{Change, ChangedFile};

/// Why the changes are looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// Someone else's work, to read: nothing can be changed.
    Review,
    /// One's own work in progress, to go on with.
    Work,
}

impl Lens {
    /// Its name, as the status line shows it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Review => "REVIEW",
            Self::Work => "WORK",
        }
    }

    /// The command that starts it.
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Work => "work",
        }
    }
}

/// What a branch changed, and against what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// Why it is looked at.
    pub lens: Lens,
    /// The commit the files are compared with.
    pub base: String,
    /// That commit as one line: its short hash and its title.
    pub base_line: String,
    /// The files that differ from it, sorted by path.
    pub files: Vec<ChangedFile>,
}

impl ChangeSet {
    /// What the project at `root` changed since `base`, or since where its
    /// branch left the main one; an error to show otherwise.
    pub(crate) fn read(root: &Path, lens: Lens, base: Option<&str>) -> Result<Self, String> {
        let base = match base {
            Some(base) => base.to_owned(),
            None => ironquill_tools::branch_base(root).ok_or_else(|| {
                format!(
                    "Nothing to compare with: this branch is where main is. /{} <commit or \
                     branch> compares with that",
                    lens.command()
                )
            })?,
        };
        let files = ironquill_tools::changed_files(root, &base)
            .ok_or_else(|| format!("{base} is not a commit of this repository"))?;
        let base_line = ironquill_tools::commit_line(root, &base).unwrap_or_else(|| base.clone());
        Ok(Self {
            lens,
            base,
            base_line,
            files,
        })
    }

    /// The change of the file at `path`, from the project's root.
    pub fn find(&self, path: &str) -> Option<&ChangedFile> {
        self.files.iter().find(|f| f.path == path)
    }

    /// How many lines were added and removed in all.
    pub fn lines(&self) -> (usize, usize) {
        self.files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed))
    }

    /// Each file's path and git's letter for its change, for the tree.
    pub(crate) fn letters(&self) -> Vec<(String, char)> {
        self.files
            .iter()
            .map(|f| {
                let letter = match f.change {
                    Change::Added => 'A',
                    Change::Modified => 'M',
                    Change::Deleted => 'D',
                    Change::Renamed { .. } => 'R',
                };
                (f.path.clone(), letter)
            })
            .collect()
    }
}
