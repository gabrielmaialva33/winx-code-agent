//! Winx MCP Server implementation using rmcp 0.5.0
//! Original MCP SDK approach

use async_trait::async_trait;
use rmcp::{
    McpServer,
    transport::stdio::StdioTransport,
    types::*,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

//****************//
//  InitializeTool  //
//****************//
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
    pub fn call_tool(&self) -> Result<CallToolResult, String> {
        let result = format!(
            "Environment initialized in: {}\nMode: {}\nOver screen: {}",
            self.folder_to_start,
            self.mode.as_deref().unwrap_or("wcgw"),
            self.over_screen.unwrap_or(false)
        );
        Ok(CallToolResult {
            content: vec![CallToolResultContent::Text { text: result }],
            is_error: None,
        })
    }
}

//******************//
//  BashCommandTool  //
//******************//
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BashCommandTool {
    /// The bash command to execute
    pub command: String,
    /// Optional text to send to the command's stdin
    #[serde(default)]
    pub send_text: Option<String>,
}

impl BashCommandTool {
    pub fn call_tool(&self) -> Result<CallToolResult, String> {
        let rt = tokio::runtime::Handle::current();
        let command = self.command.clone();
        
        let result = rt.block_on(async move {
            let output = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&command)
                .output()
                .await
                .map_err(|e| e.to_string())?;

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

            Ok::<String, String>(result)
        })?;

        Ok(CallToolResult {
            content: vec![CallToolResultContent::Text { text: result }],
            is_error: None,
        })
    }
}

//******************//
//  ReadFilesTool  //
//******************//
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadFilesTool {
    /// Array of file paths to read
    pub paths: Vec<String>,
    /// Whether to include line numbers
    #[serde(default)]
    pub include_line_numbers: Option<bool>,
}

impl ReadFilesTool {
    pub fn call_tool(&self) -> Result<CallToolResult, String> {
        if self.paths.is_empty() {
            return Err("No file paths provided".to_string());
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

            Ok::<String, String>(results.join("\n\n"))
        })?;

        Ok(CallToolResult {
            content: vec![CallToolResultContent::Text { text: result }],
            is_error: None,
        })
    }
}

//******************//
//  WriteFileTool  //
//******************//
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
    pub fn call_tool(&self) -> Result<CallToolResult, String> {
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
                    .map_err(|e| e.to_string())?;
            }

            tokio::fs::write(&expanded_path, &content)
                .await
                .map_err(|e| e.to_string())?;

            // Set executable if requested
            if is_executable {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = tokio::fs::metadata(&expanded_path)
                        .await
                        .map_err(|e| e.to_string())?
                        .permissions();
                    perms.set_mode(perms.mode() | 0o755);
                    tokio::fs::set_permissions(&expanded_path, perms)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }

            Ok::<String, String>(format!("File written successfully: {}", path))
        })?;

        Ok(CallToolResult {
            content: vec![CallToolResultContent::Text { text: result }],
            is_error: None,
        })
    }
}

//******************//
//  WinxService  //
//******************//
pub struct WinxService;

#[async_trait]
impl McpServer for WinxService {
    async fn list_tools(&self, _request: ListToolsRequest) -> Result<ListToolsResult, String> {
        let tools = vec![
            Tool {
                name: "initialize".to_string(),
                description: "Initialize the shell environment with workspace path and configuration".to_string(),
                input_schema: serde_json::to_value(schemars::schema_for!(InitializeTool))
                    .map_err(|e| e.to_string())?,
            },
            Tool {
                name: "bash_command".to_string(),
                description: "Execute a bash command with stateful session management".to_string(),
                input_schema: serde_json::to_value(schemars::schema_for!(BashCommandTool))
                    .map_err(|e| e.to_string())?,
            },
            Tool {
                name: "read_files".to_string(),
                description: "Read full file content of one or more files with optional line ranges".to_string(),
                input_schema: serde_json::to_value(schemars::schema_for!(ReadFilesTool))
                    .map_err(|e| e.to_string())?,
            },
            Tool {
                name: "write_file".to_string(),
                description: "Write content to a file, creating directories if needed".to_string(),
                input_schema: serde_json::to_value(schemars::schema_for!(WriteFileTool))
                    .map_err(|e| e.to_string())?,
            },
        ];

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    async fn call_tool(&self, request: CallToolRequest) -> Result<CallToolResult, String> {
        info!("Handling tool request: {}", request.name);
        
        match request.name.as_str() {
            "initialize" => {
                let tool: InitializeTool = serde_json::from_value(request.arguments)
                    .map_err(|e| format!("Failed to parse initialize arguments: {}", e))?;
                tool.call_tool()
            }
            "bash_command" => {
                let tool: BashCommandTool = serde_json::from_value(request.arguments)
                    .map_err(|e| format!("Failed to parse bash_command arguments: {}", e))?;
                tool.call_tool()
            }
            "read_files" => {
                let tool: ReadFilesTool = serde_json::from_value(request.arguments)
                    .map_err(|e| format!("Failed to parse read_files arguments: {}", e))?;
                tool.call_tool()
            }
            "write_file" => {
                let tool: WriteFileTool = serde_json::from_value(request.arguments)
                    .map_err(|e| format!("Failed to parse write_file arguments: {}", e))?;
                tool.call_tool()
            }
            _ => Err(format!("Unknown tool: {}", request.name)),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create the service
    let service = WinxService;

    // Create stdio transport and start server
    let transport = StdioTransport::new();
    
    // Start the server with the service and transport
    rmcp::run_server(service, transport).await
}