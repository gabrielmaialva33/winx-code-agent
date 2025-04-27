//! Tools module for the Winx application.
//!
//! This module contains all the tools that are exposed to the MCP client,
//! including shell initialization, command execution, file operations, etc.
//!
//! The `WinxService` struct is the main entry point for all tool calls.

pub mod initialize;

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
