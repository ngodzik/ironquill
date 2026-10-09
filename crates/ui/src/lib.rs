//! The ironquill interface's state, whatever draws it.
//!
//! [`App`] holds everything on screen and decides what each key, click and
//! message from the agent does. It performs no I/O: it returns an [`Effect`]
//! for the loop that drives it to carry out, which is what makes it testable
//! without a screen, and lets a terminal and a window drive the same state.
//! A view reads it and draws; [`keymap`] turns keys into actions.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod app;
pub mod blocks;
pub mod clipboard;
mod command;
pub mod defaults;
pub mod editor;
mod highlight;
mod host;
pub mod input;
pub mod keymap;
pub mod references;
pub mod review;
pub mod sessions;
pub mod style;
pub mod tree;
pub mod usage;

pub use app::{
    AgentMessage, App, CompactRow, DEFAULT_OPACITY, DockerPane, Effect, Entry, LineEditor, MapView,
    Panes, Settings, SubAgent, parse_window, step_title,
};
pub use defaults::{Defaults, Images};
pub use host::{Host, Incoming, Start, Waiting};
