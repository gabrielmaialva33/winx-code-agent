use std::fmt;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

/// Errors that can occur in the Winx application
#[derive(Debug, thiserror::Error)]
pub enum WinxError {
    /// Error when initializing the shell
    #[error("Failed to initialize shell: {message}")]
    ShellInitializationError { message: String },

    /// Error when operating on a workspace path
    #[error("Workspace path error: {message}")]
    WorkspacePathError { message: String },

    /// Error when locking the bash state
    #[error("Failed to lock the bash state: {message}")]
    BashStateLockError { message: Arc<String> },

    /// Error when the bash state is not initialized
    #[error(
        "Bash state not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
    )]
    BashStateNotInitialized,

    /// Error when a command fails to execute
    #[error("Command execution failed: {message}")]
    CommandExecutionError { message: Arc<String> },

    /// Error when parsing arguments
    #[error("Failed to parse arguments: {message}")]
    ArgumentParseError { message: Arc<String> },

    /// Error when trying to access a file or directory
    #[error("File access error for {path}: {message}")]
    FileAccessError { path: PathBuf, message: Arc<String> },

    /// Error when a command is not allowed in the current mode
    #[error("Command not allowed: {message}")]
    CommandNotAllowed { message: Arc<String> },

    /// Error when chat IDs don't match
    #[error("Chat ID mismatch: {message}")]
    ChatIdMismatch { message: Arc<String> },

    /// Error when deserializing data
    #[error("Deserialization error: {message}")]
    DeserializationError { message: Arc<String> },

    /// Error when serializing data
    #[error("Serialization error: {message}")]
    SerializationError { message: Arc<String> },

    /// Error in the search/replace format
    #[error("Search/replace syntax error: {message}")]
    SearchReplaceSyntaxError { message: Arc<String> },

    /// Error when search block is not found in content
    #[error("Search block not found in content: {message}")]
    SearchBlockNotFound { message: Arc<String> },

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
    #[error("Invalid JSON: {message}")]
    JsonParseError { message: Arc<String> },

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
    #[error("Failed to load data: {message}")]
    DataLoadingError { message: Arc<String> },

    /// Parameter validation error
    #[error("Invalid parameter: {field} - {message}")]
    ParameterValidationError {
        field: Arc<String>,
        message: Arc<String>,
    },

    /// Required parameter missing error
    #[error("Required parameter missing: {field} - {message}")]
    MissingParameterError {
        field: Arc<String>,
        message: Arc<String>,
    },

    /// Null or undefined value error
    #[error("Null or undefined value where object expected: {field}")]
    NullValueError { field: Arc<String> },

    /// Recovery suggestion error with potential solutions
    #[error("{message} - {suggestion}")]
    RecoverableSuggestionError {
        message: Arc<String>,
        suggestion: Arc<String>,
    },

    /// Context save error
    #[error("Context save error: {message}")]
    ContextSaveError { message: Arc<String> },

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
    #[error(
        "A command is already running: '{current_command}' (for {duration_seconds:.1}s). Use status_check, send_text, or interrupt."
    )]
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
    #[error("NVIDIA API error: {message}")]
    ApiError { message: Arc<String> },

    /// Network error for HTTP requests
    #[error("Network error: {message}")]
    NetworkError { message: Arc<String> },

    /// Configuration error
    #[error("Configuration error: {message}")]
    ConfigurationError { message: Arc<String> },

    /// Parse error for responses
    #[error("Parse error: {message}")]
    ParseError { message: Arc<String> },

    /// Invalid input error
    #[error("Invalid input: {message}")]
    InvalidInput { message: Arc<String> },

    /// File error for file operations
    #[error("File error: {message}")]
    FileError { message: Arc<String> },

    /// AI provider error
    #[error("AI error: {message}")]
    AIError { message: Arc<String> },
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
            WinxError::ShellInitializationError(Arc::new(format!(
                "{}: {}",
                default_message, err_string
            )))
        }
    }
}

