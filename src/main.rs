mod bash;
mod tools;
mod types;

use std::sync::Arc;

use anyhow::Result;
use rmcp::{
    const_string, model::*, service::RequestContext, tool, transport::stdio, Error as McpError,
    RoleServer, ServerHandler, ServiceExt,
};
use serde_json::json;
use tokio::sync::Mutex;
use tracing_subscriber::{self, EnvFilter};

use crate::bash::{BashState, Context, SimpleConsole};
use crate::tools::{bash_command_tool, initialize_tool};
use crate::types::*;

#[derive(Clone)]
struct WinxService {
    ctx: Arc<Context>,
}

impl WinxService {
    fn new() -> Result<Self> {
        let bash_state = BashState::new(Box::new(SimpleConsole), "", None, None, None, None, None)?;

        Ok(Self {
            ctx: Arc::new(Context::new(bash_state)),
        })
    }
}

#[tool(tool_box)]
impl WinxService {
    #[tool(description = "
- Always call this at the start of the conversation before using any of the shell tools from winx.
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
        #[tool(aggr)] init_params: Initialize,
    ) -> Result<CallToolResult, McpError> {
        let result = initialize_tool(&self.ctx, &init_params)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

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
        #[tool(aggr)] bash_cmd: BashCommand,
    ) -> Result<CallToolResult, McpError> {
        let result = bash_command_tool(&self.ctx, &bash_cmd)
            .await
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
            instructions: Some(
                "Winx is a Rust implementation of WCGW (What Could Go Wrong) shell tools.\n\n\
                Use the 'initialize' tool to set up the environment with a workspace path and mode before using any other tools.\n\
                Then use the 'bashCommand' tool to execute shell commands in the initialized environment.\n\n\
                Always start by initializing a workspace path with the relevant mode ('wcgw', 'architect', or 'code_writer').\n\
                Always check command outputs for errors and use 'pwd' if you encounter any file or directory not found errors.\n\
                Use absolute paths for files (~ is allowed) and never run multiple commands at once.".to_string()
            ),
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
