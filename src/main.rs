mod bash;
mod tools;
mod types;

use std::sync::Arc;

use anyhow::Result;
use rmcp::{
    Error as McpError, RoleServer, ServerHandler, const_string, model::*, 
    service::RequestContext, tool, ServiceExt, transport::stdio,
};
use serde_json::json;
use tokio::sync::Mutex;
use tracing_subscriber::{self, EnvFilter};

use crate::bash::{BashState, Context, SimpleConsole};
use crate::tools::{initialize_tool, bash_command_tool};
use crate::types::*;

#[derive(Clone)]
struct WinxService {
    ctx: Arc<Context>,
}

impl WinxService {
    fn new() -> Result<Self> {
        let bash_state = BashState::new(
            Box::new(SimpleConsole),
            "",
            None,
            None,
            None,
            None,
            None,
        )?;
        
        Ok(Self {
            ctx: Arc::new(Context::new(bash_state)),
        })
    }
}

#[tool(tool_box)]
impl WinxService {
    #[tool(description = "Initialize the shell environment with a workspace path and mode")]
    async fn initialize(
        &self,
        #[tool(aggr)] init_params: Initialize,
    ) -> Result<CallToolResult, McpError> {
        let result = initialize_tool(&self.ctx, &init_params).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "Execute a bash command in the shell environment")]
    async fn bash_command(
        &self,
        #[tool(aggr)] bash_cmd: BashCommand,
    ) -> Result<CallToolResult, McpError> {
        let result = bash_command_tool(&self.ctx, &bash_cmd).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

#[tool(tool_box)]
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some("This server provides tools for working with a bash shell environment. Use the 'initialize' tool to set up the environment and the 'bashCommand' tool to execute commands.".to_string()),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        Err(McpError::resource_not_found(
            "resource_not_found",
            Some(json!({
                "uri": request.uri
            })),
        ))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult {
            next_cursor: None,
            prompts: vec![],
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        Err(McpError::invalid_params("prompt not found", None))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: Vec::new(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting Winx MCP server");

    // Create the service
    let service = WinxService::new()?;

    // Serve using stdio
    let service = service.serve(stdio()).await?;

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}
