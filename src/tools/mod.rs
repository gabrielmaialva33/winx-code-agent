pub mod initialize;

use anyhow::Result;
use rmcp::{model::*, tool, Error as McpError, ServerHandler};
use serde_json;
use std::sync::{Arc, Mutex};

use crate::state::bash_state::BashState;

#[derive(Debug, Clone)]
pub struct WinxService {
    bash_state: Arc<Mutex<Option<BashState>>>,
}

impl WinxService {
    pub fn new() -> Self {
        Self {
            bash_state: Arc::new(Mutex::new(None)),
        }
    }
}

#[tool(tool_box)]
impl WinxService {
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
        // Log the args to debug what was received, with additional JSON representation
        tracing::debug!("Initialize tool received args: {:?}", args);
        
        // Log JSON serialization for debugging
        match serde_json::to_string(&args) {
            Ok(json) => tracing::debug!("Args as JSON: {}", json),
            Err(e) => tracing::error!("Failed to serialize args to JSON: {}", e),
        }

        match initialize::handle_tool_call(&self.bash_state, args).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(err) => {
                tracing::error!("Initialize tool error: {}", err);

                // Provide a more user-friendly error message
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
