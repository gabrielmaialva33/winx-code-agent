//! Winx MCP Server implementation using rmcp 0.12.0
//! Core WCGW tools only - no AI integration

use rmcp::{
    model::{
        Annotated, CallToolRequestParam, CallToolResult, Content, GetPromptRequestParam,
        GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, RawResource, ReadResourceRequestParam,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::schema_for;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex; // Changed from std::sync::Mutex for async safety
use tracing::{info, warn};

use crate::state::BashState;
use crate::types::{BashCommand, ContextSave, FileWriteOrEdit, Initialize, ReadFiles, ReadImage};

/// Type alias for the shared bash state - uses tokio::sync::Mutex for async safety
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

/// Winx service with shared bash state (core WCGW tools only)
///
/// Uses `tokio::sync::Mutex` for thread-safe async access to the shell state.
/// This prevents race conditions when multiple requests access the shell concurrently.
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

    /// Get project structure overview
    async fn get_project_structure(&self) -> Result<String, McpError> {
        let mut structure = String::new();
        structure.push_str("# Winx Code Agent - Project Structure\n\n");
        structure.push_str("## Root Files\n");
        structure.push_str("- Cargo.toml - Project configuration and dependencies\n");
        structure.push_str("- README.md - Project documentation\n");
        structure.push_str("- CLAUDE.md - Claude integration guide\n\n");
        structure.push_str("## Source Code Structure\n");
        structure.push_str("- src/main.rs - Application entry point\n");
        structure.push_str("- src/server.rs - MCP server implementation\n");
        structure.push_str("- src/tools/ - MCP tools implementation\n");
        structure.push_str("- src/state/ - Shell and terminal state management\n");
        structure.push_str("- src/utils/ - Utility functions\n\n");
        structure.push_str("## Key Features\n");
        structure.push_str("- Shell command execution with state management\n");
        structure.push_str("- File operations and context saving\n");
        structure.push_str("- Image reading capabilities\n");
        Ok(structure)
    }

    /// Get source code structure details
    async fn get_src_structure(&self) -> Result<String, McpError> {
        let mut structure = String::new();
        structure.push_str("# Source Code Organization\n\n");

        // Try to read actual directory structure
        if let Ok(mut read_dir) = tokio::fs::read_dir("src").await {
            structure.push_str("## src/ Directory Contents\n");
            let mut entries = Vec::new();
            while let Some(entry) = read_dir.next_entry().await.unwrap_or(None) {
                entries.push(entry);
            }
            entries.sort_by_key(tokio::fs::DirEntry::file_name);

            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
                if is_dir {
                    structure.push_str(&format!("- {name}/\n"));
                } else {
                    structure.push_str(&format!("- {name}\n"));
                }
            }
        } else {
            structure.push_str("## Core Modules\n");
            structure.push_str("- main.rs - Application entry point\n");
            structure.push_str("- server.rs - MCP server implementation\n");
            structure.push_str("- tools/ - MCP tools\n");
            structure.push_str("- state/ - State management\n");
            structure.push_str("- utils/ - Utilities\n");
        }

        Ok(structure)
    }
}

