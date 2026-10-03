//! Model providers for ironquill.
//!
//! The first provider speaks the OpenAI compatible chat completions protocol,
//! which routers such as Requesty or OpenRouter expose for hundreds of models
//! behind one key. Providers for other protocols are added next to it, each as
//! an implementation of [`ironquill_core::ChatModel`]. Claude Code, an agent
//! rather than a model, implements [`ironquill_core::Delegate`].

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod claude_code;
mod error;
mod openai_compat;

pub use claude_code::ClaudeCode;
pub use error::LlmError;
pub use openai_compat::OpenAiCompatible;
