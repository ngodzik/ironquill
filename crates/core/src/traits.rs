use std::future::Future;

use crate::types::{
    ChatRequest, ChatResponse, DelegateEvent, DelegateReply, DelegateRequest, ModelId,
};

/// Something that can answer a chat request.
///
/// Each provider brings its own error type, so that a caller can tell a
/// network failure from a refusal without parsing strings.
pub trait ChatModel: Send + Sync {
    /// The error this provider reports.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Sends one request and waits for the whole answer.
    fn complete(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<ChatResponse, Self::Error>> + Send;

    /// The most `model` can read at once, in tokens, when the provider says.
    /// Used to show how full the context is; `None` shows nothing.
    fn context_window(&self, model: &ModelId) -> impl Future<Output = Option<u64>> + Send {
        let _ = model;
        std::future::ready(None)
    }
}

/// An external agent a whole task can be handed to, such as Claude Code.
///
/// Unlike a [`ChatModel`], it reads and edits files itself and runs its own
/// loop. It reports what it does through `on_event` as it happens, so that
/// the person sees it live, and the files it changed can still be checked.
pub trait Delegate: Send + Sync {
    /// The error this agent reports.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Runs one task, or one follow-up when the request names a session.
    fn run(
        &self,
        request: &DelegateRequest,
        on_event: &mut (dyn FnMut(DelegateEvent) + Send),
    ) -> impl Future<Output = Result<DelegateReply, Self::Error>> + Send;
}
