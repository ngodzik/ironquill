use std::path::PathBuf;

use thiserror::Error;

/// Why a tool could not do what it was asked.
///
/// Most of these are reported back to the model as text, since a model that
/// asked for a file outside the workspace should be told and try again, not
/// abort the session.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The path is absolute, climbs out with `..`, or resolves outside the
    /// workspace through a symbolic link.
    #[error("{0} is outside the workspace")]
    OutsideWorkspace(PathBuf),

    /// The path is inside `.git`: a hook or the repository's configuration
    /// runs code later, and is not the model's to write.
    #[error("{0} is inside .git, which models may not write")]
    Protected(PathBuf),

    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The text to replace does not occur in the file.
    #[error("the text to replace was not found in {0}")]
    NotFound(PathBuf),

    /// The text to replace occurs more than once, so the edit is ambiguous.
    #[error(
        "the text to replace occurs {count} times in {path}; include more surrounding lines so it occurs once"
    )]
    Ambiguous {
        /// The file involved.
        path: PathBuf,
        /// How many times it occurs.
        count: usize,
    },

    /// The file holds binary data, which a model cannot read as text.
    #[error("{path} is a binary file ({bytes} bytes), not text: it cannot be read or edited")]
    Binary {
        /// The file.
        path: PathBuf,
        /// Its size.
        bytes: usize,
    },

    /// A directory listing was asked for a file.
    #[error("{0} is a file, not a directory: read it with read_file")]
    NotADirectory(PathBuf),

    /// The model called a tool that does not exist.
    #[error("there is no tool named {0}")]
    UnknownTool(String),

    /// The arguments were not valid JSON or did not match the tool's schema.
    #[error("invalid arguments for {tool}: {reason}")]
    InvalidArguments {
        /// The tool that was called.
        tool: String,
        /// What was wrong.
        reason: String,
    },

    /// An external command could not be started at all.
    #[error("could not run {command}: {source}")]
    Spawn {
        /// The command line.
        command: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// git answered with an error.
    #[error("git failed: {0}")]
    Git(String),
}
