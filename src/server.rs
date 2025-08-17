//! Winx MCP Server implementation using rust-mcp-sdk
//! Simplified version based on official examples

use async_trait::async_trait;
use rust_mcp_sdk::{
    mcp_server::{server_runtime, ServerHandler, ServerRuntime},
    McpServer, StdioTransport, TransportOptions,
    error::SdkResult,
    macros::{mcp_tool, JsonSchema},
    tool_box,
};
use rust_mcp_schema::{
    schema_utils::CallToolError,
    CallToolRequest, CallToolResult, RpcError, TextContent,
    ListToolsRequest, ListToolsResult,
    Implementation, InitializeResult, ServerCapabilities, 
    ServerCapabilitiesTools, LATEST_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

//****************//
//  InitializeTool  //
//****************//
#[mcp_tool(
    name = "initialize",
    description = "Initialize the shell environment with workspace path and configuration"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct InitializeTool {
    /// The workspace folder to start in
    #[serde(default)]
    pub folder_to_start: String,
    /// Operating mode (wcgw, architect, code_writer)
    #[serde(default)]
    pub mode: Option<String>,
    /// Whether to use screen session
    #[serde(default)]
    pub over_screen: Option<bool>,
}

impl InitializeTool {
    pub fn call_tool(&self) -> Result<CallToolResult, CallToolError> {
        let result = format!(
            "Environment initialized in: {}\nMode: {}\nOver screen: {}",
            self.folder_to_start,
            self.mode.as_deref().unwrap_or("wcgw"),
            self.over_screen.unwrap_or(false)
        );
        Ok(CallToolResult::text_content(vec![TextContent::from(result)]))
    }
}

//******************//
//  BashCommandTool  //
//******************//
#[mcp_tool(
    name = "bash_command",
    description = "Execute a bash command with stateful session management"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BashCommandTool {
    /// The bash command to execute
    pub command: String,
    /// Optional text to send to the command's stdin
    #[serde(default)]
    pub send_text: Option<String>,
}

impl BashCommandTool {
    pub fn call_tool(&self) -> Result<CallToolResult, CallToolError> {
        let rt = tokio::runtime::Handle::current();
        let command = self.command.clone();
        
        let result = rt.block_on(async move {
            let output = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&command)
                .output()
                .await
                .map_err(|e| CallToolError::new(e))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);

            let mut result = String::new();
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("STDERR:\n");
                result.push_str(&stderr);
            }
            result.push_str(&format!("\n[Exit code: {}]", exit_code));

            Ok::<String, CallToolError>(result)
        })?;

        Ok(CallToolResult::text_content(vec![TextContent::from(result)]))
    }
}

//******************//
//  ReadFilesTool  //
//******************//
#[mcp_tool(
    name = "read_files",
    description = "Read full file content of one or more files with optional line ranges"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadFilesTool {
    /// Array of file paths to read
    pub paths: Vec<String>,
    /// Whether to include line numbers
    #[serde(default)]
    pub include_line_numbers: Option<bool>,
}

impl ReadFilesTool {
    pub fn call_tool(&self) -> Result<CallToolResult, CallToolError> {
        if self.paths.is_empty() {
            return Err(CallToolError::new("No file paths provided"));
        }

        let rt = tokio::runtime::Handle::current();
        let paths = self.paths.clone();
        let include_line_numbers = self.include_line_numbers.unwrap_or(false);
        
        let result = rt.block_on(async move {
            let mut results = Vec::new();
            
            for path in &paths {
                let expanded_path = if path.starts_with('~') {
                    home::home_dir()
                        .map(|home| home.join(&path[2..]))
                        .unwrap_or_else(|| std::path::PathBuf::from(path))
                } else {
                    std::path::PathBuf::from(path)
                };

                match tokio::fs::read_to_string(&expanded_path).await {
                    Ok(content) => {
                        let final_content = if include_line_numbers {
                            content
                                .lines()
                                .enumerate()
                                .map(|(i, line)| format!("{:4} | {}", i + 1, line))
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            content
                        };
                        
                        results.push(format!("=== {} ===\n{}", path, final_content));
                    }
                    Err(e) => {
                        results.push(format!("=== {} ===\nERROR: {}", path, e));
                    }
                }
            }

            Ok::<String, CallToolError>(results.join("\n\n"))
        })?;

        Ok(CallToolResult::text_content(vec![TextContent::from(result)]))
    }
}

