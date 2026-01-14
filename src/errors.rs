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
    #[error("Bash state not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path.")]
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

    /// Security error - path traversal or symlink escape attempt
    #[error("Security violation: {message}")]
    PathSecurityError { path: PathBuf, message: String },

    /// Error when a command is not allowed in the current mode
    #[error("Command not allowed: {0}")]
    CommandNotAllowed(String),

    /// Error when chat IDs don't match
    #[error("Thread ID mismatch: {0}")]
    ThreadIdMismatch(String),

    /// Error when deserializing data
    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    /// Error when serializing data
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Error in the search/replace format
    #[error("Search/replace syntax error: {0}")]
    SearchReplaceSyntaxError(String),

    /// Error when search block is not found in content
    #[error("Search block not found in content: {0}")]
    SearchBlockNotFound(String),

    /// Error when search block matches multiple locations (WCGW-style)
    #[error("Search block matched multiple times")]
    SearchBlockAmbiguous { block_content: String, match_count: usize, suggestions: Vec<String> },

    /// Error when search blocks have conflicting matches
    #[error("Multiple search blocks have conflicting matches")]
    SearchBlockConflict { conflicting_blocks: Vec<String>, first_differing_block: Option<String> },

    /// Enhanced search/replace syntax error with detailed context
    #[error("Search/replace syntax error: {message}")]
    SearchReplaceSyntaxErrorDetailed {
        message: String,
        line_number: Option<usize>,
        block_type: Option<String>,
        suggestions: Vec<String>,
    },

    /// Error when JSON parsing fails
    #[error("Invalid JSON: {0}")]
    JsonParseError(String),

    /// Error when a file is too large for operation
    #[error("File {path} is too large: {size} bytes (max {max_size})")]
    FileTooLarge { path: PathBuf, size: u64, max_size: u64 },

    /// Error when writing to a file
    #[error("Failed to write file {path}: {message}")]
    FileWriteError { path: PathBuf, message: String },

    /// Error loading data
    #[error("Failed to load data: {0}")]
    DataLoadingError(String),

    /// Parameter validation error
    #[error("Invalid parameter: {field} - {message}")]
    ParameterValidationError { field: String, message: String },

    /// Required parameter missing error
    #[error("Required parameter missing: {field} - {message}")]
    MissingParameterError { field: String, message: String },

    /// Null or undefined value error
    #[error("Null or undefined value where object expected: {field}")]
    NullValueError { field: String },

    /// Recovery suggestion error with potential solutions
    #[error("{message} - {suggestion}")]
    RecoverableSuggestionError { message: String, suggestion: String },

    /// Context save error
    #[error("Context save error: {0}")]
    ContextSaveError(String),

    /// Command timeout error
    #[error("Command timed out after {timeout_seconds}s: {command}")]
    CommandTimeout { command: String, timeout_seconds: u64 },

    /// Interactive command detected error
    #[error(
        "Interactive command detected: {command}. Use appropriate flags or consider alternatives."
    )]
    InteractiveCommandDetected { command: String },

    /// Command already running error
    #[error("A command is already running: '{current_command}' (for {duration_seconds:.1}s). Use status_check, send_text, or interrupt.")]
    CommandAlreadyRunning { current_command: String, duration_seconds: f64 },

    /// Process cleanup error
    #[error("Failed to cleanup process: {message}")]
    ProcessCleanupError { message: String },

    /// Buffer overflow error
    #[error("Command output exceeded maximum size: {size} bytes (max {max_size})")]
    BufferOverflow { size: usize, max_size: usize },

    /// Session recovery error
    #[error("Failed to recover bash session: {message}")]
    SessionRecoveryError { message: String },

    /// Resource allocation error
    #[error("Resource allocation failed: {message}")]
    ResourceAllocationError { message: String },

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigurationError(String),

    /// Parse error for responses
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Invalid input error
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// File error for file operations
    #[error("File error: {0}")]
    FileError(String),
}

/// Type alias for Result with `WinxError`
pub type Result<T> = std::result::Result<T, WinxError>;

/// Conversion from `anyhow::Error` to `WinxError`
impl From<anyhow::Error> for WinxError {
    fn from(error: anyhow::Error) -> Self {
        WinxError::CommandExecutionError(format!("{error}"))
    }
}

/// Advanced error recovery and suggestion options
pub struct ErrorRecovery;

impl ErrorRecovery {
    pub fn suggest(error: WinxError, _suggestion: &str) -> WinxError {
        error
    }

    pub fn param_error(field: &str, message: &str) -> WinxError {
        WinxError::ParameterValidationError {
            field: field.to_string(),
            message: message.to_string(),
        }
    }

    pub fn missing_param(field: &str, message: &str) -> WinxError {
        WinxError::MissingParameterError { field: field.to_string(), message: message.to_string() }
    }

