use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

/// Errors that can occur in the Winx application
#[derive(Error, Debug)]
pub enum WinxError {
    /// Error when initializing the shell
    #[error("Failed to initialize shell: {0}")]
    ShellInitializationError(Arc<String>),

    /// Error when operating on a workspace path
    #[error("Workspace path error: {0}")]
    WorkspacePathError(Arc<String>),

    /// Error when locking the bash state
    #[error("Failed to lock the bash state: {0}")]
    BashStateLockError(Arc<String>),

    /// Error when the bash state is not initialized
    #[error("Bash state not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path.")]
    BashStateNotInitialized,

    /// Error when a command fails to execute
    #[error("Command execution failed: {0}")]
    CommandExecutionError(Arc<String>),

    /// Error when parsing arguments
    #[error("Failed to parse arguments: {0}")]
    ArgumentParseError(Arc<String>),

    /// Error when trying to access a file or directory
    #[error("File access error for {path}: {message}")]
    FileAccessError { path: PathBuf, message: Arc<String> },

    /// Error when a command is not allowed in the current mode
    #[error("Command not allowed: {0}")]
    CommandNotAllowed(Arc<String>),

    /// Error when chat IDs don't match
    #[error("Chat ID mismatch: {0}")]
    ChatIdMismatch(Arc<String>),

    /// Error when deserializing data
    #[error("Deserialization error: {0}")]
    DeserializationError(Arc<String>),

    /// Error when serializing data
    #[error("Serialization error: {0}")]
    SerializationError(Arc<String>),

    /// Error in the search/replace format
    #[error("Search/replace syntax error: {0}")]
    SearchReplaceSyntaxError(Arc<String>),

    /// Error when search block is not found in content
    #[error("Search block not found in content: {0}")]
    SearchBlockNotFound(Arc<String>),

    /// Error when search block matches multiple locations (WCGW-style)
    #[error("Search block matched multiple times")]
    SearchBlockAmbiguous {
        block_content: Arc<String>,
        match_count: usize,
        suggestions: Arc<Vec<String>>,
    },

    /// Error when search blocks have conflicting matches
    #[error("Multiple search blocks have conflicting matches")]
    SearchBlockConflict {
        conflicting_blocks: Arc<Vec<String>>,
        first_differing_block: Option<Arc<String>>,
    },

    /// Enhanced search/replace syntax error with detailed context
    #[error("Search/replace syntax error: {message}")]
    SearchReplaceSyntaxErrorDetailed {
        message: Arc<String>,
        line_number: Option<usize>,
        block_type: Option<Arc<String>>,
        suggestions: Arc<Vec<String>>,
    },

    /// Error when JSON parsing fails
    #[error("Invalid JSON: {0}")]
    JsonParseError(Arc<String>),

    /// Error when a file is too large for operation
    #[error("File {path} is too large: {size} bytes (max {max_size})")]
    FileTooLarge {
        path: PathBuf,
        size: u64,
        max_size: u64,
    },

    /// Error when writing to a file
    #[error("Failed to write file {path}: {message}")]
    FileWriteError { path: PathBuf, message: Arc<String> },

    /// Error loading data
    #[error("Failed to load data: {0}")]
    DataLoadingError(Arc<String>),

    /// Parameter validation error
    #[error("Invalid parameter: {field} - {message}")]
    ParameterValidationError { field: Arc<String>, message: Arc<String> },

    /// Required parameter missing error
    #[error("Required parameter missing: {field} - {message}")]
    MissingParameterError { field: Arc<String>, message: Arc<String> },

    /// Null or undefined value error
    #[error("Null or undefined value where object expected: {field}")]
    NullValueError { field: Arc<String> },

    /// Recovery suggestion error with potential solutions
    #[error("{message} - {suggestion}")]
    RecoverableSuggestionError { message: Arc<String>, suggestion: Arc<String> },

    /// Context save error
    #[error("Context save error: {0}")]
    ContextSaveError(Arc<String>),

    /// Command timeout error
    #[error("Command timed out after {timeout_seconds}s: {command}")]
    CommandTimeout {
        command: Arc<String>,
        timeout_seconds: u64,
    },

