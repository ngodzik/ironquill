//! Deterministic tools for ironquill.
//!
//! Everything here gives the same answer for the same input and costs no
//! tokens: reading and editing files inside a sandboxed workspace, running the
//! checks that judge a change, asking git about the working tree. The model
//! reaches the file tools through [`Toolbox`]; it never reaches [`Check`],
//! which the agent runs itself, so that a model cannot skip or fake a check.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod check;
mod command;
mod definitions;
mod detect;
mod diff;
mod docker;
mod error;
mod git;
mod gitview;
mod lsp;
mod outline;
mod rules;
mod search;
mod snapshot;
mod toolbox;
mod workspace;

pub use check::{Check, CheckFailure, CheckReport, Trial};
pub use command::{
    Assessment, CommandOutput, Policy, assess, known_hosts, redact_secrets, run_command, unreadable,
};
pub use definitions::{Definition, definitions, uses};
pub use detect::detect_checks;
pub use diff::{DiffLine, line_diff};
pub use docker::{Container, running_containers};
pub use error::ToolError;
pub use git::{changes_text, diff_stat, is_clean, project_context, project_files, tracked_files};
pub use gitview::{
    Change, ChangedFile, LineChanges, LineMark, branch_base, changed_files, commit_line,
    committed_lines, file_status, is_commit, line_changes, lines_at,
};
pub use lsp::{Location, LspError, Servers};
pub use outline::project_map;
pub use rules::project_instructions;
pub use snapshot::Snapshot;
pub use toolbox::{ToolOutput, ToolSummary, Toolbox};
pub use workspace::Workspace;
