//! Winx MCP Server implementation using rmcp 0.5.0
//! Following the tool_router pattern from official examples

use rmcp::{
    model::*,
    transport::stdio,
    ServiceExt,
    ErrorData as McpError,
};
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::state::bash_state::BashState;

/// Winx service with shared bash state and tool implementations
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    pub bash_state: Arc<Mutex<Option<BashState>>>,
    /// Version information for the service
    pub version: String,
}

impl WinxService {
    /// Create a new WinxService instance
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Initialize the shell environment
    async fn initialize(
        &self,
        folder_to_start: String,
        mode: Option<String>,
        over_screen: Option<bool>,
    ) -> Result<CallToolResult, McpError> {
        let result = format!(
            "Environment initialized in: {}\nMode: {}\nOver screen: {}",
            folder_to_start,
            mode.as_deref().unwrap_or("wcgw"),
            over_screen.unwrap_or(false)
        );
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Execute a bash command
    async fn bash_command(
        &self,
        command: String,
        send_text: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let output = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&command)
            .output()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

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

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Read files with optional line numbers
    async fn read_files(
        &self,
        paths: Vec<String>,
        include_line_numbers: Option<bool>,
    ) -> Result<CallToolResult, McpError> {
        if paths.is_empty() {
            return Err(McpError::invalid_params("No file paths provided", None));
        }

        let include_line_numbers = include_line_numbers.unwrap_or(false);
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

        Ok(CallToolResult::success(vec![Content::text(results.join("\n\n"))]))
    }

    /// Write content to a file
    async fn write_file(
        &self,
        path: String,
        content: String,
        is_executable: Option<bool>,
    ) -> Result<CallToolResult, McpError> {
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
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }

        tokio::fs::write(&expanded_path, &content)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Set executable if requested
        if is_executable.unwrap_or(false) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = tokio::fs::metadata(&expanded_path)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .permissions();
                perms.set_mode(perms.mode() | 0o755);
                tokio::fs::set_permissions(&expanded_path, perms)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            }
        }

        let result = format!("File written successfully: {}", path);
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

// Manual implementation of the MCP server without macros for now
// We'll implement the ServerHandler trait directly

use rmcp::ServerHandler;

impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            name: "winx-code-agent".to_string(),
            version: self.version.clone(),
        }
    }

    fn get_capabilities(&self) -> ServerCapabilities {
        ServerCapabilities::builder()
            .with_tools()
            .build()
    }

    async fn list_tools(&self, _params: ListToolsRequestParams) -> Result<ListToolsResult, McpError> {
        let tools = vec![
            Tool {
                name: "initialize".to_string(),
                description: "Initialize the shell environment with workspace path and configuration".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "folder_to_start": {"type": "string"},
                        "mode": {"type": "string", "enum": ["wcgw", "architect", "code_writer"]},
                        "over_screen": {"type": "boolean"}
                    },
                    "required": ["folder_to_start"]
                }),
            },
            Tool {
                name: "bash_command".to_string(),
                description: "Execute a bash command with stateful session management".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "send_text": {"type": "string"}
                    },
                    "required": ["command"]
                }),
            },
            Tool {
                name: "read_files".to_string(),
                description: "Read full file content of one or more files with optional line ranges".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "paths": {"type": "array", "items": {"type": "string"}},
                        "include_line_numbers": {"type": "boolean"}
                    },
                    "required": ["paths"]
                }),
            },
            Tool {
                name: "write_file".to_string(),
                description: "Write content to a file, creating directories if needed".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"},
                        "is_executable": {"type": "boolean"}
                    },
                    "required": ["path", "content"]
                }),
            },
        ];

        Ok(ListToolsResult { tools })
    }

    async fn call_tool(&self, params: CallToolRequestParams) -> Result<CallToolResult, McpError> {
        info!("Handling tool request: {}", params.name);
        
        match params.name.as_str() {
            "initialize" => {
                let folder_to_start = params.arguments.get("folder_to_start")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mode = params.arguments.get("mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let over_screen = params.arguments.get("over_screen")
                    .and_then(|v| v.as_bool());
                
                self.initialize(folder_to_start, mode, over_screen).await
            }
            "bash_command" => {
                let command = params.arguments.get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| McpError::invalid_params("Missing command parameter", None))?
                    .to_string();
                let send_text = params.arguments.get("send_text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                
                self.bash_command(command, send_text).await
            }
            "read_files" => {
                let paths = params.arguments.get("paths")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| McpError::invalid_params("Missing paths parameter", None))?
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                let include_line_numbers = params.arguments.get("include_line_numbers")
                    .and_then(|v| v.as_bool());
                
                self.read_files(paths, include_line_numbers).await
            }
            "write_file" => {
                let path = params.arguments.get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| McpError::invalid_params("Missing path parameter", None))?
                    .to_string();
                let content = params.arguments.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| McpError::invalid_params("Missing content parameter", None))?
                    .to_string();
                let is_executable = params.arguments.get("is_executable")
                    .and_then(|v| v.as_bool());
                
                self.write_file(path, content, is_executable).await
            }
            _ => Err(McpError::method_not_found(format!("Unknown tool: {}", params.name), None)),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create the service and start it with stdio transport
    let service = WinxService::new().serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}