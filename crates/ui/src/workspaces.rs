//! Where a new conversation works: one of the folders `workspaces` lists in
//! the settings, the folder the program was started in, or a path typed.
//! Another folder than the current one starts the program again there.

use std::path::{Path, PathBuf};

use crate::input::{KeyCode, KeyEvent};

/// The program started again elsewhere, once the interface is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    /// The folder it starts in.
    pub dir: PathBuf,
    /// The conversation it opens there, by id; a new one when `None`.
    pub conversation: Option<String>,
}

/// `~` and `~/...` as the home folder.
#[must_use]
pub fn expand(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// The choice of a folder, asked when a new conversation starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePicker {
    /// The folders offered: those the settings list, then the current one.
    pub options: Vec<PathBuf>,
    /// The option chosen; past the last, the path typed.
    pub selected: usize,
    /// A path typed, relative to the current folder or absolute.
    pub typed: String,
    /// What a typed path that is no folder said.
    pub error: Option<String>,
    here: PathBuf,
}

/// What a key in the picker decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// Still choosing.
    Choosing,
    /// The current folder: nothing to do.
    Here,
    /// Another folder: the program starts again there.
    There(PathBuf),
}

impl WorkspacePicker {
    /// The picker for a program started in `here`, offering `workspaces`
    /// first, the first chosen; `None` when the settings list none, as
    /// there is then nothing to choose from.
    #[must_use]
    pub fn new(workspaces: &[PathBuf], here: &Path) -> Option<Self> {
        let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_owned());
        let here = real(here);
        let mut options: Vec<PathBuf> = Vec::new();
        for folder in workspaces.iter().map(|w| real(w)) {
            if folder.is_dir() && !options.contains(&folder) {
                options.push(folder);
            }
        }
        if options.is_empty() {
            return None;
        }
        if !options.contains(&here) {
            options.push(here.clone());
        }
        Some(Self {
            options,
            selected: 0,
            typed: String::new(),
            error: None,
            here,
        })
    }

    /// The folder the program was started in.
    #[must_use]
    pub fn here(&self) -> &Path {
        &self.here
    }

    /// Applies `key`: ↑ ↓ choose, typing writes a path, Enter decides, Esc
    /// stays here.
    pub fn key(&mut self, key: KeyEvent, home: Option<&Path>) -> Picked {
        let last = self.options.len();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(last),
            KeyCode::Esc => return Picked::Here,
            KeyCode::Backspace => {
                self.typed.pop();
                self.selected = last;
            }
            KeyCode::Char(c) => {
                self.typed.push(c);
                self.selected = last;
                self.error = None;
            }
            KeyCode::Enter => {
                let chosen = if self.selected < last {
                    self.options[self.selected].clone()
                } else {
                    let typed = expand(self.typed.trim(), home);
                    let path = if typed.is_absolute() {
                        typed
                    } else {
                        self.here.join(typed)
                    };
                    match path.canonicalize() {
                        Ok(dir) if dir.is_dir() => dir,
                        _ => {
                            self.error = Some(format!("{} is not a folder", path.display()));
                            return Picked::Choosing;
                        }
                    }
                };
                return if chosen == self.here {
                    Picked::Here
                } else {
                    Picked::There(chosen)
                };
            }
            _ => {}
        }
        Picked::Choosing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listed_folders_come_first_then_here_and_a_path_can_be_typed() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(b.path().join("sub")).unwrap();
        let real = |p: &Path| p.canonicalize().unwrap();
        let mut picker =
            WorkspacePicker::new(&[a.path().to_owned(), PathBuf::from("/no/such")], b.path())
                .unwrap();
        assert_eq!(picker.options, [real(a.path()), real(b.path())]);
        assert_eq!(
            picker.key(KeyCode::Enter.into(), None),
            Picked::There(real(a.path()))
        );
        picker.key(KeyCode::Down.into(), None);
        assert_eq!(picker.key(KeyCode::Enter.into(), None), Picked::Here);
        for c in "nope".chars() {
            picker.key(KeyCode::Char(c).into(), None);
        }
        assert_eq!(picker.key(KeyCode::Enter.into(), None), Picked::Choosing);
        assert!(picker.error.is_some());
        for _ in 0..4 {
            picker.key(KeyCode::Backspace.into(), None);
        }
        for c in "sub".chars() {
            picker.key(KeyCode::Char(c).into(), None);
        }
        assert_eq!(
            picker.key(KeyCode::Enter.into(), None),
            Picked::There(real(&b.path().join("sub")))
        );
        assert!(WorkspacePicker::new(&[], b.path()).is_none());
    }

    #[test]
    fn a_tilde_is_the_home_folder() {
        let home = Path::new("/home/me");
        assert_eq!(expand("~/code", Some(home)), PathBuf::from("/home/me/code"));
        assert_eq!(expand("~", Some(home)), PathBuf::from("/home/me"));
        assert_eq!(expand("~other/x", Some(home)), PathBuf::from("~other/x"));
        assert_eq!(expand("/srv", Some(home)), PathBuf::from("/srv"));
    }
}
