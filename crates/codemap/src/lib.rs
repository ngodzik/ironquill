//! The shape of a codebase, for the views that draw it: its files and
//! folders as nodes, what holds what and what imports what as edges, and a
//! place for each in space.
//!
//! Everything here is read from the files, never guessed by a model: the
//! imports are found by patterns per language, and an import that does not
//! name a file of the project is left out rather than drawn wrong.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod imports;
mod layout;
mod map;

pub use layout::Layout;
pub use map::{CodeMap, Edge, EdgeKind, Language, Node, NodeKind, map};
