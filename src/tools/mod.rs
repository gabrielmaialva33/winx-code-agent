//! Tools module for the Winx application.
//!
//! This module contains all the tools that are exposed to the MCP client,
//! including shell initialization, command execution, file operations, etc.
//!
//! The `WinxService` struct is the main entry point for all tool calls.

pub mod bash_command;
pub mod code_analyzer;
pub mod command_suggestions;
pub mod context_save;
pub mod file_write_or_edit;
pub mod initialize;
pub mod read_files;
pub mod read_image;

use anyhow::Result;
use rmcp::{model::*, tool, Error as McpError, ServerHandler};
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use crate::errors::WinxError;
use crate::state::bash_state::BashState;

/// Version of the MCP protocol implemented by this service
#[allow(dead_code)]
const MCP_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2024_11_05;

/// Main service implementation for Winx
///
/// This struct maintains the state of the shell environment and provides
/// methods for interacting with it through the MCP protocol.
#[derive(Debug, Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    bash_state: Arc<Mutex<Option<BashState>>>,
    /// Version information for the service
    version: String,
    /// Startup timestamp
    start_time: std::time::Instant,
}

impl WinxService {
    /// Create a new instance of the WinxService
    ///
    /// # Returns
    ///
    /// A new WinxService instance with an uninitialized bash state
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
            start_time: std::time::Instant::now(),
        }
    }

    /// Get the uptime of the service
    ///
    /// # Returns
    ///
    /// The duration since the service was started
    pub fn uptime(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// Get the version of the service
    ///
    /// # Returns
    ///
    /// The version string of the service
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get a reference to the bash state, locking the mutex
    ///
    /// # Returns
    ///
    /// A Result containing a MutexGuard for the bash state
    #[allow(dead_code)]
    fn lock_bash_state(&self) -> crate::errors::Result<std::sync::MutexGuard<Option<BashState>>> {
        self.bash_state
            .lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e)))
    }
}

