use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur in the Winx application
#[derive(Error, Debug)]
pub enum WinxError {
    /// Error when initializing the shell
    #[error("Failed to initialize shell: {0}")]
    ShellInitializationError(String),

    /// Error when operating on a workspace path
    #[error("Workspace path error: {0}")]
    WorkspacePathError(String),

    /// Error when locking the bash state
    #[error("Failed to lock the bash state: {0}")]
    BashStateLockError(String),

    /// Error when the bash state is not initialized
    #[error("Bash state not initialized, call with type=first_call first")]
    BashStateNotInitialized,

    /// Error when a command fails to execute
    #[error("Command execution failed: {0}")]
    CommandExecutionError(String),

    /// Error when parsing arguments
    #[error("Failed to parse arguments: {0}")]
    ArgumentParseError(String),

    /// Error when trying to access a file or directory
    #[error("File access error for {path}: {message}")]
    FileAccessError { path: PathBuf, message: String },

    /// Error when deserializing data
    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    /// Error when serializing data
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Type alias for Result with WinxError
pub type Result<T> = std::result::Result<T, WinxError>;

/// Extension trait to convert anyhow errors to WinxError
pub trait AnyhowErrorExt {
    /// Convert the error to a WinxError
    fn to_winx_error(self, default_message: &str) -> WinxError;
}

impl AnyhowErrorExt for anyhow::Error {
    fn to_winx_error(self, default_message: &str) -> WinxError {
        // Convert anyhow::Error to WinxError based on the error message/context
        if let Some(err) = self.downcast_ref::<WinxError>() {
            return err.clone();
        }

        let err_string = self.to_string();

        if err_string.contains("bash state") {
            WinxError::BashStateLockError(err_string)
        } else if err_string.contains("workspace") || err_string.contains("directory") {
            WinxError::WorkspacePathError(err_string)
        } else if err_string.contains("command") {
            WinxError::CommandExecutionError(err_string)
        } else if err_string.contains("parse") || err_string.contains("deserializ") {
            WinxError::DeserializationError(err_string)
        } else if err_string.contains("serialize") {
            WinxError::SerializationError(err_string)
        } else {
            WinxError::ShellInitializationError(format!("{}: {}", default_message, err_string))
        }
    }
}

/// Enable cloning for WinxError
impl Clone for WinxError {
    fn clone(&self) -> Self {
        match self {
            Self::ShellInitializationError(msg) => Self::ShellInitializationError(msg.clone()),
            Self::WorkspacePathError(msg) => Self::WorkspacePathError(msg.clone()),
            Self::BashStateLockError(msg) => Self::BashStateLockError(msg.clone()),
            Self::BashStateNotInitialized => Self::BashStateNotInitialized,
            Self::CommandExecutionError(msg) => Self::CommandExecutionError(msg.clone()),
            Self::ArgumentParseError(msg) => Self::ArgumentParseError(msg.clone()),
            Self::FileAccessError { path, message } => Self::FileAccessError {
                path: path.clone(),
                message: message.clone(),
            },
            Self::DeserializationError(msg) => Self::DeserializationError(msg.clone()),
            Self::SerializationError(msg) => Self::SerializationError(msg.clone()),
            Self::IoError(err) => Self::IoError(std::io::Error::new(err.kind(), err.to_string())),
        }
    }
}
