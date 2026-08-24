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

    /// Error when a command fails for an internal execution reason.
    #[error("Command execution failed: {0}")]
    CommandExecutionError(String),

    /// A status/input action targeted a shell with no running command.
    #[error("{0}")]
    NoActiveCommand(String),

    /// A background command identifier is unknown or has already exited.
    #[error("{0}")]
    BackgroundSessionNotFound(String),

    /// An interactive input action was supplied without any payload.
    #[error("Failure: {action} cannot be empty")]
    EmptyInteractiveInput { action: String },

    /// Interactive bytes were sent to an idle shell instead of a running program.
    #[error("{0}")]
    InteractiveTargetNotRunning(String),

    /// Error when parsing arguments
    #[error("Failed to parse arguments: {0}")]
    ArgumentParseError(String),

    /// Error when trying to access a file or directory
    #[error("File access error for {path}: {message}")]
    FileAccessError { path: PathBuf, message: String },

    /// Security error - path traversal or symlink escape attempt
    #[error("Security violation: {message}")]
    PathSecurityError { path: PathBuf, message: String },

    /// A model-owned helper violated the managed `.winx/tmp/<session>` contract.
    #[error("Temporary artifact policy rejected {path}: {message}")]
    TemporaryArtifactPolicy { path: PathBuf, temporary_artifact_dir: PathBuf, message: String },

    /// Bash created dynamic helpers that could not be projected safely before
    /// execution and the active managed session is now over budget.
    #[error(
        "Temporary artifact budget exceeded in {temporary_artifact_dir}: workspace {total_bytes}/{max_total_bytes} bytes; session {session_files} files / {session_bytes} bytes (limits: {max_session_files} files / {max_session_bytes} bytes; largest file {largest_file_bytes} bytes, limit {max_file_bytes}). Run an explicit cleanup-only BashCommand against $WINX_TEMP_DIR before continuing."
    )]
    TemporaryArtifactBudgetExceeded {
        temporary_artifact_dir: PathBuf,
        total_bytes: u64,
        max_total_bytes: u64,
        session_bytes: u64,
        max_session_bytes: u64,
        session_files: usize,
        max_session_files: usize,
        largest_file_bytes: u64,
        max_file_bytes: u64,
    },

    /// A valid delivery policy was paired with an action it cannot represent.
    #[error(
        "wait_policy={wait_policy} is not valid for BashCommand action {action}; use until_complete only with a finite foreground command"
    )]
    InvalidWaitPolicyForAction { wait_policy: String, action: String },

    /// Syntax navigation over derived helpers exceeded its bounded session budget.
    #[error("Derived CodeMap budget rejected {path}: {message}")]
    DerivedCodeMapBudget {
        path: PathBuf,
        temporary_artifact_dir: PathBuf,
        calls_used: usize,
        calls_limit: usize,
        unique_files_used: usize,
        unique_files_limit: usize,
        message: String,
    },

    /// Error when a command is not allowed in the current mode
    #[error("Command not allowed: {0}")]
    CommandNotAllowed(String),

    /// Error when chat IDs don't match
    #[error("Thread ID mismatch: {0}")]
    ThreadIdMismatch(String),

    /// A remote stateful call omitted the explicit workspace half of its
    /// session binding.
    #[error(
        "Remote session binding is incomplete for thread_id `{thread_id}`: pass the exact workspace_root returned by Initialize. No operation was executed. workspace_root identifies the project session; it does not restrict target paths."
    )]
    WorkspaceBindingRequired { thread_id: String },

    /// The supplied workspace and thread cannot belong to the same affinity
    /// key, so executing the request could select another project's shell.
    #[error(
        "Session coherence check failed: thread_id `{thread_id}` does not belong to workspace_root `{workspace_root}`. No operation was executed. Initialize that workspace and preserve the returned thread_id/workspace_root pair."
    )]
    WorkspaceThreadMismatch { thread_id: String, workspace_root: PathBuf },

    /// The requested binding disagrees with the workspace stored in the
    /// selected durable session.
    #[error(
        "Session coherence check failed: thread_id `{thread_id}` is bound to `{bound_workspace}`, not `{requested_workspace}`. No operation was executed. Initialize the intended workspace and use its returned pair."
    )]
    WorkspaceBindingMismatch {
        thread_id: String,
        requested_workspace: PathBuf,
        bound_workspace: PathBuf,
    },

    /// Remote sessions never change project identity in place. A new first
    /// call gives the target workspace its own coherent session binding.
    #[error(
        "Remote workspace changes require a new session binding. No operation was executed. Call Initialize with type=\"first_call\" for `{workspace_root}` and use the new thread_id/workspace_root pair."
    )]
    WorkspaceChangeRequiresNewSession { workspace_root: PathBuf },

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

    /// Path-aware context for a planning failure in an atomic multi-file edit.
    /// The boxed source preserves the original recovery class instead of
    /// flattening it into an argument/string error.
    #[error(
        "MultiFileEdit aborted before writing anything - file {index} ({path}) failed validation: {source}"
    )]
    MultiFilePlanError { index: usize, path: PathBuf, source: Box<WinxError> },

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
        // `{:#}` renders the full anyhow context chain inline ("failed X: caused
        // by Y: root Z") instead of just the outermost message, so a PTY-spawn or
        // persistence failure keeps its real cause instead of a bare one-liner.
        WinxError::CommandExecutionError(format!("{error:#}"))
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
    #[allow(clippy::too_many_lines)] // exhaustive variant-preserving clone
    fn clone(&self) -> Self {
        match self {
            Self::ShellInitializationError(msg) => Self::ShellInitializationError(msg.clone()),
            Self::WorkspacePathError(msg) => Self::WorkspacePathError(msg.clone()),
            Self::BashStateLockError(msg) => Self::BashStateLockError(msg.clone()),
            Self::BashStateNotInitialized => Self::BashStateNotInitialized,
            Self::CommandExecutionError(msg) => Self::CommandExecutionError(msg.clone()),
            Self::NoActiveCommand(msg) => Self::NoActiveCommand(msg.clone()),
            Self::BackgroundSessionNotFound(msg) => Self::BackgroundSessionNotFound(msg.clone()),
            Self::EmptyInteractiveInput { action } => {
                Self::EmptyInteractiveInput { action: action.clone() }
            }
            Self::InteractiveTargetNotRunning(msg) => {
                Self::InteractiveTargetNotRunning(msg.clone())
            }
            Self::CommandNotAllowed(msg) => Self::CommandNotAllowed(msg.clone()),
            Self::ThreadIdMismatch(msg) => Self::ThreadIdMismatch(msg.clone()),
            Self::WorkspaceBindingRequired { thread_id } => {
                Self::WorkspaceBindingRequired { thread_id: thread_id.clone() }
            }
            Self::WorkspaceThreadMismatch { thread_id, workspace_root } => {
                Self::WorkspaceThreadMismatch {
                    thread_id: thread_id.clone(),
                    workspace_root: workspace_root.clone(),
                }
            }
            Self::WorkspaceBindingMismatch { thread_id, requested_workspace, bound_workspace } => {
                Self::WorkspaceBindingMismatch {
                    thread_id: thread_id.clone(),
                    requested_workspace: requested_workspace.clone(),
                    bound_workspace: bound_workspace.clone(),
                }
            }
            Self::WorkspaceChangeRequiresNewSession { workspace_root } => {
                Self::WorkspaceChangeRequiresNewSession { workspace_root: workspace_root.clone() }
            }
            Self::ArgumentParseError(msg) => Self::ArgumentParseError(msg.clone()),
            Self::FileAccessError { path, message } => {
                Self::FileAccessError { path: path.clone(), message: message.clone() }
            }
            Self::TemporaryArtifactPolicy { path, temporary_artifact_dir, message } => {
                Self::TemporaryArtifactPolicy {
                    path: path.clone(),
                    temporary_artifact_dir: temporary_artifact_dir.clone(),
                    message: message.clone(),
                }
            }
            Self::TemporaryArtifactBudgetExceeded {
                temporary_artifact_dir,
                total_bytes,
                max_total_bytes,
                session_bytes,
                max_session_bytes,
                session_files,
                max_session_files,
                largest_file_bytes,
                max_file_bytes,
            } => Self::TemporaryArtifactBudgetExceeded {
                temporary_artifact_dir: temporary_artifact_dir.clone(),
                total_bytes: *total_bytes,
                max_total_bytes: *max_total_bytes,
                session_bytes: *session_bytes,
                max_session_bytes: *max_session_bytes,
                session_files: *session_files,
                max_session_files: *max_session_files,
                largest_file_bytes: *largest_file_bytes,
                max_file_bytes: *max_file_bytes,
            },
            Self::InvalidWaitPolicyForAction { wait_policy, action } => {
                Self::InvalidWaitPolicyForAction {
                    wait_policy: wait_policy.clone(),
                    action: action.clone(),
                }
            }
            Self::DerivedCodeMapBudget {
                path,
                temporary_artifact_dir,
                calls_used,
                calls_limit,
                unique_files_used,
                unique_files_limit,
                message,
            } => Self::DerivedCodeMapBudget {
                path: path.clone(),
                temporary_artifact_dir: temporary_artifact_dir.clone(),
                calls_used: *calls_used,
                calls_limit: *calls_limit,
                unique_files_used: *unique_files_used,
                unique_files_limit: *unique_files_limit,
                message: message.clone(),
            },
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
            Self::MultiFilePlanError { index, path, source } => Self::MultiFilePlanError {
                index: *index,
                path: path.clone(),
                source: Box::new((**source).clone()),
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
