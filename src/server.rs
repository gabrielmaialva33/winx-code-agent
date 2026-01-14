//! Winx MCP Server implementation using rmcp 0.12.0
//! Core MCP tools only - High performance shell and file management

use rmcp::{
    model::{
        Annotated, CallToolRequestParam, CallToolResult, Content, Implementation,
        ListResourcesResult, ListToolsResult, PaginatedRequestParam, ProtocolVersion, RawResource,
        ReadResourceRequestParam, ReadResourceResult, ResourceContents, ServerCapabilities,
        ServerInfo, Tool, ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::schema_for;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::state::BashState;
use crate::types::{
    BashCommand, ContextSave, FileWriteOrEdit, Initialize, ReadFiles, ReadImage,
};

/// Type alias for the shared bash state - uses `tokio::sync::Mutex` for async safety
pub type SharedBashState = Arc<Mutex<Option<BashState>>>;

/// Helper function to create JSON schema from schemars Schema
fn schema_to_input_schema<T: schemars::JsonSchema>() -> Arc<serde_json::Map<String, Value>> {
    let schema = schema_for!(T);
    let value = serde_json::to_value(schema).unwrap_or(Value::Object(serde_json::Map::new()));
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Winx service with shared bash state
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment (async-safe)
    pub bash_state: SharedBashState,
    /// Version information for the service
    pub version: String,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new `WinxService` instance
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// `ServerHandler` implementation
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-mcp-server".into(),
                version: self.version.clone(),
                title: Some("Winx High-Performance MCP".into()),
                icons: None,
                website_url: None,
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of MCP tools for shell and file management."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: vec![
                Tool {
                    name: "Initialize".into(),
                    description: Some(
                        "- Always call this at the start of the conversation before using any of the shell tools from wcgw. \
                         - Use `any_workspace_path` to initialize the shell in the appropriate project directory. \
                         - If the user has mentioned a workspace or project root or any other file or folder use it to set `any_workspace_path`. \
                         - If user has mentioned any files use `initial_files_to_read` to read, use absolute paths only (~ allowed) \
                         - By default use mode \"wcgw\" \
                         - In \"code-writer\" mode, set the commands and globs which user asked to set, otherwise use 'all'. \
                         - Use type=\"first_call\" if it's the first call to this tool. \
                         - Use type=\"user_asked_mode_change\" if in a conversation user has asked to change mode. \
                         - Use type=\"reset_shell\" if in a conversation shell is not working after multiple tries. \
                         - Use type=\"user_asked_change_workspace\" if in a conversation user asked to change workspace"
                            .into(),
                    ),
                    input_schema: schema_to_input_schema::<Initialize>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().read_only(true).open_world(false)),
                    title: None,
                    icons: None,
                    meta: None,
                },
                Tool {
                    name: "BashCommand".into(),
                    description: Some(
                        "- Execute a bash command. This is stateful (beware with subsequent calls). \
                         - Status of the command and the current working directory will always be returned at the end. \
                         - The first or the last line might be `(...truncated)` if the output is too long. \
                         - Always run `pwd` if you get any file or directory not found error to make sure you're not lost. \
                         - Do not run bg commands using \"&\", instead use this tool. \
                         - You must not use echo/cat to read/write files, use ReadFiles/FileWriteOrEdit \
                         - In order to check status of previous command, use `status_check` with empty command argument. \
                         - Only command is allowed to run at a time. You need to wait for any previous command to finish before running a new one. \
                         - Programs don't hang easily, so most likely explanation for no output is usually that the program is still running, and you need to check status again. \
                         - Do not send Ctrl-c before checking for status till 10 minutes or whatever is appropriate for the program to finish. \
                         - Only run long running commands in background. Each background command is run in a new non-reusable shell. \
                         - On running a bg command you'll get a bg command id that you should use to get status or interact."
                            .into(),
                    ),
                    input_schema: schema_to_input_schema::<BashCommand>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().destructive(true).open_world(true)),
                    title: None,
                    icons: None,
                    meta: None,
                },
                Tool {
                    name: "ReadFiles".into(),
                    description: Some(
                        "- Read full file content of one or more files. \
                         - Provide absolute paths only (~ allowed) \
                         - Only if the task requires line numbers understanding: \
                         - You may extract a range of lines. E.g., `/path/to/file:1-10` for lines 1-10. You can drop start or end like `/path/to/file:1-` or `/path/to/file:-10`"
                            .into(),
                    ),
                    input_schema: schema_to_input_schema::<ReadFiles>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().read_only(true).open_world(false)),
                    title: None,
                    icons: None,
                    meta: None,
                },
                Tool {
                    name: "FileWriteOrEdit".into(),
                    description: Some(
                        "- Writes or edits a file based on the percentage of changes. \
                         - Use absolute path only (~ allowed). \
                         - First write down percentage of lines that need to be replaced in the file (between 0-100) in percentage_to_change \
                         - percentage_to_change should be low if mostly new code is to be added. It should be high if a lot of things are to be replaced. \
                         - If percentage_to_change > 50, provide full file content in text_or_search_replace_blocks \
                         - If percentage_to_change <= 50, text_or_search_replace_blocks should be search/replace blocks. \
                         \
                         Instructions for editing files. \
                         # Example \
                         ## Input file \
                         ``` \
                         import numpy as np \
                         from impls import impl1, impl2 \
                         \
                         def hello(): \
                             \"print a greeting\" \
                             print(\"hello\") \
                         \
                         def call_hello(): \
                             \"call hello\" \
                             hello() \
                             print(\"Called\") \
                             impl1() \
                             hello() \
                             impl2() \
                         ``` \
                         ## Edit format on the input file \
                         ``` \
                         <<<<<<< SEARCH \
                         from impls import impl1, impl2 \
                         ======= \
                         from impls import impl1, impl2 \
                         from hello import hello as hello_renamed \
                         >>>>>>> REPLACE \
                         <<<<<<< SEARCH \
                         def hello(): \
                             \"print a greeting\" \
                             print(\"hello\") \
                         ======= \
                         >>>>>>> REPLACE \
                         <<<<<<< SEARCH \
                         def call_hello(): \
                             \"call hello\" \
                             hello() \
                         ======= \
                         def call_hello_renamed(): \
                             \"call hello renamed\" \
                             hello_renamed() \
                         >>>>>>> REPLACE \
                         <<<<<<< SEARCH \
                         impl1() \
                         hello() \
                         impl2() \
                         ======= \
                         impl1() \
                         hello_renamed() \
                         impl2() \
                         >>>>>>> REPLACE \
                         ``` \
                         # *SEARCH/REPLACE block* Rules: \
                         Every \"<<<<<<< SEARCH\" section must *EXACTLY MATCH* the existing file content, character for character, including all comments, docstrings, whitespaces, etc. \
                         Including multiple unique *SEARCH/REPLACE* blocks if needed. \
                         Include enough and only enough lines in each SEARCH section to uniquely match each set of lines that need to change. \
                         Keep *SEARCH/REPLACE* blocks concise. \
                         Break large *SEARCH/REPLACE* blocks into a series of smaller blocks that each change a small portion of the file. \
                         Include just the changing lines, and a few surrounding lines (0-3 lines) if needed for uniqueness. \
                         Other than for uniqueness, avoid including those lines which do not change in search (and replace) blocks. Target 0-3 non trivial extra lines per block. \
                         Preserve leading spaces and indentations in both SEARCH and REPLACE blocks."
                            .into(),
                    ),
                    input_schema: schema_to_input_schema::<FileWriteOrEdit>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().destructive(true).open_world(false)),
                    title: None,
                    icons: None,
                    meta: None,
                },
                Tool {
                    name: "ContextSave".into(),
                    description: Some(
                        "Saves provided description and file contents of all the relevant file paths or globs in a single text file. \
                         - Provide random 3 word unqiue id or whatever user provided. \
                         - Leave project path as empty string if no project path"
                            .into(),
                    ),
                    input_schema: schema_to_input_schema::<ContextSave>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().destructive(false).open_world(false)),
                    title: None,
                    icons: None,
                    meta: None,
                },
                Tool {
                    name: "ReadImage".into(),
                    description: Some("Read an image from the shell.".into()),
                    input_schema: schema_to_input_schema::<ReadImage>(),
                    output_schema: None,
                    annotations: Some(ToolAnnotations::new().read_only(true).open_world(false)),
                    title: None,
                    icons: None,
                    meta: None,
                },

            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resources(
        &self,
        _param: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![Annotated {
                raw: RawResource {
                    uri: "file://readme".into(),
                    name: "README".into(),
                    description: Some("Project README documentation".into()),
                    mime_type: Some("text/markdown".into()),
                    size: None,
                    title: None,
                    icons: None,
                    meta: None,
                },
                annotations: None,
            }],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        param: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let content = match param.uri.as_ref() {
            "file://readme" => match tokio::fs::read_to_string("README.md").await {
                Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                Err(_) => vec![ResourceContents::text(
                    "README.md not found".to_string(),
                    param.uri.clone(),
                )],
            },
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown resource URI: {}", param.uri),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult { contents: content })
    }

    async fn call_tool(
        &self,
        param: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args_value = param.arguments.map(Value::Object);
        match param.name.as_ref() {
            "Initialize" => self.handle_initialize(args_value).await,
            "BashCommand" => self.handle_bash_command(args_value).await,
            "ReadFiles" => self.handle_read_files(args_value).await,
            "FileWriteOrEdit" => self.handle_file_write_or_edit(args_value).await,
            "ContextSave" => self.handle_context_save(args_value).await,
            "ReadImage" => self.handle_read_image(args_value).await,
            _ => Err(McpError::invalid_request(format!("Unknown tool: {}", param.name), None)),
        }
    }
}