/// Helper function to create recoverable errors with suggestions
pub fn with_suggestion(error: WinxError, suggestion: &str) -> WinxError {
    match error {
        WinxError::FileAccessError { path, message } => WinxError::RecoverableSuggestionError {
            message: Arc::new(format!(
                "File access error for {}: {}",
                path.display(),
                message
            )),
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
                    Some(format!(
                        "Permission denied for file '{}'. Check file permissions or use sudo if appropriate.",
                        path.display()
                    ))
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
                    Some(format!(
                        "No space left on device while writing to '{}'. Free up disk space and try again.",
                        path.display()
                    ))
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
            Self::ShellInitializationError { message } => Self::ShellInitializationError {
                message: message.clone(),
            },
            Self::WorkspacePathError { message } => Self::WorkspacePathError {
                message: message.clone(),
            },
            Self::BashStateLockError { message } => Self::BashStateLockError {
                message: message.clone(),
            },
            Self::BashStateNotInitialized => Self::BashStateNotInitialized,
            Self::CommandExecutionError { message } => Self::CommandExecutionError {
                message: message.clone(),
            },
            Self::CommandNotAllowed { message } => Self::CommandNotAllowed {
                message: message.clone(),
            },
            Self::ChatIdMismatch { message } => Self::ChatIdMismatch {
                message: message.clone(),
            },
            Self::ArgumentParseError { message } => Self::ArgumentParseError {
                message: message.clone(),
            },
            Self::FileAccessError { path, message } => Self::FileAccessError {
                path: path.clone(),
                message: message.clone(),
            },
            Self::DeserializationError { message } => Self::DeserializationError {
                message: message.clone(),
            },
            Self::SerializationError { message } => Self::SerializationError {
                message: message.clone(),
            },
            Self::SearchReplaceSyntaxError { message } => Self::SearchReplaceSyntaxError {
                message: message.clone(),
            },
            Self::SearchBlockNotFound { message } => Self::SearchBlockNotFound {
                message: message.clone(),
            },
            Self::SearchBlockAmbiguous {
                block_content,
                match_count,
                suggestions,
            } => Self::SearchBlockAmbiguous {
                block_content: block_content.clone(),
                match_count: *match_count,
                suggestions: suggestions.clone(),
            },
            Self::SearchBlockConflict {
                conflicting_blocks,
                first_differing_block,
            } => Self::SearchBlockConflict {
                conflicting_blocks: conflicting_blocks.clone(),
                first_differing_block: first_differing_block.clone(),
            },
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
            Self::JsonParseError { message } => Self::JsonParseError {
                message: message.clone(),
            },
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
                message: message.clone(),
            },
            Self::DataLoadingError { message } => Self::DataLoadingError {
                message: message.clone(),
            },
            Self::ParameterValidationError { field, message } => Self::ParameterValidationError {
                field: field.clone(),
                message: message.clone(),
            },
            Self::MissingParameterError { field, message } => Self::MissingParameterError {
                field: field.clone(),
                message: message.clone(),
            },
            Self::NullValueError { field } => Self::NullValueError {
                field: field.clone(),
            },
            Self::RecoverableSuggestionError {
                message,
                suggestion,
            } => Self::RecoverableSuggestionError {
                message: message.clone(),
                suggestion: suggestion.clone(),
            },
            Self::ContextSaveError { message } => Self::ContextSaveError {
                message: message.clone(),
            },
            Self::CommandTimeout {
                command,
                timeout_seconds,
            } => Self::CommandTimeout {
                command: command.clone(),
                timeout_seconds: *timeout_seconds,
            },
            Self::InteractiveCommandDetected { command } => Self::InteractiveCommandDetected {
                command: command.clone(),
            },
            Self::CommandAlreadyRunning {
                current_command,
                duration_seconds,
            } => Self::CommandAlreadyRunning {
                current_command: current_command.clone(),
                duration_seconds: *duration_seconds,
            },
            Self::ProcessCleanupError { message } => Self::ProcessCleanupError {
                message: message.clone(),
            },
            Self::BufferOverflow { size, max_size } => Self::BufferOverflow {
                size: *size,
                max_size: *max_size,
            },
            Self::SessionRecoveryError { message } => Self::SessionRecoveryError {
                message: message.clone(),
            },
            Self::ResourceAllocationError { message } => Self::ResourceAllocationError {
                message: message.clone(),
            },
            Self::IoError(err) => Self::IoError(std::io::Error::new(err.kind(), err.to_string())),
            Self::ApiError { message } => Self::ApiError {
                message: message.clone(),
            },
            Self::NetworkError { message } => Self::NetworkError {
                message: message.clone(),
            },
            Self::ConfigurationError { message } => Self::ConfigurationError {
                message: message.clone(),
            },
            Self::ParseError { message } => Self::ParseError {
                message: message.clone(),
            },
            Self::InvalidInput { message } => Self::InvalidInput {
                message: message.clone(),
            },
            Self::FileError { message } => Self::FileError {
                message: message.clone(),
            },
            Self::AIError { message } => Self::AIError {
                message: message.clone(),
            },
        }
    }
}
