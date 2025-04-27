//! Tools module for the Winx application.
//!
//! This module contains all the tools that are exposed to the MCP client,
//! including shell initialization, command execution, file operations, etc.
//!
//! The `WinxService` struct is the main entry point for all tool calls.

pub mod bash_command;
pub mod file_write_or_edit;
pub mod initialize;
pub mod read_files;

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
        }
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

                // Provide a more user-friendly error message based on error type
                let error_message = format!(
                    "Error initializing shell environment: {}\n\n\
                    This might be due to issues with workspace path or permissions.\n\
                    Please try again with a valid workspace path.",
                    err
                );

                Err(McpError::internal_error(error_message, None))
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
                        "Shell environment not initialized. Please call Initialize first."
                            .to_string()
                    }
                    WinxError::ChatIdMismatch(_) => {
                        format!(
                            "{}\nPlease use the chat_id provided by the Initialize tool.",
                            err
                        )
                    }
                    WinxError::CommandNotAllowed(_) => {
                        format!("{}\nTry a different command or change the mode.", err)
                    }
                    _ => format!(
                        "Error executing command: {}\n\n\
                        This might be due to issues with the command syntax or permissions.",
                        err
                    ),
                };

                Err(McpError::internal_error(error_message, None))
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

        // We'll handle line range parsing in the implementation
        debug!("After parsing line ranges: {:?}", args);

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
                        "Shell environment not initialized. Please call Initialize first."
                            .to_string()
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}", path.display(), message)
                    }
                    _ => format!(
                        "Error reading files: {}\n\n\
                        This might be due to issues with file paths or permissions.",
                        err
                    ),
                };

                Err(McpError::internal_error(error_message, None))
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
                        "Shell environment not initialized. Please call Initialize first."
                            .to_string()
                    }
                    WinxError::ChatIdMismatch(_) => {
                        format!(
                            "{}\nPlease use the chat_id provided by the Initialize tool.",
                            err
                        )
                    }
                    WinxError::FileAccessError { path, message } => {
                        format!("File access error for {}: {}", path.display(), message)
                    }
                    WinxError::CommandNotAllowed(_) => {
                        format!("{}\nTry a different mode or check permissions.", err)
                    }
                    _ => format!(
                        "Error writing or editing file: {}\n\n\
                        This might be due to issues with file permissions or the file path.",
                        err
                    ),
                };

                Err(McpError::internal_error(error_message, None))
            }
        }
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
