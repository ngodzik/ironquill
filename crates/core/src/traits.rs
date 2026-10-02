use std::future::Future;

use crate::types::{ChatRequest, ChatResponse};

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
}