//******************//
//  WriteFileTool  //
//******************//
#[mcp_tool(
    name = "write_file", 
    description = "Write content to a file, creating directories if needed"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WriteFileTool {
    /// Path to the file to write
    pub path: String,
    /// Content to write to the file
    pub content: String,
    /// Whether to make the file executable
    #[serde(default)]
    pub is_executable: Option<bool>,
}

impl WriteFileTool {
    pub fn call_tool(&self) -> Result<CallToolResult, CallToolError> {
        let rt = tokio::runtime::Handle::current();
        let path = self.path.clone();
        let content = self.content.clone();
        let is_executable = self.is_executable.unwrap_or(false);
        
        let result = rt.block_on(async move {
            let expanded_path = if path.starts_with('~') {
                home::home_dir()
                    .map(|home| home.join(&path[2..]))
                    .unwrap_or_else(|| std::path::PathBuf::from(&path))
            } else {
                std::path::PathBuf::from(&path)
            };

            // Create parent directories if they don't exist
            if let Some(parent) = expanded_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| CallToolError::new(e))?;
            }

            tokio::fs::write(&expanded_path, &content)
                .await
                .map_err(|e| CallToolError::new(e))?;

            // Set executable if requested
            if is_executable {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = tokio::fs::metadata(&expanded_path)
                        .await
                        .map_err(|e| CallToolError::new(e))?
                        .permissions();
                    perms.set_mode(perms.mode() | 0o755);
                    tokio::fs::set_permissions(&expanded_path, perms)
                        .await
                        .map_err(|e| CallToolError::new(e))?;
                }
            }

            Ok::<String, CallToolError>(format!("File written successfully: {}", path))
        })?;

        Ok(CallToolResult::text_content(vec![TextContent::from(result)]))
    }
}

//******************//
//  WinxTools  //
//******************//
// Generates an enum named WinxTools with all tool variants
tool_box!(WinxTools, [InitializeTool, BashCommandTool, ReadFilesTool, WriteFileTool]);

//******************//
//  WinxServerHandler  //
//******************//
pub struct WinxServerHandler;

#[async_trait]
impl ServerHandler for WinxServerHandler {
    // Handle ListToolsRequest, return list of available tools as ListToolsResult
    async fn handle_list_tools_request(
        &self,
        _request: ListToolsRequest,
        _runtime: &dyn McpServer,
    ) -> std::result::Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: WinxTools::tools(),
        })
    }

    // Handles incoming CallToolRequest and processes it using the appropriate tool
    async fn handle_call_tool_request(
        &self,
        request: CallToolRequest,
        _runtime: &dyn McpServer,
    ) -> std::result::Result<CallToolResult, CallToolError> {
        info!("Handling tool request: {}", request.method);
        
        // Attempt to convert request parameters into WinxTools enum
        let tool_params: WinxTools =
            WinxTools::try_from(request.params).map_err(CallToolError::new)?;

        // Match the tool variant and execute its corresponding logic
        match tool_params {
            WinxTools::InitializeTool(tool) => tool.call_tool(),
            WinxTools::BashCommandTool(tool) => tool.call_tool(),
            WinxTools::ReadFilesTool(tool) => tool.call_tool(),
            WinxTools::WriteFileTool(tool) => tool.call_tool(),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> SdkResult<()> {
    info!("Starting Winx MCP Server using rust-mcp-sdk");

    // Define server capabilities and info
    let server_details = InitializeResult {
        server_info: Implementation {
            name: "winx-code-agent".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            title: Some("Winx Code Agent - Rust WCGW Implementation".to_string()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        meta: None,
        instructions: Some("Winx is a high-performance Rust implementation of WCGW for code agents. It provides shell execution and file management capabilities.".to_string()),
        protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
    };

    // Create stdio transport
    let transport = StdioTransport::new(TransportOptions::default())?;

    // Create handler
    let handler = WinxServerHandler;

    // Create and start server
    let server: ServerRuntime = server_runtime::create_server(server_details, transport, handler);
    server.start().await
}