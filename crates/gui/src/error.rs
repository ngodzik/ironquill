//! Why the window could not run.

/// Why the window could not run.
#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    /// Bevy stopped with an error code: the window could not be opened, or
    /// a system failed.
    #[error("the window stopped with error code {0}")]
    Exited(u8),
}
