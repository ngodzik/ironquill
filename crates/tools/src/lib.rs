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
mod error;
mod git;
mod toolbox;
mod workspace;

pub use check::{Check, CheckFailure, CheckReport};
pub use error::ToolError;
pub use git::{is_clean, tracked_files};
pub use toolbox::Toolbox;
pub use workspace::Workspace;