#[tool(tool_box)]
impl WinxService {
    /// Initialize the shell environment
    ///
    /// This tool must be called before any other shell tools can be used.
    /// It sets up the shell environment with the specified workspace path
    /// and configuration.
    #[tool(description = "
- Always call this at the start of the conversation before using any of the shell tools from wcgw.
- Use `any_workspace_path` to initialize the shell in the appropriate project directory.
- If the user has mentioned a workspace or project root or any other file or folder use it to set `any_workspace_path`.
- If user has mentioned any files use `initial_files_to_read` to read, use absolute paths only (~ allowed)
- By default use mode \"wcgw\"
- In \"code-writer\" mode, set the commands and globs which user asked to set, otherwise use 'all'.
- Use type=\"first_call\" if it's the first call to this tool.
- Use type=\"user_asked_mode_change\" if in a conversation user has asked to change mode.
- Use type=\"reset_shell\" if in a conversation shell is not working after multiple tries.
- Use type=\"user_asked_change_workspace\" if in a conversation user asked to change workspace
")]
    async fn initialize(
        &self,
        #[tool(aggr)] args: crate::types::Initialize,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("Initialize tool received args: {:?}", args);

        // Log JSON serialization for debugging
        match serde_json::to_string(&args) {
            Ok(json) => debug!("Args as JSON: {}", json),
            Err(e) => tracing::error!("Failed to serialize args to JSON: {}", e),
        }

        // Call the implementation and measure execution time
        match initialize::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!("Initialize tool completed successfully in {:.2?}", elapsed);
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("Initialize tool error: {}", err);

                // Format error as "GOT EXCEPTION" message
                let error_message = match &err {
                    WinxError::WorkspacePathError(msg) => {
                        format!(
                            "Workspace path error: {}\n\n\
                            Please provide a valid absolute path to a directory or file.\n\
                            If the directory doesn't exist, it will be created for absolute paths.",
                            msg
                        )
                    }
                    WinxError::BashStateLockError(msg) => {
                        format!(
                            "Failed to access bash state: {}\n\n\
                            This is likely a temporary issue. Please try again.",
                            msg
                        )
                    }
                    _ => format!(
                        "Error initializing shell environment: {}\n\n\
                        This might be due to issues with workspace path or permissions.\n\
                        Please try again with a valid workspace path.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Execute a shell command
    ///
    /// This tool executes a command in the shell environment and returns the result.
    /// It can also be used to check the status of a running command or send input.
    #[tool(description = "
- Execute a bash command. This is stateful (beware with subsequent calls).
- Status of the command and the current working directory will always be returned at the end.
- The first or the last line might be `(...truncated)` if the output is too long.
- Always run `pwd` if you get any file or directory not found error to make sure you're not lost.
- Run long running commands in background using screen instead of \"&\".
- Do not use 'cat' to read files, use ReadFiles tool instead
- In order to check status of previous command, use `status_check` with empty command argument.
- Only command is allowed to run at a time. You need to wait for any previous command to finish before running a new one.
- Programs don't hang easily, so most likely explanation for no output is usually that the program is still running, and you need to check status again.
- Do not send Ctrl-c before checking for status till 10 minutes or whatever is appropriate for the program to finish.
")]
    async fn bash_command(
        &self,
        #[tool(aggr)] args: crate::types::BashCommand,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("BashCommand tool received args: {:?}", args);

        // Log JSON serialization for debugging
        match serde_json::to_string(&args) {
            Ok(json) => debug!("Args as JSON: {}", json),
            Err(e) => tracing::error!("Failed to serialize args to JSON: {}", e),
        }

        // Call the implementation and measure execution time
        match bash_command::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!("BashCommand tool completed successfully in {:.2?}", elapsed);
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("BashCommand tool error: {}", err);

                // Provide a user-friendly error message based on error type
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::ChatIdMismatch(_) => {
                        format!(
                            "{}\nPlease use the chat_id provided by the Initialize tool. This value must be included in all subsequent tool calls.",
                            err
                        )
                    }
                    WinxError::CommandNotAllowed(_) => {
                        format!("{}\nTry a different command or change the mode using Initialize with type=\"user_asked_mode_change\".", err)
                    }
                    WinxError::CommandExecutionError(msg) => {
                        if msg.contains("command not found") {
                            format!("Command not found: {}. Please check if the command is installed and in the PATH.", msg)
                        } else if msg.contains("permission denied") {
                            format!("Permission denied executing command: {}. Check file permissions.", msg)
                        } else {
                            format!("Error executing command: {}. Check command syntax and parameters.", msg)
                        }
                    }
                    _ => format!(
                        "Error executing command: {}\n\n\
                        This might be due to issues with the command syntax or permissions. Try running a simpler command first to verify the shell is working.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Read files
    ///
    /// This tool reads one or more files and returns their contents, with
    /// optional line numbers and line range filtering.
    #[tool(description = "
- Read full file content of one or more files.
- Provide absolute paths only (~ allowed)
- REQUIRED FIELD: file_paths - A list of file paths to read (cannot be empty)
- Only if the task requires line numbers understanding:
    - You may populate \"show_line_numbers_reason\" with your reason, by default null/empty means no line numbers are shown.
    - You may extract a range of lines. E.g., `/path/to/file:1-10` for lines 1-10. You can drop start or end like `/path/to/file:1-` or `/path/to/file:-10`
")]
    async fn read_files(
        &self,
        #[tool(aggr)] args: crate::types::ReadFiles,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("ReadFiles tool received args: {:?}", args);

        // Log JSON serialization for improved error diagnosis
        match serde_json::to_string(&args) {
            Ok(json) => debug!("ReadFiles args as JSON: {}", json),
            Err(e) => {
                tracing::error!("Failed to serialize ReadFiles args to JSON: {}", e);
                // For syntax errors, return a helpful error message
                if e.is_syntax() {
                    // Format JSON error as "GOT EXCEPTION" message
                    let error_message = format!("JSON syntax error in ReadFiles arguments: {}. Please check your tool argument format.", e);
                    let exception_message =
                        format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                    return Ok(CallToolResult::success(vec![Content::text(
                        exception_message,
                    )]));
                }
            }
        }

        // Validate file_paths is not empty (this should be handled by the deserializer, but double-check)
        if args.file_paths.is_empty() {
            tracing::error!("ReadFiles called with empty file_paths");
            let error_message =
                "file_paths cannot be empty. Please provide at least one file path to read.";
            let exception_message =
                format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
            return Ok(CallToolResult::success(vec![Content::text(
                exception_message,
            )]));
        }

        // We'll handle line range parsing in the implementation
        debug!("Processing files with args: {:?}", args);

        // Call the implementation and measure execution time
        match read_files::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!("ReadFiles tool completed successfully in {:.2?}", elapsed);
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("ReadFiles tool error: {}", err);

                // Provide a user-friendly error message based on error type
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}. Please verify the file exists, has the correct path and permissions.", path.display(), message)
                    }
                    _ => format!(
                        "Error reading files: {}\n\n\
                        This might be due to issues with file paths or permissions. Make sure to use absolute paths and check that files exist.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Write or edit a file
    ///
    /// This tool writes new content to a file or edits an existing file using
    /// search and replace blocks. It can handle full file content or partial edits.
    #[tool(description = "
- Writes or edits a file based on the percentage of changes.
- Use absolute path only (~ allowed).
- percentage_to_change is calculated as number of existing lines that will have some diff divided by total existing lines.
- First write down percentage of lines that need to be replaced in the file (between 0-100) in percentage_to_change
- percentage_to_change should be low if mostly new code is to be added. It should be high if a lot of things are to be replaced.
- If percentage_to_change > 50, provide full file content in file_content_or_search_replace_blocks
- If percentage_to_change <= 50, file_content_or_search_replace_blocks should be search/replace blocks.
")]
    async fn file_write_or_edit(
        &self,
        #[tool(aggr)] args: crate::types::FileWriteOrEdit,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("FileWriteOrEdit tool received args: {:?}", args);

        // Log JSON serialization for improved error diagnosis
        match serde_json::to_string(&args) {
            Ok(json) => debug!("FileWriteOrEdit args as JSON: {}", json),
            Err(e) => {
                tracing::error!("Failed to serialize FileWriteOrEdit args to JSON: {}", e);
                // For syntax errors, return a helpful error message
                if e.is_syntax() {
                    // Format JSON error as "GOT EXCEPTION" message
                    let error_message = format!("JSON syntax error in FileWriteOrEdit arguments: {}. Please check your tool argument format.", e);
                    let exception_message =
                        format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                    return Ok(CallToolResult::success(vec![Content::text(
                        exception_message,
                    )]));
                }
            }
        }

        // Call the implementation and measure execution time
        match file_write_or_edit::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!(
                    "FileWriteOrEdit tool completed successfully in {:.2?}",
                    elapsed
                );
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("FileWriteOrEdit tool error: {}", err);

                // Provide a user-friendly error message based on error type
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::ChatIdMismatch(_) => {
                        format!(
                            "{}\nPlease use the chat_id provided by the Initialize tool.",
                            err
                        )
                    }
                    WinxError::FileAccessError { path, message } => {
                        // Make error message more user-friendly and actionable
                        if message.contains("read the file at least once") {
                            format!("File access error for {}: {}. Please use ReadFiles tool to read this file first before attempting to modify it.", path.display(), message)
                        } else if message.contains("has changed since") {
                            format!("File access error for {}: {}. Please read the file again with ReadFiles before modifying it.", path.display(), message)
                        } else if message.contains("read more of the file") {
                            format!("File access error for {}: {}. Please use ReadFiles to read the remaining unread portions of this file.", path.display(), message)
                        } else {
                            format!("File access error for {}: {}", path.display(), message)
                        }
                    }
                    WinxError::CommandNotAllowed(_) => {
                        format!("{}\nTry a different mode or check permissions.", err)
                    }
                    WinxError::SearchReplaceSyntaxError(msg) => {
                        // Keep the message format consistent and avoid redundancy
                        format!("Search/replace syntax error: {}", msg)
                    }
                    WinxError::SearchBlockNotFound(msg) => {
                        // Since the message already contains the full error details,
                        // avoid adding redundant prefixes
                        msg.to_string()
                    }
                    WinxError::FileTooLarge {
                        path,
                        size,
                        max_size,
                    } => {
                        format!("File {} is too large: {} bytes (max {}). Try splitting the file or using a different approach.", 
                            path.display(), size, max_size)
                    }
                    WinxError::ArgumentParseError(msg) => {
                        format!(
                            "Failed to parse arguments: {}\nPlease check your input format.",
                            msg
                        )
                    }
                    _ => format!(
                        "Error writing or editing file: {}\n\n\
                        This might be due to issues with file permissions or the file path.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Save context information about a task
    ///
    /// This tool saves the description and contents of files matching the
    /// provided glob patterns to a file for knowledge transfer.
    #[tool(description = "
Saves provided description and file contents of all the relevant file paths or globs in a single text file.
- Provide random 3 word unqiue id or whatever user provided.
- Leave project path as empty string if no project path")]
    async fn context_save(
        &self,
        #[tool(aggr)] args: crate::types::ContextSave,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("ContextSave tool received args: {:?}", args);

        // Call the implementation and measure execution time
        match context_save::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!("ContextSave tool completed successfully in {:.2?}", elapsed);
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("ContextSave tool error: {}", err);

                // Provide a user-friendly error message based on error type
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}. Please verify the file exists and you have read permissions.", path.display(), message)
                    }
                    _ => format!(
                        "Error saving context: {}\n\n\
                        This might be due to issues with file paths or globs. Make sure all paths are valid and accessible.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Read an image file
    ///
    /// This tool reads an image file and returns its contents as base64-encoded
    /// data with the appropriate MIME type.
    #[tool(description = "Read an image from the shell.")]
    async fn read_image(
        &self,
        #[tool(aggr)] args: crate::types::ReadImage,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the args to debug what was received
        debug!("ReadImage tool received args: {:?}", args);

        // Call the implementation and measure execution time
        match read_image::handle_tool_call(&self.bash_state, args).await {
            Ok((media_type, data)) => {
                let elapsed = start_time.elapsed();
                info!("ReadImage tool completed successfully in {:.2?}", elapsed);
                Ok(CallToolResult::success(vec![Content::image(
                    data, media_type,
                )]))
            }
            Err(err) => {
                tracing::error!("ReadImage tool error: {}", err);

                // Provide a user-friendly error message based on error type
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}. Verify the file exists and is a valid image format.", path.display(), message)
                    }
                    _ => format!(
                        "Error reading image: {}\n\n\
                        This might be due to issues with the file path or format. Make sure the file is a valid image (jpg, png, gif, etc.) and you have read permissions.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Analyze code for issues and suggestions
    ///
    /// This tool analyzes a code file for issues, suggestions, and complexity metrics.
    #[tool(description = "
Analyze code for issues, suggestions, and complexity metrics.
- Identifies potential bugs, security issues, and code smells
- Provides suggestions for code improvement
- Calculates complexity metrics
- Supports multiple programming languages
- Helps maintain high code quality and prevent bugs
")]
    async fn code_analyzer(
        &self,
        #[tool(aggr)] args: crate::types::CodeAnalysis,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the call for debugging
        debug!("CodeAnalyzer tool call with args: {:?}", args);

        // Convert to internal parameter type
        let params = crate::tools::code_analyzer::CodeAnalysisParams {
            file_path: args.file_path,
            language: args.language,
            analysis_depth: args.analysis_depth,
            include_complexity: args.include_complexity,
            include_suggestions: args.include_suggestions,
            show_code_snippets: args.show_code_snippets,
            analyze_dependencies: args.analyze_dependencies,
            chat_id: args.chat_id,
        };

        // Call the implementation and measure execution time
        match code_analyzer::handle_tool_call(&self.bash_state, params).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!(
                    "CodeAnalyzer tool completed successfully in {:.2?}",
                    elapsed
                );
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("CodeAnalyzer tool error: {}", err);

                // Create a user-friendly error message
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}. Please verify the file exists and has the correct path and permissions.", path.display(), message)
                    }
                    WinxError::ChatIdMismatch(_) => {
                        format!(
                            "{}\nPlease use the chat_id provided by the Initialize tool.",
                            err
                        )
                    }
                    _ => format!(
                        "Error analyzing code: {}\n\n\
                        This might be due to issues with the file or the analysis engine.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Get intelligent command suggestions
    ///
    /// This tool provides command suggestions based on command history,
    /// context, and patterns observed in previous usage.
    #[tool(description = "
Get intelligent command suggestions based on command history and context.
- Provides command suggestions based on partial input and command history
- Learns from your command usage patterns over time
- Can provide explanations of suggested commands
- Tailors suggestions to the current working directory and previous commands
")]
    async fn command_suggestions(
        &self,
        #[tool(aggr)] args: crate::types::CommandSuggestions,
    ) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Log the call for debugging
        debug!("CommandSuggestions tool call with args: {:?}", args);

        // Call the implementation and measure execution time
        match command_suggestions::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => {
                let elapsed = start_time.elapsed();
                info!(
                    "CommandSuggestions tool completed successfully in {:.2?}",
                    elapsed
                );
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(err) => {
                tracing::error!("CommandSuggestions tool error: {}", err);

                // Create a user-friendly error message
                let error_message = match &err {
                    WinxError::BashStateNotInitialized => {
                        "Shell environment not initialized. Please call Initialize first with type=\"first_call\" and a valid workspace path."
                            .to_string()
                    }
                    WinxError::BashStateLockError(msg) => {
                        format!("Failed to access bash state: {}. This is likely a temporary issue. Please try again.", msg)
                    }
                    _ => format!(
                        "Error getting command suggestions: {}\n\n\
                        This might be due to an issue with the pattern analyzer.",
                        err
                    ),
                };

                // Return as successful response with GOT EXCEPTION prefix
                let exception_message =
                    format!("GOT EXCEPTION while calling tool. Error: {}", error_message);
                Ok(CallToolResult::success(vec![Content::text(
                    exception_message,
                )]))
            }
        }
    }

    /// Get agent status
    ///
    /// This tool returns information about the agent's current status,
    /// including uptime, version, and resource usage.
    #[tool(description = "
Get status information about the Winx agent.
- Returns uptime, version, and resource usage information
- No parameters required
")]
    async fn agent_status(&self) -> Result<CallToolResult, McpError> {
        // Start timing for performance monitoring
        let start_time = std::time::Instant::now();

        // Measure approximate memory usage
        let memory_usage = match std::process::Command::new("ps")
            .args(["o", "rss=", "-p", &std::process::id().to_string()])
            .output()
        {
            Ok(output) => {
                let output_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if let Ok(kb) = output_str.parse::<u64>() {
                    format!("{:.2} MB", kb as f64 / 1024.0)
                } else {
                    "Unknown".to_string()
                }
            }
            Err(_) => "Unknown".to_string(),
        };

        // Get number of active sessions
        let active_sessions = {
            let bash_state_guard = self.bash_state.lock().map_err(|e| {
                McpError::internal_error(format!("Failed to lock bash state: {}", e), None)
            })?;

            match &*bash_state_guard {
                Some(_) => 1,
                None => 0,
            }
        };

        // Get system load if available
        let system_load = match std::fs::read_to_string("/proc/loadavg") {
            Ok(load_str) => {
                let parts: Vec<&str> = load_str.split_whitespace().take(3).collect();
                if parts.len() >= 3 {
                    format!("{} {} {}", parts[0], parts[1], parts[2])
                } else {
                    "Unknown".to_string()
                }
            }
            Err(_) => {
                // Try using uptime for macOS/BSD
                match std::process::Command::new("uptime").output() {
                    Ok(output) => {
                        let output_str = String::from_utf8_lossy(&output.stdout);
                        if let Some(load_part) = output_str.split("load average:").nth(1) {
                            load_part.trim().to_string()
                        } else {
                            "Unknown".to_string()
                        }
                    }
                    Err(_) => "Unknown".to_string(),
                }
            }
        };

        // Get current working directory
        let cwd = match std::env::current_dir() {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(_) => "Unknown".to_string(),
        };

        // Format the status report
        let status_report = format!(
            "## Winx Agent Status\n\n\
            - **Version**: {}\n\
            - **Uptime**: {:.2?}\n\
            - **Memory Usage**: {}\n\
            - **System Load**: {}\n\
            - **Active Sessions**: {}\n\
            - **Working Directory**: {}\n\
            - **MCP Protocol**: {}\n\n\
            Status check completed in {:.2?}",
            self.version(),
            self.uptime(),
            memory_usage,
            system_load,
            active_sessions,
            cwd,
            format!("{:?}", MCP_PROTOCOL_VERSION),
            start_time.elapsed()
        );

        info!(
            "AgentStatus tool completed successfully in {:.2?}",
            start_time.elapsed()
        );
        Ok(CallToolResult::success(vec![Content::text(status_report)]))
    }
}

#[tool(tool_box)]
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "This server provides shell access and file handling capabilities".to_string(),
            ),
        }
    }
}
