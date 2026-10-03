use ironquill_tools::ToolError;
use thiserror::Error;

/// What stops a session before it reaches a verdict.
///
/// Failing checks are not an error: they are a verdict, reported in
/// [`Outcome`](crate::Outcome).
#[derive(Debug, Error)]
pub enum AgentError {
    /// The configuration cannot work.
    #[error("invalid configuration: {0}")]
    Config(&'static str),

    /// The provider failed: network, authentication, quota.
    #[error("the model request failed")]
    Model(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A check could not even be started.
    #[error("a check could not run")]
    Check(#[source] ToolError),

    /// The request spent its budget. [`crate::Session::send`] turns it into
    /// [`crate::Verdict::OverBudget`]; it is only seen inside the crate.
    #[error("the request spent its budget")]
    OverBudget,
}
