//! The universe interface's state, whatever draws it.
//!
//! [`App`] holds everything on screen and decides what each key, click and
//! message from the agent does. It performs no I/O: it returns an [`Effect`]
//! for the loop that drives it to carry out, which is what makes it testable
//! without a screen, and lets a terminal and a window drive the same state.
//! A view reads it and draws; [`keymap`] turns keys into actions.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod app;
mod awake;
pub mod backdrop;
pub mod blocks;
pub mod changes;
pub mod clipboard;
mod command;
pub mod defaults;
pub mod diagrams;
pub mod editor;
mod highlight;
mod host;
pub mod input;
pub mod keymap;
pub mod markdown;
pub mod references;
pub mod review;
mod search;
pub mod sessions;
pub mod style;
pub mod tickets;
pub mod tree;
pub mod usage;
pub mod workspaces;

pub use app::{
    AgentMessage, App, ChatRow, ChatState, CompactRow, DEFAULT_OPACITY, DefinitionChoice,
    DockerPane, Effect, Entry, Found, LineEditor, Lookup, MapView, Panes, Settings, Shown,
    SubAgent, TaskRow, TickStatus, TicketView, parse_window, step_title,
};
pub use defaults::{Defaults, Images};
pub use host::{Host, Incoming, Start, Waiting};
pub use search::search_matches;