/// `ServerHandler` implementation with manual tool handling
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-code-agent".into(),
                version: self.version.clone(),
                title: Some("Winx Code Agent".into()),
                icons: None,
                website_url: None,
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents. \
                Provides shell execution, file management, and context saving capabilities."
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
                        "- Always call this at the start of the conversation before using any of the shell tools from wcgw.\n\
                        - Use `any_workspace_path` to initialize the shell in the appropriate project directory.\n\
                        - If the user has mentioned a workspace or project root or any other file or folder use it to set `any_workspace_path`.\n\
                        - If user has mentioned any files use `initial_files_to_read` to read, use absolute paths only (~ allowed)\n\
                        - By default use mode \"wcgw\"\n\
                        - In \"code-writer\" mode, set the commands and globs which user asked to set, otherwise use 'all'.\n\
                        - Use type=\"first_call\" if it's the first call to this tool.\n\
                        - Use type=\"user_asked_mode_change\" if in a conversation user has asked to change mode.\n\
                        - Use type=\"reset_shell\" if in a conversation shell is not working after multiple tries.\n\
                        - Use type=\"user_asked_change_workspace\" if in a conversation user asked to change workspace".into()
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
                        "- Execute a bash command. This is stateful (beware with subsequent calls).\n\
                        - Status of the command and the current working directory will always be returned at the end.\n\
                        - The first or the last line might be `(...truncated)` if the output is too long.\n\
                        - Always run `pwd` if you get any file or directory not found error to make sure you're not lost.\n\
                        - Do not run bg commands using \"&\", instead use this tool.\n\
                        - You must not use echo/cat to read/write files, use ReadFiles/FileWriteOrEdit\n\
                        - In order to check status of previous command, use `status_check` with empty command argument.\n\
                        - Only command is allowed to run at a time. You need to wait for any previous command to finish before running a new one.\n\
                        - Programs don't hang easily, so most likely explanation for no output is usually that the program is still running, and you need to check status again.\n\
                        - Do not send Ctrl-c before checking for status till 10 minutes or whatever is appropriate for the program to finish.\n\
                        - Only run long running commands in background. Each background command is run in a new non-reusable shell.\n\
                        - On running a bg command you'll get a bg command id that you should use to get status or interact.".into()
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
                        "- Read full file content of one or more files.\n\
                        - Provide absolute paths only (~ allowed)\n\
                        - Only if the task requires line numbers understanding:\n\
                            - You may extract a range of lines. E.g., `/path/to/file:1-10` for lines 1-10. You can drop start or end like `/path/to/file:1-` or `/path/to/file:-10`".into()
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
                        "- Writes or edits a file based on the percentage of changes.\n\
                        - Use absolute path only (~ allowed).\n\
                        - First write down percentage of lines that need to be replaced in the file (between 0-100) in percentage_to_change\n\
                        - percentage_to_change should be low if mostly new code is to be added. It should be high if a lot of things are to be replaced.\n\
                        - If percentage_to_change > 50, provide full file content in text_or_search_replace_blocks\n\
                        - If percentage_to_change <= 50, text_or_search_replace_blocks should be search/replace blocks.\n\
                        - Search/replace block format:\n\
                        <<<<<<< SEARCH\n\
                        old content to find\n\
                        =======\n\
                        new content to replace with\n\
                        >>>>>>> REPLACE".into()
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
                        "Saves provided description and file contents of all the relevant file paths or globs in a single text file.\n\
                        - Provide random 3 word unique id or whatever user provided.\n\
                        - Leave project path as empty string if no project path".into()
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
            resources: vec![
                Annotated {
                    raw: RawResource {
                        uri: "file://project-structure".into(),
                        name: "Project Structure".into(),
                        description: Some("Overview of the project structure and files".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                        title: None,
                        icons: None,
                        meta: None,
                    },
                    annotations: None,
                },
                Annotated {
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
                },
                Annotated {
                    raw: RawResource {
                        uri: "file://cargo-toml".into(),
                        name: "Cargo.toml".into(),
                        description: Some("Project configuration and dependencies".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                        title: None,
                        icons: None,
                        meta: None,
                    },
                    annotations: None,
                },
                Annotated {
                    raw: RawResource {
                        uri: "file://src-structure".into(),
                        name: "Source Code Structure".into(),
                        description: Some("Overview of the source code organization".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                        title: None,
                        icons: None,
                        meta: None,
                    },
                    annotations: None,
                },
            ],
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
            "file://project-structure" => {
                let structure = self.get_project_structure().await?;
                vec![ResourceContents::text(structure, param.uri.clone())]
            }
            "file://readme" => match tokio::fs::read_to_string("README.md").await {
                Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                Err(_) => vec![ResourceContents::text(
                    "README.md not found".to_string(),
                    param.uri.clone(),
                )],
            },
            "file://cargo-toml" => match tokio::fs::read_to_string("Cargo.toml").await {
                Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                Err(_) => vec![ResourceContents::text(
                    "Cargo.toml not found".to_string(),
                    param.uri.clone(),
                )],
            },
            "file://src-structure" => {
                let structure = self.get_src_structure().await?;
                vec![ResourceContents::text(structure, param.uri.clone())]
            }
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown resource URI: {}", param.uri),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult { contents: content })
    }

    async fn list_prompts(
        &self,
        _param: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult { prompts: vec![], next_cursor: None, meta: None })
    }

    async fn get_prompt(
        &self,
        param: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        Err(McpError::invalid_request(format!("Unknown prompt: {}", param.name), None))
    }

    async fn call_tool(
        &self,
        param: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args_value = param.arguments.map(Value::Object);
        let result = match param.name.as_ref() {
            "Initialize" => self.handle_initialize(args_value.clone()).await?,
            "BashCommand" => self.handle_bash_command(args_value.clone()).await?,
            "ReadFiles" => self.handle_read_files(args_value.clone()).await?,
            "FileWriteOrEdit" => self.handle_file_write_or_edit(args_value.clone()).await?,
            "ContextSave" => self.handle_context_save(args_value.clone()).await?,
            "ReadImage" => self.handle_read_image(args_value.clone()).await?,
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown tool: {}", param.name),
                    None,
                ))
            }
        };

        Ok(result)
    }
}

impl WinxService {
    async fn handle_initialize(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        // Parse Initialize from args - using wcgw field names
        let initialize: crate::types::Initialize =
            serde_json::from_value(args.clone()).map_err(|e| {
                McpError::invalid_request(format!("Invalid Initialize parameters: {e}"), None)
            })?;

        // Call the real implementation
        match crate::tools::initialize::handle_tool_call(&self.bash_state, initialize).await {
            Ok(result) => {
                info!("Initialize succeeded");
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("Initialize failed: {}", e);
                Err(McpError::internal_error(format!("Initialize failed: {e}"), None))
            }
        }
    }