    /// Interactive command detected error
    #[error(
        "Interactive command detected: {command}. Use appropriate flags or consider alternatives."
    )]
    InteractiveCommandDetected { command: Arc<String> },

    /// Command already running error
    #[error("A command is already running: '{current_command}' (for {duration_seconds:.1}s). Use status_check, send_text, or interrupt.")]
    CommandAlreadyRunning {
        current_command: Arc<String>,
        duration_seconds: f64,
    },

    /// Process cleanup error
    #[error("Failed to cleanup process: {message}")]
    ProcessCleanupError { message: Arc<String> },

    /// Buffer overflow error
    #[error("Command output exceeded maximum size: {size} bytes (max {max_size})")]
    BufferOverflow { size: usize, max_size: usize },

    /// Session recovery error
    #[error("Failed to recover bash session: {message}")]
    SessionRecoveryError { message: Arc<String> },

    /// Resource allocation error
    #[error("Resource allocation failed: {message}")]
    ResourceAllocationError { message: Arc<String> },

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// NVIDIA API error
    #[error("NVIDIA API error: {0}")]
    ApiError(Arc<String>),

    /// Network error for HTTP requests
    #[error("Network error: {0}")]
    NetworkError(Arc<String>),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigurationError(Arc<String>),

    /// Parse error for responses
    #[error("Parse error: {0}")]
    ParseError(Arc<String>),

    /// Invalid input error
    #[error("Invalid input: {0}")]
    InvalidInput(Arc<String>),

    /// File error for file operations
    #[error("File error: {0}")]
    FileError(Arc<String>),

    /// AI provider error
    #[error("AI error: {0}")]
    AIError(Arc<String>),
}

/// Type alias for Result with WinxError
pub type Result<T> = std::result::Result<T, WinxError>;

/// Conversion from anyhow::Error to WinxError
impl From<anyhow::Error> for WinxError {
    fn from(error: anyhow::Error) -> Self {
        WinxError::CommandExecutionError(Arc::new(format!("{}", error)))
    }
}

/// Extension trait to convert anyhow errors to WinxError
#[allow(dead_code)]
pub trait AnyhowErrorExt {
    /// Convert the error to a WinxError
    fn to_winx_error(self, default_message: &str) -> WinxError;
}

impl AnyhowErrorExt for anyhow::Error {
    fn to_winx_error(self, default_message: &str) -> WinxError {
        // First, try to downcast if it's already a WinxError
        if let Some(err) = self.downcast_ref::<WinxError>() {
            return err.clone();
        }

        // Get error string for pattern matching
        let err_string = self.to_string();
        let root_cause = self.root_cause().to_string();

        // Classify based on error content
        if root_cause.contains("command not found") {
            WinxError::CommandExecutionError(Arc::new(format!("Command not found: {}", self)))
        } else if root_cause.contains("permission denied") {
            WinxError::CommandExecutionError(Arc::new(format!("Permission denied: {}", self)))
        } else if err_string.contains("bash state") {
            WinxError::BashStateLockError(Arc::new(err_string))
        } else if err_string.contains("workspace") || err_string.contains("directory") {
            WinxError::WorkspacePathError(Arc::new(err_string))
        } else if err_string.contains("command") {
            WinxError::CommandExecutionError(Arc::new(err_string))
        } else if err_string.contains("null") || err_string.contains("undefined") {
            WinxError::NullValueError {
                field: Arc::new("unknown".to_string()),
            }
        } else if err_string.contains("parse") || err_string.contains("deserializ") {
            WinxError::DeserializationError(Arc::new(err_string))
        } else if err_string.contains("serialize") {
            WinxError::SerializationError(Arc::new(err_string))
        } else {
            WinxError::ShellInitializationError(Arc::new(format!("{}: {}", default_message, err_string)))
        }
    }
}

/// Helper function to create recoverable errors with suggestions
pub fn with_suggestion(error: WinxError, suggestion: &str) -> WinxError {
    match error {
        WinxError::FileAccessError { path, message } => WinxError::RecoverableSuggestionError {
            message: Arc::new(format!("File access error for {}: {}", path.display(), message)),
            suggestion: Arc::new(suggestion.to_string()),
        },
        WinxError::DeserializationError(msg) => WinxError::RecoverableSuggestionError {
            message: Arc::new(format!("Failed to parse input: {}", msg)),
            suggestion: Arc::new(suggestion.to_string()),
        },
        WinxError::NullValueError { field } => WinxError::RecoverableSuggestionError {
            message: Arc::new(format!("Null or undefined value found in field: {}", field)),
            suggestion: Arc::new(suggestion.to_string()),
        },
        WinxError::ParameterValidationError { field, message } => {
            WinxError::RecoverableSuggestionError {
                message: Arc::new(format!("Invalid parameter {}: {}", field, message)),
                suggestion: Arc::new(suggestion.to_string()),
            }
        }
        WinxError::MissingParameterError { field, message } => {
            WinxError::RecoverableSuggestionError {
                message: Arc::new(format!("Missing required parameter {}: {}", field, message)),
                suggestion: Arc::new(suggestion.to_string()),
            }
        }
        // For other error types, just add the suggestion but maintain the original error type
        _ => WinxError::RecoverableSuggestionError {
            message: Arc::new(format!("{}", error)),
            suggestion: Arc::new(suggestion.to_string()),
        },
    }
}

