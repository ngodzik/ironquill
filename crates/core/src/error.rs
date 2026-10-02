use thiserror::Error;

/// Errors raised while building or validating domain values.
#[derive(Debug, Error, PartialEq)]
pub enum CoreError {
    /// A model identifier was empty or made only of whitespace.
    #[error("model identifier is empty")]
    EmptyModelId,

    /// A price was negative or not a finite number.
    #[error("price must be a finite, non-negative amount, got {0}")]
    InvalidPrice(f64),
}
