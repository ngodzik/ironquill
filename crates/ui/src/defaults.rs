//! The choices each new session starts with, kept in
//! `~/.universe/config.toml` (or `$UNIVERSE_HOME/config.toml`).
//!
//! ```toml
//! model = "deepseek/deepseek-chat"
//! models = ["deepseek/deepseek-chat", "anthropic/claude-sonnet-4-5"]
//! team = ["anthropic/claude-sonnet-4-5"]
//! budget = 0.10
//! effort = "high"
//! mermaid = ["mmdc", "-i", "{input}", "-o", "{output}", "-b", "transparent"]
//! cache_ttl = "1h"
//! background = "stars"
//! background_motion = "slow"
//! ```
//!
//! Keys never go here: they come from the environment only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use universe_core::CacheTtl;

use std::collections::BTreeMap;

use universe_core::McpServer;

use crate::backdrop::{Motion, Pattern};
use serde::{Deserialize, Serialize};

/// What a new session starts with, unless the command line says otherwise.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// The model that answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The models offered by the model picker (Ctrl-E).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// The models the first one may hand tasks to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub team: Vec<String>,
    /// The most one request may cost, in dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<f64>,
    /// How hard models think: low, medium, high, xhigh or max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The member of the team that plans in a pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    /// The model for work that starts with little context: `/debug`'s
    /// investigations, reading a ticket, checking progress. Unsaid, the
    /// conversation's model does it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_model: Option<String>,
    /// The usage pane's window, such as `6h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_window: Option<String>,
    /// The names of the secrets of the environment commands may use. Their
    /// values stay in the environment, never here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_secrets: Vec<String>,
    /// Whether a program universe does not know runs without asking, as
    /// `/strict off` chose; by default it asks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lenient_commands: bool,
    /// Servers commands may reach besides those already used, as the
    /// person allowed them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// The project's own programs, which strict mode knows as if it had a
    /// rule for them: a migration tool, say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_programs: Vec<String>,
    /// Whether `/pair` has Claude Code's Opus plan and its Sonnet code; when
    /// unsaid, it does when Claude Code is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_mode: Option<bool>,
    /// Whether the warm sessions are kept warm while the conversation
    /// waits, as `/tick` left it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tick: bool,
    /// Whether the machine is kept from idle sleep while ticks run; unsaid,
    /// it is. Set in the file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_awake: Option<bool>,
    /// Whether the window lets what is behind it show through, as a
    /// terminal can, as Ctrl-M left it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub see_through: bool,
    /// How opaque the window is while see-through, from 0.2 to 1. Set in
    /// the file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// How large the window's text is, as Cmd + and Cmd − left it; unsaid,
    /// 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_size: Option<f32>,
    /// How wide the window's conversation is beside a file or the
    /// codebase, in points, as last dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_width: Option<f32>,
    /// The command that draws a Mermaid diagram as a PNG image, `{input}`
    /// and `{output}` standing for the files; `[]` draws none. Unsaid, the
    /// first installed of mermaid-cli (`mmdc`) and `mmdr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mermaid: Option<Vec<String>>,
    /// How long what Claude's cache marks write is kept, through an API:
    /// `5m` (unsaid) or `1h`. Set in the file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<CacheTtl>,
    /// The pattern behind the window's panels: stars, waves, grid or
    /// rain; unsaid, none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Pattern>,
    /// Whether it moves: still (unsaid) or slow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_motion: Option<Motion>,
    /// The folders a new conversation may work in, `~` allowed: asked at
    /// the start, the first chosen. Set in the file only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    /// The share of the model's context, in percent, past which `/compact`
    /// is suggested; 80 when unsaid, 100 never. Set in the file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_warning: Option<u8>,
    /// The models whose conversation may start again from its summary when
    /// their cache expired after a pause, the person asked first: typically
    /// the expensive ones. Set in the file only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summarize_when_cold: Vec<String>,
    /// MCP servers, `[mcp.<name>]`, each a `url` (with `headers`) or a
    /// `command` (with `env`). Set in the file only: it may hold tokens.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, McpServer>,
    /// Ticket trackers, `[trackers.<name>]`, each read through an MCP
    /// server's tools. Set in the file only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub trackers: BTreeMap<String, crate::tickets::Tracker>,
}

