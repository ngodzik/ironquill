//! The shape of a codebase, for the views that draw it: its files and
//! folders as nodes, what holds what and what imports what as edges, and a
//! place for each in space.
//!
//! Everything here is read from the files, never guessed by a model: the
//! imports are found by patterns per language, and an import that does not
//! name a file of the project is left out rather than drawn wrong.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub(crate) mod api;
mod architecture;
mod grouping;
mod imports;
mod layout;
mod map;
mod review;

pub use api::{Body, Operation, Parameter, Response};
pub use architecture::{Architecture, Component, ComponentKind, Link, architecture};
pub use grouping::{Criterion, Group, Grouping, Key, MAX_CRITERIA, Role, group};
pub use layout::Layout;
pub use map::{CodeMap, Edge, EdgeKind, Language, Node, NodeKind, map, map_with};
pub use review::{
    Area, Delta, Migration, Review, Revised, RouteChange, SchemaChange, migration, model_changes,
    review, route_changes,
};
