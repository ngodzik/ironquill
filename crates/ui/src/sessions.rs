//! Conversations saved on disk, so that they can be resumed.
//!
//! One JSON file per conversation under `~/.ironquill/sessions/<project>/`,
//! where `<project>` is the project's path with its slashes turned into
//! dashes. `IRONQUILL_HOME` replaces `~/.ironquill`. A file holds what the
//! model saw, so that a resumed conversation continues where it stopped, and
//! what the person saw, so that the screen comes back as it was.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ironquill_agent::Session;
use ironquill_core::{Usage, Usd};
use serde::{Deserialize, Serialize};

use crate::app::Entry;
use crate::usage::UsageLog;

/// A whole conversation as written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) project: PathBuf,
    /// Seconds since the Unix epoch.
    pub(crate) created: u64,
    pub(crate) updated: u64,
    pub(crate) requests: usize,
    pub(crate) usage: Usage,
    pub(crate) cost: Usd,
    pub(crate) cost_complete: bool,
    pub(crate) transcript: Vec<Entry>,
    /// The agent's conversation, to resume it.
    pub session: Session,
    /// The calls of the last day, for the usage pane.
    #[serde(default)]
    pub(crate) usage_log: UsageLog,
}

/// What the resume list shows of a conversation. Read from the same file:
/// serde skips the fields it does not name, so the history is not decoded.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Summary {
    /// Its id, the name of its file.
    pub id: String,
    /// Its name: given, or taken from its first request.
    pub name: String,
    /// When it was last saved, in seconds since 1970.
    pub updated: u64,
    /// How many requests it holds.
    pub requests: usize,
    /// What it cost.
    pub cost: Usd,
}

/// The conversations of one project.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store for `project`, or `None` when there is no home directory
    /// to put it in.
    pub fn for_project(project: &Path) -> Option<Self> {
        let base = match std::env::var_os("IRONQUILL_HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".ironquill"),
        };
        Some(Self::at(base.join("sessions").join(project_key(project))))
    }

    pub(crate) fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Writes `saved`, replacing any earlier version of the same conversation.
    /// Write then rename, so that a crash never leaves half a file.
    pub fn save(&self, saved: &Saved) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_vec_pretty(saved).map_err(io::Error::other)?;
        let path = self.dir.join(format!("{}.json", saved.id));
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(tmp, path)
    }

    /// The project's conversations, most recently used first. Files that
    /// cannot be read are skipped rather than failing the whole list.
    pub fn list(&self) -> Vec<Summary> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut found: Vec<Summary> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
            .collect();
        found.sort_by_key(|s| std::cmp::Reverse(s.updated));
        found
    }

    /// The id of the conversation whose id is `id` or starts with it.
    pub fn find(&self, id: &str) -> Result<String, String> {
        // An id names a file in this directory: nothing that could leave it.
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("{id:?} is not a conversation id"));
        }
        let found: Vec<String> = self
            .list()
            .into_iter()
            .map(|s| s.id)
            .filter(|s| s.starts_with(id))
            .collect();
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(format!(
                "No saved conversation of this project has the id {id}"
            )),
            many => Err(format!(
                "{} conversations have an id starting with {id}: give more of it",
                many.len()
            )),
        }
    }

    /// Reads the conversation with this id.
    pub fn load(&self, id: &str) -> Result<Saved, String> {
        let path = self.dir.join(format!("{id}.json"));
        let bytes = fs::read(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("Cannot read {}: {e}", path.display()))
    }
}

/// `/home/me/code/app` becomes `-home-me-code-app`: one directory per
/// project, readable at a glance.
fn project_key(project: &Path) -> String {
    project
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An identifier that sorts by creation time and does not collide between
/// two conversations started in the same second.
pub(crate) fn new_id() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:05x}", since.as_secs(), since.subsec_micros())
}

/// `just now`, `5 min ago`, `3 h ago`, `2 days ago`.
pub fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => {
            let days = secs / 86_400;
            format!("{days} day{} ago", if days == 1 { "" } else { "s" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(id: &str, name: &str, updated: u64) -> Saved {
        Saved {
            id: id.into(),
            name: name.into(),
            project: PathBuf::from("/p"),
            created: updated,
            updated,
            requests: 2,
            usage: Usage::default(),
            cost: Usd(0.0012),
            cost_complete: true,
            transcript: vec![Entry::User("hello".into())],
            session: Session::new(),
            usage_log: UsageLog::default(),
        }
    }

    #[test]
    fn a_conversation_is_found_by_the_start_of_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_owned());
        for (id, at) in [
            ("1759600000-0a1b2", 1),
            ("1759600000-0c3d4", 2),
            ("1759700000-00001", 3),
        ] {
            store.save(&saved(id, "x", at)).unwrap();
        }
        assert_eq!(store.find("17597").as_deref(), Ok("1759700000-00001"));
        assert_eq!(
            store.find("1759600000-0c").as_deref(),
            Ok("1759600000-0c3d4")
        );
        assert!(
            store
                .find("1759600000")
                .unwrap_err()
                .starts_with("2 conversations")
        );
        assert!(
            store
                .find("9")
                .unwrap_err()
                .starts_with("No saved conversation")
        );
        // Nothing that could name a file elsewhere.
        assert!(
            store
                .find("../secret")
                .unwrap_err()
                .ends_with("is not a conversation id")
        );
    }

    #[test]
    fn saved_conversations_come_back_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_owned());
        store.save(&saved("1", "old", 100)).unwrap();
        store.save(&saved("2", "new", 200)).unwrap();
        store.save(&saved("1", "old, renamed", 100)).unwrap();
        fs::write(dir.path().join("broken.json"), "{").unwrap();

        let list = store.list();
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["new", "old, renamed"]
        );

        let loaded = store.load("1").unwrap();
        assert_eq!(loaded.transcript, [Entry::User("hello".into())]);
        assert!(store.load("missing").is_err());
    }

    #[test]
    fn project_paths_become_one_directory_name() {
        assert_eq!(project_key(Path::new("/home/me/app")), "-home-me-app");
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7_200), "2 h ago");
        assert_eq!(ago(0, 86_400), "1 day ago");
    }
}