impl Defaults {
    /// The budget of a request when nothing sets one, in dollars.
    pub const BUDGET: f64 = 0.10;

    /// Where the defaults live, or `None` without a home directory.
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("UNIVERSE_HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".universe"),
        };
        Some(base.join("config.toml"))
    }

    /// Where every command a model runs is written down:
    /// `~/.universe/audit.log`.
    pub fn audit_log_path() -> Option<PathBuf> {
        Self::path().map(|p| p.with_file_name("audit.log"))
    }

    /// The person's own instructions for every model, next to the defaults:
    /// `~/.universe/instructions.md`. Never in a project.
    pub fn instructions_path() -> Option<PathBuf> {
        Self::path().map(|p| p.with_file_name("instructions.md"))
    }

    /// The person's instructions as they are now; `None` when there are none.
    pub fn instructions() -> Option<String> {
        let text = fs::read_to_string(Self::instructions_path()?).ok()?;
        // The first lines, universe's own explanation, are not instructions.
        let text: String = text
            .lines()
            .filter(|line| !line.starts_with("<!--"))
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// Reads the defaults at `path`; none when the file does not exist.
    ///
    /// # Errors
    ///
    /// A sentence for the person when the file cannot be read or is not
    /// valid: starting with other choices than theirs would be worse.
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the defaults to `path`, through a temporary file so that a
    /// crash never leaves half of one.
    ///
    /// # Errors
    ///
    /// Any error writing the file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        // Its servers' headers may hold tokens: readable by its owner only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_a_save_and_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("config.toml");
        assert_eq!(Defaults::load(&path), Ok(Defaults::default()));
        let defaults = Defaults {
            model: Some("a/cheap".into()),
            models: vec!["a/cheap".into(), "b/strong".into()],
            team: vec!["b/strong".into()],
            budget: Some(0.25),
            effort: Some("max".into()),
            planner: Some("b/strong".into()),
            fresh_model: Some("a/cheap".into()),
            usage_window: Some("6h".into()),
            allowed_secrets: vec!["API_TOKEN".into()],
            lenient_commands: true,
            allowed_hosts: vec!["api.example.com".into()],
            allowed_programs: vec!["migrate".into()],
            pair_mode: Some(true),
            tick: true,
            keep_awake: None,
            see_through: true,
            opacity: Some(0.7),
            text_size: Some(1.25),
            chat_width: Some(420.0),
            mermaid: Some(vec!["mmdr".into(), "-i".into(), "{input}".into()]),
            cache_ttl: Some(CacheTtl::OneHour),
            background: Some(Pattern::Rain),
            background_motion: Some(Motion::Slow),
            workspaces: vec!["~/code/shop".into()],
            context_warning: Some(90),
            summarize_when_cold: vec!["b/strong".into()],
            mcp: BTreeMap::from([(
                "tracker".into(),
                McpServer {
                    url: Some("https://mcp.example.com/mcp".into()),
                    headers: BTreeMap::from([("Authorization".into(), "Bearer t".into())]),
                    ..McpServer::default()
                },
            )]),
            trackers: BTreeMap::from([(
                "issues".into(),
                crate::tickets::Tracker {
                    server: "tracker".into(),
                    issue: "get_issue".into(),
                    comments: None,
                    argument: "id".into(),
                    ids: "[A-Z]+-[0-9]+".into(),
                    hosts: vec!["issues.example.com".into()],
                },
            )]),
        };
        defaults.save(&path).unwrap();
        assert_eq!(Defaults::load(&path), Ok(defaults));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "its tokens are its owner's");
        }
    }

    #[test]
    fn the_cache_lasts_five_minutes_or_an_hour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "cache_ttl = \"1h\"").unwrap();
        assert_eq!(
            Defaults::load(&path).unwrap().cache_ttl,
            Some(CacheTtl::OneHour)
        );
        fs::write(&path, "cache_ttl = \"2h\"").unwrap();
        assert!(Defaults::load(&path).unwrap_err().contains("2h"));
    }

    #[test]
    fn a_mistake_in_the_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "modle = \"x\"").unwrap();
        assert!(Defaults::load(&path).unwrap_err().contains("modle"));
    }
}
