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
mod diff;
mod docker;
mod error;
mod git;
mod gitview;
mod outline;
mod search;
mod toolbox;
mod workspace;

pub use check::{Check, CheckFailure, CheckReport};
pub use diff::{DiffLine, line_diff};
pub use docker::{Container, running_containers};
pub use error::ToolError;
pub use git::{diff_stat, is_clean, project_context, project_files, tracked_files};
pub use gitview::{LineChanges, LineMark, committed_lines, file_status, line_changes};
pub use toolbox::{ToolOutput, ToolSummary, Toolbox};
pub use workspace::Workspace;