impl WinxService {
    async fn handle_initialize(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let initialize: Initialize = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid Initialize parameters: {e}"), None)
        })?;

        match crate::tools::initialize::handle_tool_call(&self.bash_state, initialize).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(format!("Initialize failed: {e}"), None)),
        }
    }

    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let bash_command: BashCommand = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid BashCommand parameters: {e}"), None)
        })?;

        match crate::tools::bash_command::handle_tool_call(&self.bash_state, bash_command).await {
            Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
            Err(e) => Err(McpError::internal_error(format!("BashCommand failed: {e}"), None)),
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_files: ReadFiles = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {e}"), None)
        })?;

        match crate::tools::read_files::handle_tool_call(&self.bash_state, read_files).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(format!("ReadFiles failed: {e}"), None)),
        }
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let file_write_or_edit: FileWriteOrEdit = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid FileWriteOrEdit parameters: {e}"), None)
        })?;

        match crate::tools::file_write_or_edit::handle_tool_call(
            &self.bash_state,
            file_write_or_edit,
        )
        .await
        {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(format!("FileWriteOrEdit failed: {e}"), None)),
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let context_save: ContextSave = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ContextSave parameters: {e}"), None)
        })?;

        match crate::tools::context_save::handle_tool_call(&self.bash_state, context_save).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(format!("ContextSave failed: {e}"), None)),
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_image: ReadImage = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadImage parameters: {e}"), None)
        })?;

        match crate::tools::read_image::handle_tool_call(&self.bash_state, read_image).await {
            Ok((mime_type, base64_data)) => {
                let result_text = format!("MIME: {mime_type}\nData: {base64_data}");
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
            }
            Err(e) => Err(McpError::internal_error(format!("ReadImage failed: {e}"), None)),
        }
    }


}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server");
    let service = WinxService::new();
    let server = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