    pub fn null_value(field: &str) -> WinxError {
        WinxError::NullValueError { field: field.to_string() }
    }
}

/// Enable cloning for `WinxError`
impl Clone for WinxError {
    fn clone(&self) -> Self {
        match self {
            Self::ShellInitializationError(msg) => Self::ShellInitializationError(msg.clone()),
            Self::WorkspacePathError(msg) => Self::WorkspacePathError(msg.clone()),
            Self::BashStateLockError(msg) => Self::BashStateLockError(msg.clone()),
            Self::BashStateNotInitialized => Self::BashStateNotInitialized,
            Self::CommandExecutionError(msg) => Self::CommandExecutionError(msg.clone()),
            Self::CommandNotAllowed(msg) => Self::CommandNotAllowed(msg.clone()),
            Self::ThreadIdMismatch(msg) => Self::ThreadIdMismatch(msg.clone()),
            Self::ArgumentParseError(msg) => Self::ArgumentParseError(msg.clone()),
            Self::FileAccessError { path, message } => {
                Self::FileAccessError { path: path.clone(), message: message.clone() }
            }
            Self::DeserializationError(msg) => Self::DeserializationError(msg.clone()),
            Self::SerializationError(msg) => Self::SerializationError(msg.clone()),
            Self::SearchReplaceSyntaxError(msg) => Self::SearchReplaceSyntaxError(msg.clone()),
            Self::SearchBlockNotFound(msg) => Self::SearchBlockNotFound(msg.clone()),
            Self::SearchBlockAmbiguous { block_content, match_count, suggestions } => {
                Self::SearchBlockAmbiguous {
                    block_content: block_content.clone(),
                    match_count: *match_count,
                    suggestions: suggestions.clone(),
                }
            }
            Self::SearchBlockConflict { conflicting_blocks, first_differing_block } => {
                Self::SearchBlockConflict {
                    conflicting_blocks: conflicting_blocks.clone(),
                    first_differing_block: first_differing_block.clone(),
                }
            }
            Self::SearchReplaceSyntaxErrorDetailed {
                message,
                line_number,
                block_type,
                suggestions,
            } => Self::SearchReplaceSyntaxErrorDetailed {
                message: message.clone(),
                line_number: *line_number,
                block_type: block_type.clone(),
                suggestions: suggestions.clone(),
            },
            Self::JsonParseError(msg) => Self::JsonParseError(msg.clone()),
            Self::FileTooLarge { path, size, max_size } => {
                Self::FileTooLarge { path: path.clone(), size: *size, max_size: *max_size }
            }
            Self::FileWriteError { path, message } => {
                Self::FileWriteError { path: path.clone(), message: message.clone() }
            }
            Self::DataLoadingError(msg) => Self::DataLoadingError(msg.clone()),
            Self::ParameterValidationError { field, message } => {
                Self::ParameterValidationError { field: field.clone(), message: message.clone() }
            }
            Self::MissingParameterError { field, message } => {
                Self::MissingParameterError { field: field.clone(), message: message.clone() }
            }
            Self::NullValueError { field } => Self::NullValueError { field: field.clone() },
            Self::RecoverableSuggestionError { message, suggestion } => {
                Self::RecoverableSuggestionError {
                    message: message.clone(),
                    suggestion: suggestion.clone(),
                }
            }
            Self::ContextSaveError(msg) => Self::ContextSaveError(msg.clone()),
            Self::CommandTimeout { command, timeout_seconds } => {
                Self::CommandTimeout { command: command.clone(), timeout_seconds: *timeout_seconds }
            }
            Self::InteractiveCommandDetected { command } => {
                Self::InteractiveCommandDetected { command: command.clone() }
            }
            Self::CommandAlreadyRunning { current_command, duration_seconds } => {
                Self::CommandAlreadyRunning {
                    current_command: current_command.clone(),
                    duration_seconds: *duration_seconds,
                }
            }
            Self::ProcessCleanupError { message } => {
                Self::ProcessCleanupError { message: message.clone() }
            }
            Self::BufferOverflow { size, max_size } => {
                Self::BufferOverflow { size: *size, max_size: *max_size }
            }
            Self::SessionRecoveryError { message } => {
                Self::SessionRecoveryError { message: message.clone() }
            }
            Self::ResourceAllocationError { message } => {
                Self::ResourceAllocationError { message: message.clone() }
            }
            Self::IoError(err) => Self::IoError(std::io::Error::new(err.kind(), err.to_string())),
            Self::ConfigurationError(msg) => Self::ConfigurationError(msg.clone()),
            Self::ParseError(msg) => Self::ParseError(msg.clone()),
            Self::InvalidInput(msg) => Self::InvalidInput(msg.clone()),
            Self::FileError(msg) => Self::FileError(msg.clone()),
            Self::PathSecurityError { path, message } => {
                Self::PathSecurityError { path: path.clone(), message: message.clone() }
            }
        }
    }
}