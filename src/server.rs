//! Winx MCP Server implementation using rmcp 0.5.0
//! Enhanced server with NVIDIA AI integration

use rmcp::{
    ErrorData as McpError,
    ServiceExt, 
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
    ServerHandler
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::state::BashState;

/// Helper function to create JSON schema from serde_json::Value
fn json_to_schema(value: Value) -> Arc<serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Winx service with shared bash state and NVIDIA AI integration
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    pub bash_state: Arc<Mutex<Option<BashState>>>,
    /// NVIDIA client for AI-powered features (optional)
    pub nvidia_client: Arc<Mutex<Option<NvidiaClient>>>,
    /// Version information for the service
    pub version: String,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new WinxService instance
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            nvidia_client: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Initialize NVIDIA integration if API key is available
    pub async fn initialize_nvidia(&self) -> crate::Result<bool> {
        match NvidiaConfig::from_env() {
            Ok(config) => match crate::nvidia::initialize(config).await {
                Ok(client) => {
                    *self.nvidia_client.lock().await = Some(client);
                    info!("NVIDIA AI integration initialized successfully");
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to initialize NVIDIA integration: {}", e);
                    Ok(false)
                }
            },
            Err(e) => {
                info!("NVIDIA integration not available: {}", e);
                Ok(false)
            }
        }
    }

    /// Get NVIDIA client if available
    pub async fn get_nvidia_client(&self) -> Option<NvidiaClient> {
        self.nvidia_client.lock().await.clone()
    }
}

/// ServerHandler implementation with manual tool handling
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-code-agent".into(),
                version: self.version.clone(),
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents with NVIDIA AI integration. \
                Provides shell execution, file management, and AI-powered code analysis capabilities.".into(),
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
                    name: "ping".into(),
                    description: Some("Test server connectivity".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "message": {
                                "type": "string",
                                "description": "Optional message to echo back"
                            }
                        }
                    })),
                    output_schema: None,
                },
                Tool {
                    name: "initialize".into(),
                    description: Some("Initialize the bash shell environment".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "shell": {
                                "type": "string",
                                "description": "Shell to use (default: bash)"
                            }
                        }
                    })),
                    output_schema: None,
                },
                Tool {
                    name: "bash_command".into(),
                    description: Some("Execute a command in the bash shell".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "description": "Command to execute"
                            },
                            "timeout_seconds": {
                                "type": "integer",
                                "description": "Timeout in seconds (default: 30)"
                            }
                        },
                        "required": ["command"]
                    })),
                },
                Tool {
                    name: "read_files".into(),
                    description: Some("Read contents of one or more files".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "paths": {
                                "type": "array",
                                "items": {
                                    "type": "string"
                                },
                                "description": "File paths to read"
                            }
                        },
                        "required": ["paths"]
                    })),
                },
                Tool {
                    name: "file_write_or_edit".into(),
                    description: Some("Write or edit file contents".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path to write"
                            },
                            "content": {
                                "type": "string",
                                "description": "Content to write"
                            },
                            "create_if_missing": {
                                "type": "boolean",
                                "description": "Create file if it doesn't exist (default: true)"
                            }
                        },
                        "required": ["path", "content"]
                    })),
                    output_schema: None,
                },
            ],
        })
    }

    async fn call_tool(
        &self,
        param: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let result = match param.name.as_str() {
            "ping" => self.handle_ping(param.arguments).await?,
            "initialize" => self.handle_initialize(param.arguments).await?,
            "bash_command" => self.handle_bash_command(param.arguments).await?,
            "read_files" => self.handle_read_files(param.arguments).await?,
            "file_write_or_edit" => self.handle_file_write_or_edit(param.arguments).await?,
            _ => return Err(McpError::invalid_request(format!("Unknown tool: {}", param.name), None)),
        };

        Ok(result)
    }
}

impl WinxService {
    async fn handle_ping(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let message = args
            .and_then(|v| v.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("pong");
        
        let content = format!("Server: winx-code-agent v{}\nResponse: {}", self.version, message);
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    async fn handle_initialize(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let shell = args
            .and_then(|v| v.get("shell"))
            .and_then(|v| v.as_str())
            .unwrap_or("bash");
        
        let mut bash_state_guard = self.bash_state.lock().await;
        if bash_state_guard.is_some() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Shell environment is already initialized".to_string()
            )]));
        }

        let mut state = crate::state::BashState::new();
        match state.init_interactive_bash() {
            Ok(_) => {
                *bash_state_guard = Some(state);
                info!("Shell environment initialized with {}", shell);
                Ok(CallToolResult::success(vec![Content::text(
                    format!("Shell environment initialized with {}", shell)
                )]))
            }
            Err(e) => {
                warn!("Failed to initialize shell: {}", e);
                Err(McpError::internal_error(format!("Failed to initialize shell: {}", e), None))
            }
        }
    }

    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let command = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing command", None))?;
        let timeout_seconds = args.get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(30) as f32;
        
        let mut bash_state_guard = self.bash_state.lock().await;
        if bash_state_guard.is_none() {
            return Err(McpError::invalid_request("Shell not initialized. Call initialize first.", None));
        }

        let bash_state = bash_state_guard.as_mut().unwrap();
        
        match bash_state.execute_interactive(command, timeout_seconds).await {
            Ok(output) => {
                let working_dir = bash_state.cwd.display().to_string();
                let content = format!("Working directory: {}\n\n{}", working_dir, output);
                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(e) => {
                warn!("Command execution failed: {}", e);
                Err(McpError::internal_error(format!("Command execution failed: {}", e), None))
            }
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let paths = args.get("paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::invalid_request("Missing paths array", None))?;
        
        let mut content_parts = Vec::new();
        
        for path_value in paths {
            let path = path_value.as_str()
                .ok_or_else(|| McpError::invalid_request("Invalid path in array", None))?;
            
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    content_parts.push(format!("=== {} ({} bytes) ===\n{}\n", path, content.len(), content));
                }
                Err(e) => {
                    content_parts.push(format!("=== {} ===\nERROR: {}\n", path, e));
                }
            }
        }
        
        Ok(CallToolResult::success(vec![Content::text(content_parts.join("\n"))]))
    }

    async fn handle_file_write_or_edit(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing path", None))?;
        let content = args.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing content", None))?;
        let create = args.get("create_if_missing")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        
        if !create && !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err(McpError::invalid_request(format!("File does not exist: {}", path), None));
        }

        match tokio::fs::write(path, content).await {
            Ok(_) => {
                info!("File written successfully: {}", path);
                Ok(CallToolResult::success(vec![Content::text(
                    format!("File written successfully: {} ({} bytes)", path, content.len())
                )]))
            }
            Err(e) => {
                warn!("Failed to write file {}: {}", path, e);
                Err(McpError::internal_error(format!("Failed to write file {}: {}", path, e), None))
            }
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create service and initialize NVIDIA integration
    let service = WinxService::new();

    // Temporarily disable NVIDIA initialization to debug MCP issues
    // TODO: Re-enable after fixing the connection issue
    // if let Err(e) = service.initialize_nvidia().await {
    //     warn!("Could not initialize NVIDIA integration: {}", e);
    // }
    info!("NVIDIA integration temporarily disabled for debugging");

    // Create and run the server with STDIO transport
    let server = service.serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {}", e);
    })?;
    server.waiting().await?;

    Ok(())
}
