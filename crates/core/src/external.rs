//! Tools served by others, such as the servers of the Model Context
//! Protocol: described here, reached by a crate that does the I/O.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::types::ToolSpec;

/// What a call to an external tool comes to: its text, or why it failed.
pub type ExternalCall<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// Tools offered to models besides universe's own, each named
/// `<server>__<tool>`.
pub trait ExternalTools: Send + Sync + fmt::Debug {
    /// The tools offered now: those of the servers that answered.
    fn specs(&self) -> Vec<ToolSpec>;

    /// Whether the tool `name` says it only reads; `None` when it is not
    /// one of these.
    fn reads_only(&self, name: &str) -> Option<bool>;

    /// Calls the tool `name` with `arguments`, a JSON object as text.
    fn call<'a>(&'a self, name: &'a str, arguments: &'a str) -> ExternalCall<'a>;

    /// Each server, and how it answered, in a few words.
    fn status(&self) -> Vec<(String, String)>;
}

/// An MCP server declared in the settings: reached at a URL (streamable
/// HTTP, with headers such as a token), or started as a command (stdio).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// Its URL, for streamable HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Headers sent with each request, such as `Authorization`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// The program and its arguments, for stdio.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Variables set for the program.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// Written by hand: headers and variables often hold tokens, and only
/// their names show.
impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = |map: &BTreeMap<String, String>| {
            map.keys()
                .map(|k| format!("{k}: <redacted>"))
                .collect::<Vec<_>>()
        };
        f.debug_struct("McpServer")
            .field("url", &self.url)
            .field("headers", &names(&self.headers))
            .field("command", &self.command)
            .field("env", &names(&self.env))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_servers_tokens_never_show_in_its_debug() {
        let server = McpServer {
            url: Some("https://mcp.example.com/mcp".into()),
            headers: BTreeMap::from([("Authorization".into(), "Bearer secret-token".into())]),
            command: Vec::new(),
            env: BTreeMap::from([("API_KEY".into(), "key-value".into())]),
        };
        let shown = format!("{server:?}");
        assert!(shown.contains("Authorization: <redacted>"));
        assert!(!shown.contains("secret-token") && !shown.contains("key-value"));
    }
}
