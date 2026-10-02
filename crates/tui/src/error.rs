use thiserror::Error;

/// What can stop the interface.
#[derive(Debug, Error)]
pub enum TuiError {
    /// The terminal could not be set up, read or drawn to.
    #[error("terminal error")]
    Terminal(#[source] std::io::Error),
}
