//! The ironquill loop.
//!
//! A model edits files through the [`Toolbox`](ironquill_tools::Toolbox).
//! When it says it is done, ironquill runs the checks itself. If they pass,
//! the session ends. If they fail, the same model gets the failure and another
//! round; after its rounds are spent, the next model in the list starts from a
//! short brief rather than the whole conversation, which keeps a strong model
//! from paying to read a weak one's history.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod context;
mod delegate;
mod error;
mod event;
mod run;

pub use config::{
    AgentConfig, AgentConfigBuilder, Answer, Approval, Approver, COMPACT_AT, Member, Pair, Question,
};
pub use error::AgentError;
pub use event::Event;
pub use run::{Compaction, Outcome, Session, Subject, Verdict, run};
