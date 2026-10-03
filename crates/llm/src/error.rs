use thiserror::Error;

/// What can go wrong between ironquill and a provider.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The request never got an HTTP answer: DNS, TLS, proxy, timeout.
    #[error("could not reach {url}")]
    Transport {
        /// The endpoint that was called.
        url: String,
        /// The underlying failure, kept whole so that its cause chain is not lost.
        #[source]
        source: reqwest::Error,
    },

    /// The provider answered with an error status.
    #[error("{url} answered HTTP {status}: {body}")]
    Status {
        /// The endpoint that was called.
        url: String,
        /// The HTTP status code.
        status: u16,
        /// The response body, which usually says why.
        body: String,
    },

    /// The provider answered, but not in the expected shape.
    #[error("unexpected response from {url}: {reason}")]
    Malformed {
        /// The endpoint that was called.
        url: String,
        /// What was missing or wrong.
        reason: String,
    },

    /// An agent program could not be started at all.
    #[error("could not run {program}")]
    Spawn {
        /// The program.
        program: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// An agent the task was handed to failed, with its own explanation.
    #[error("{0}")]
    Delegate(String),

    /// The provider does not list the requested model.
    #[error("model {0} is not listed by the provider")]
    UnknownModel(String),
}
