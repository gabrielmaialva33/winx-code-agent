pub mod initialize;

use anyhow::Result;
use rmcp::{model::*, tool, Error as McpError, ServerHandler};
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
    #[tool(description = "Initialize the shell environment")]
    async fn initialize(
        &self,
        #[tool(aggr)] args: crate::types::Initialize,
    ) -> Result<CallToolResult, McpError> {
        // Log the args to debug what was received
        tracing::debug!("Initialize tool received args: {:?}", args);

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