    /// Handle bash command execution using the correct WCGW-compatible schema.
    ///
    /// Uses the BashCommand struct with action_json field for full WCGW parity.
    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        // Parse BashCommand from args - using WCGW-compatible schema with action_json
        let bash_command: crate::types::BashCommand = serde_json::from_value(args.clone())
            .map_err(|e| {
                McpError::invalid_request(format!("Invalid BashCommand parameters: {e}"), None)
            })?;

        // Call the real WCGW-compatible implementation
        match crate::tools::bash_command::handle_tool_call(&self.bash_state, bash_command).await {
            Ok(output) => {
                info!("BashCommand succeeded");
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => {
                warn!("BashCommand failed: {}", e);
                Err(McpError::internal_error(format!("BashCommand failed: {e}"), None))
            }
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        // Parse ReadFiles from args - uses custom deserializer that handles line ranges
        let read_files: crate::types::ReadFiles =
            serde_json::from_value(args.clone()).map_err(|e| {
                McpError::invalid_request(format!("Invalid ReadFiles parameters: {e}"), None)
            })?;

        // Call the real implementation with full functionality:
        // - Line range support (:1-100)
        // - Path expansion (~)
        // - Whitelist tracking
        // - Optimized mmap reading
        // - Token economy
        match crate::tools::read_files::handle_tool_call(&self.bash_state, read_files).await {
            Ok(result) => {
                info!("ReadFiles succeeded");
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("ReadFiles failed: {}", e);
                Err(McpError::internal_error(format!("ReadFiles failed: {e}"), None))
            }
        }
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        // Parse FileWriteOrEdit from args - using correct wcgw field names
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing file_path", None))?
            .to_string();

        let percentage_to_change =
            args.get("percentage_to_change").and_then(|v| v.as_u64()).unwrap_or(100) as u32;

        let text_or_search_replace_blocks = args
            .get("text_or_search_replace_blocks")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::invalid_request("Missing text_or_search_replace_blocks", None)
            })?
            .to_string();

        let thread_id = args.get("thread_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Create FileWriteOrEdit struct
        let file_write_or_edit = crate::types::FileWriteOrEdit {
            file_path: file_path.clone(),
            percentage_to_change,
            text_or_search_replace_blocks,
            thread_id,
        };

        // Call the real implementation
        match crate::tools::file_write_or_edit::handle_tool_call(
            &self.bash_state,
            file_write_or_edit,
        )
        .await
        {
            Ok(result) => {
                info!("FileWriteOrEdit succeeded: {}", file_path);
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("FileWriteOrEdit failed for {}: {}", file_path, e);
                Err(McpError::internal_error(format!("FileWriteOrEdit failed: {e}"), None))
            }
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing id", None))?;
        let project_root_path = args
            .get("project_root_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing project_root_path", None))?;
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing description", None))?;
        let relevant_file_globs = args
            .get("relevant_file_globs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::invalid_request("Missing relevant_file_globs array", None))?;

        let globs: Result<Vec<String>, McpError> = relevant_file_globs
            .iter()
            .map(|v| {
                v.as_str()
                    .map(std::string::ToString::to_string)
                    .ok_or_else(|| McpError::invalid_request("Invalid glob in array", None))
            })
            .collect();
        let globs = globs?;

        let context_save = ContextSave {
            id: id.to_string(),
            project_root_path: project_root_path.to_string(),
            description: description.to_string(),
            relevant_file_globs: globs,
        };

        match crate::tools::context_save::handle_tool_call(&self.bash_state, context_save).await {
            Ok(result) => {
                info!("Context saved successfully");
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("Failed to save context: {}", e);
                Err(McpError::internal_error(format!("Failed to save context: {e}"), None))
            }
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing file_path", None))?;

        let read_image = ReadImage { file_path: file_path.to_string() };

        match crate::tools::read_image::handle_tool_call(&self.bash_state, read_image).await {
            Ok((mime_type, base64_data)) => {
                info!("Image read successfully: {}", file_path);
                let result_text = format!(
                    "Image: {file_path}\nMIME Type: {mime_type}\nBase64 Data: {base64_data}"
                );
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
            }
            Err(e) => {
                warn!("Failed to read image {}: {}", file_path, e);
                Err(McpError::internal_error(format!("Failed to read image: {e}"), None))
            }
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.12.0 (core tools only)");

    // Create service
    let service = WinxService::new();

    // Create and run the server with STDIO transport
    let server = service.serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {e}");
    })?;
    server.waiting().await?;

    Ok(())
}
