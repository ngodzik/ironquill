//! The choices each new session starts with, kept in
//! `~/.ironquill/config.toml` (or `$IRONQUILL_HOME/config.toml`).
//!
//! ```toml
//! model = "deepseek/deepseek-chat"
//! models = ["deepseek/deepseek-chat", "anthropic/claude-sonnet-4-5"]
//! team = ["anthropic/claude-sonnet-4-5"]
//! budget = 0.10
//! effort = "high"
//! images = "auto"
//! mermaid = ["mmdc", "-i", "{input}", "-o", "{output}", "-b", "transparent"]
//! ```
//!
//! Keys never go here: they come from the environment only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graphics::Images;

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
    /// The usage pane's window, such as `6h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_window: Option<String>,
    /// The names of the secrets of the environment commands may use. Their
    /// values stay in the environment, never here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_secrets: Vec<String>,
    /// Whether a program ironquill does not know runs without asking, as
    /// `/strict off` chose; by default it asks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lenient_commands: bool,
    /// Servers commands may reach besides those already used, as the
    /// person allowed them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// Whether `/pair` has Claude Code's Opus plan and its Sonnet code; when
    /// unsaid, it does when Claude Code is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_mode: Option<bool>,
    /// Whether the warm sessions are kept warm while the conversation
    /// waits, as `/tick` left it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tick: bool,
    /// Whether replies show images: `auto` in a terminal known to draw
    /// them, `kitty` with Kitty's protocol whatever the terminal, `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Images>,
    /// The command that draws a Mermaid diagram as a PNG image, `{input}`
    /// and `{output}` standing for the files; `[]` draws none. Unsaid, the
    /// first installed of mermaid-cli (`mmdc`) and `mmdr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mermaid: Option<Vec<String>>,
}

impl Defaults {
    /// The budget of a request when nothing sets one, in dollars.
    pub const BUDGET: f64 = 0.10;

    /// Where the defaults live, or `None` without a home directory.
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("IRONQUILL_HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".ironquill"),
        };
        Some(base.join("config.toml"))
    }

    /// Where every command a model runs is written down:
    /// `~/.ironquill/audit.log`.
    pub fn audit_log_path() -> Option<PathBuf> {
        Self::path().map(|p| p.with_file_name("audit.log"))
    }

    /// The person's own instructions for every model, next to the defaults:
    /// `~/.ironquill/instructions.md`. Never in a project.
    pub fn instructions_path() -> Option<PathBuf> {
        Self::path().map(|p| p.with_file_name("instructions.md"))
    }

    /// The person's instructions as they are now; `None` when there are none.
    pub fn instructions() -> Option<String> {
        let text = fs::read_to_string(Self::instructions_path()?).ok()?;
        // The first lines, ironquill's own explanation, are not instructions.
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
            usage_window: Some("6h".into()),
            allowed_secrets: vec!["API_TOKEN".into()],
            lenient_commands: true,
            allowed_hosts: vec!["api.example.com".into()],
            pair_mode: Some(true),
            tick: true,
            images: Some(Images::Off),
            mermaid: Some(vec!["mmdr".into(), "-i".into(), "{input}".into()]),
        };
        defaults.save(&path).unwrap();
        assert_eq!(Defaults::load(&path), Ok(defaults));
    }

    #[test]
    fn images_are_auto_kitty_or_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "images = \"kitty\"").unwrap();
        assert_eq!(Defaults::load(&path).unwrap().images, Some(Images::Kitty));
        fs::write(&path, "images = \"sometimes\"").unwrap();
        assert!(Defaults::load(&path).unwrap_err().contains("sometimes"));
    }

    #[test]
    fn a_mistake_in_the_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "modle = \"x\"").unwrap();
        assert!(Defaults::load(&path).unwrap_err().contains("modle"));
    }
}
