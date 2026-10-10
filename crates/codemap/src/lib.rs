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
mod cache;
mod deploy;
mod deployment;
mod grouping;
mod hcl;
mod imports;
mod layout;
mod map;
mod render;
mod repos;
mod review;
mod schema;
mod secret;
mod services;
mod settings;
mod terraform;
mod yaml;

pub use api::{Body, Operation, Parameter, Response};
pub use architecture::{Architecture, Component, ComponentKind, Link, architecture};
pub use cache::{Cache, Update, branch, fingerprint, off_main, update_repos};
pub use deploy::{
    Chart, ChartSource, Deploy, Earlier, Environment, Manifest, Part, Release, Setting, deploy,
};
pub use deployment::{
    ArchLink, Deployment, Element, ElementKind, LinkKind, cloud_block, deployment, with_cloud,
};
pub use grouping::{Criterion, Group, Grouping, Key, MAX_CRITERIA, Role, group};
pub use layout::Layout;
pub use map::{CodeMap, Edge, EdgeKind, Language, Node, NodeKind, map, map_with};
pub use render::{Object, Rendered, render};
pub use repos::{Place, Repo, Repos};
pub use review::{
    Area, Delta, Migration, Review, Revised, RouteChange, SchemaChange, migration, model_changes,
    review, route_changes,
};
pub use schema::{Column, ForeignKey, Index, Reference, Schema, Table, schema};
pub use services::{Evidence, Service, ServiceLink, Services, Tool, ToolKind, Via};
pub use settings::{
    NamedEnvironment, Settings, SettingsError, cache_folder, read_settings, settings_file,
};
pub use terraform::{
    BlockKind, CloudId, CloudKind, Inner, Quality, Resolved, Terraform, TfBlock, cloud_ids,
    configures, makes, terraform,
};
