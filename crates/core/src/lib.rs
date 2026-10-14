//! Domain types and traits shared by every universe crate.
//!
//! This crate does no I/O and knows nothing about any particular provider.
//! Everything that talks to the network or the filesystem lives in a crate
//! that depends on this one, never the other way round.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod external;
mod traits;
mod types;

pub use error::CoreError;
pub use external::{ExternalCall, ExternalTools, McpServer};
pub use traits::{ChatModel, Delegate};
pub use types::{
    Agent, CacheTtl, CacheUse, ChatRequest, ChatResponse, ContextUse, DelegateEvent, DelegateReply,
    DelegateRequest, Effort, Message, ModelId, Pricing, TokenCount, ToolCall, ToolSpec, Usage, Usd,
};