/// Advanced error recovery and suggestion options
pub struct ErrorRecovery;

impl ErrorRecovery {
    /// Create a recoverable error with suggestion
    pub fn suggest(error: WinxError, suggestion: &str) -> WinxError {
        with_suggestion(error, suggestion)
    }

    /// Attempt to recover from a parameter error with a default value
    pub fn with_default<T: Clone>(
        result: std::result::Result<T, WinxError>,
        default: T,
        context: &str,
    ) -> Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(e) => {
                tracing::warn!("Recovering from error in {}: {}", context, e);
                Ok(default)
            }
        }
    }

    /// Create a parameter validation error
    pub fn param_error(field: &str, message: &str) -> WinxError {
        WinxError::ParameterValidationError {
            field: Arc::new(field.to_string()),
            message: Arc::new(message.to_string()),
        }
    }

    /// Create a missing parameter error
    pub fn missing_param(field: &str, message: &str) -> WinxError {
        WinxError::MissingParameterError {
            field: Arc::new(field.to_string()),
            message: Arc::new(message.to_string()),
        }
    }

    /// Create a null value error
    pub fn null_value(field: &str) -> WinxError {
        WinxError::NullValueError {
            field: Arc::new(field.to_string()),
        }
    }

    /// Retry an operation with exponential backoff
    pub async fn retry<T, F, Fut>(
        operation: F,
        max_retries: usize,
        initial_delay_ms: u64,
        context: &str,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut delay_ms = initial_delay_ms;
        let mut attempt = 0;

        loop {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_retries {
                        tracing::error!(
                            "Retry failed after {} attempts in context '{}': {}",
                            attempt,
                            context,
                            e
                        );
                        return Err(e);
                    }

                    tracing::warn!(
                        "Attempt {} failed in context '{}': {}. Retrying in {}ms...",
                        attempt,
                        context,
                        e,
                        delay_ms
                    );

                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;

                    // Exponential backoff with jitter
                    delay_ms =
                        ((delay_ms as f64) * 1.5 * (0.8 + 0.4 * rand::random::<f64>())) as u64;
                }
            }
        }
    }

    /// Try to recover from common file system errors
    pub fn recover_fs_error(err: &WinxError) -> Option<String> {
        match err {
            WinxError::FileAccessError { path, message } => {
                if message.contains("No such file or directory") {
                    Some(format!(
                        "The file '{}' does not exist. Consider creating it first.",
                        path.display()
                    ))
                } else if message.contains("Permission denied") {
                    Some(format!("Permission denied for file '{}'. Check file permissions or use sudo if appropriate.", path.display()))
                } else if message.contains("Is a directory") {
                    Some(format!(
                        "'{}' is a directory, not a file. Specify a file path instead.",
                        path.display()
                    ))
                } else {
                    None
                }
            }
            WinxError::FileWriteError { path, message } => {
                if message.contains("No space left on device") {
                    Some(format!("No space left on device while writing to '{}'. Free up disk space and try again.", path.display()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Check if an error is potentially recoverable
    pub fn is_recoverable(err: &WinxError) -> bool {
        match err {
            WinxError::BashStateLockError(_) => true,
            WinxError::FileAccessError { .. } => true,
            WinxError::CommandExecutionError(msg) if msg.contains("timed out") => true,
            WinxError::RecoverableSuggestionError { .. } => true,
            _ => false,
        }
    }
}

/// Enable cloning for WinxError
impl Clone for WinxError {
    fn clone(&self) -> Self {
        match self {
            Self::ShellInitializationError(msg) => Self::ShellInitializationError(Arc::clone(msg)),
            Self::WorkspacePathError(msg) => Self::WorkspacePathError(Arc::clone(msg)),
            Self::BashStateLockError(msg) => Self::BashStateLockError(Arc::clone(msg)),
            Self::BashStateNotInitialized => Self::BashStateNotInitialized,
            Self::CommandExecutionError(msg) => Self::CommandExecutionError(Arc::clone(msg)),
            Self::CommandNotAllowed(msg) => Self::CommandNotAllowed(Arc::clone(msg)),
            Self::ChatIdMismatch(msg) => Self::ChatIdMismatch(Arc::clone(msg)),
            Self::ArgumentParseError(msg) => Self::ArgumentParseError(Arc::clone(msg)),
            Self::FileAccessError { path, message } => Self::FileAccessError {
                path: path.clone(),
                message: Arc::clone(message),
            },
            Self::DeserializationError(msg) => Self::DeserializationError(Arc::clone(msg)),
            Self::SerializationError(msg) => Self::SerializationError(Arc::clone(msg)),
            Self::SearchReplaceSyntaxError(msg) => Self::SearchReplaceSyntaxError(Arc::clone(msg)),
            Self::SearchBlockNotFound(msg) => Self::SearchBlockNotFound(Arc::clone(msg)),
            Self::SearchBlockAmbiguous {
                block_content,
                match_count,
                suggestions,
            } => Self::SearchBlockAmbiguous {
                block_content: Arc::clone(block_content),
                match_count: *match_count,
                suggestions: Arc::clone(suggestions),
            },
            Self::SearchBlockConflict {
                conflicting_blocks,
                first_differing_block,
            } => Self::SearchBlockConflict {
                conflicting_blocks: Arc::clone(conflicting_blocks),
                first_differing_block: first_differing_block.as_ref().map(|s| Arc::clone(s)),
            },
            Self::SearchReplaceSyntaxErrorDetailed {
                message,
                line_number,
                block_type,
                suggestions,
            } => Self::SearchReplaceSyntaxErrorDetailed {
                message: Arc::clone(message),
                line_number: *line_number,
                block_type: block_type.as_ref().map(|s| Arc::clone(s)),
                suggestions: Arc::clone(suggestions),
            },
            Self::JsonParseError(msg) => Self::JsonParseError(Arc::clone(msg)),
            Self::FileTooLarge {
                path,
                size,
                max_size,
            } => Self::FileTooLarge {
                path: path.clone(),
                size: *size,
                max_size: *max_size,
            },
            Self::FileWriteError { path, message } => Self::FileWriteError {
                path: path.clone(),
                message: Arc::clone(message),
            },
            Self::DataLoadingError(msg) => Self::DataLoadingError(Arc::clone(msg)),
            Self::ParameterValidationError { field, message } => Self::ParameterValidationError {
                field: Arc::clone(field),
                message: Arc::clone(message),
            },
            Self::MissingParameterError { field, message } => Self::MissingParameterError {
                field: Arc::clone(field),
                message: Arc::clone(message),
            },
            Self::NullValueError { field } => Self::NullValueError {
                field: Arc::clone(field),
            },
            Self::RecoverableSuggestionError {
                message,
                suggestion,
            } => Self::RecoverableSuggestionError {
                message: Arc::clone(message),
                suggestion: Arc::clone(suggestion),
            },
            Self::ContextSaveError(msg) => Self::ContextSaveError(Arc::clone(msg)),
            Self::CommandTimeout {
                command,
                timeout_seconds,
            } => Self::CommandTimeout {
                command: Arc::clone(command),
                timeout_seconds: *timeout_seconds,
            },
            Self::InteractiveCommandDetected { command } => Self::InteractiveCommandDetected {
                command: Arc::clone(command),
            },
            Self::CommandAlreadyRunning {
                current_command,
                duration_seconds,
            } => Self::CommandAlreadyRunning {
                current_command: Arc::clone(current_command),
                duration_seconds: *duration_seconds,
            },
            Self::ProcessCleanupError { message } => Self::ProcessCleanupError {
                message: Arc::clone(message),
            },
            Self::BufferOverflow { size, max_size } => Self::BufferOverflow {
                size: *size,
                max_size: *max_size,
            },
            Self::SessionRecoveryError { message } => Self::SessionRecoveryError {
                message: Arc::clone(message),
            },
            Self::ResourceAllocationError { message } => Self::ResourceAllocationError {
                message: Arc::clone(message),
            },
            Self::IoError(err) => Self::IoError(std::io::Error::new(err.kind(), err.to_string())),
            Self::ApiError(msg) => Self::ApiError(Arc::clone(msg)),
            Self::NetworkError(msg) => Self::NetworkError(Arc::clone(msg)),
            Self::ConfigurationError(msg) => Self::ConfigurationError(Arc::clone(msg)),
            Self::ParseError(msg) => Self::ParseError(Arc::clone(msg)),
            Self::InvalidInput(msg) => Self::InvalidInput(Arc::clone(msg)),
            Self::FileError(msg) => Self::FileError(Arc::clone(msg)),
            Self::AIError(msg) => Self::AIError(Arc::clone(msg)),
        }
    }
}
