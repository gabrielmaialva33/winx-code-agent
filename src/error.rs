use std::io;
use thiserror::Error;

/// Custom error types for the Winx application
#[derive(Error, Debug)]
pub enum WinxError {
    /// Error related to IO operations
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// Error related to executing bash commands
    #[error("Bash command error: {0}")]
    BashCommand(String),

    /// Error when a shell process is not initialized
    #[error("Shell process not initialized")]
    ShellNotInitialized,

    /// Error when a command is executed while another is still running
    #[error("Command is already running")]
    CommandAlreadyRunning,

    /// Error when bash commands are not allowed in the current mode
    #[error("Bash commands not allowed in current mode")]
    CommandsNotAllowed,

    /// Error when a chat ID doesn't match
    #[error("Chat ID mismatch: {0}")]
    ChatIdMismatch(String),

    /// Error when the service fails to initialize
    #[error("Service initialization error: {0}")]
    ServiceInitialization(String),

    /// Error for invalid workspace paths
    #[error("Invalid workspace path: {0}")]
    InvalidWorkspacePath(String),

    /// Error for any other cases
    #[error("Unknown error: {0}")]
    Unknown(String),
}

/// Type alias for Result with WinxError
pub type WinxResult<T> = std::result::Result<T, WinxError>;

/// Helper function to convert anyhow::Error to WinxError
pub fn to_winx_error(err: anyhow::Error) -> WinxError {
    if let Some(e) = err.downcast_ref::<io::Error>() {
        return WinxError::Io(io::Error::new(e.kind(), e.to_string()));
    }

    WinxError::Unknown(err.to_string())
}

// Note: No need to manually implement From<WinxError> for anyhow::Error.
// The thiserror crate automatically implements std::error::Error,
// and anyhow::Error has a blanket impl for all types that implement std::error::Error.
