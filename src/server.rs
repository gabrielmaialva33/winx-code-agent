//! Winx MCP Server implementation using rmcp 0.5.0
//! Enhanced server with NVIDIA AI integration

use rmcp::{
    ErrorData as McpError,
    ServiceExt, 
    model::*, 
    tool, 
    tool_router,
    tool_handler,
    transport::stdio,
    handler::server::tool::ToolCallContext,
    handler::server::router::tool::ToolRouter,
    ServerHandler
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::state::BashState;

/// Winx service with shared bash state and NVIDIA AI integration
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    pub bash_state: Arc<Mutex<Option<BashState>>>,
    /// NVIDIA client for AI-powered features (optional)
    pub nvidia_client: Arc<Mutex<Option<NvidiaClient>>>,
    /// Version information for the service
    pub version: String,
    /// Tool router for handling tool calls
    pub tool_router: ToolRouter<Self>,
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
            tool_router: Self::tool_router(),
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

/// Core tool implementations with proper rmcp 0.5.0 pattern
#[tool_router]
impl WinxService {
    /// Simple ping tool for testing connectivity
    #[tool(description = "Test server connectivity")]
    async fn ping(&self, message: Option<String>) -> Result<CallToolResult, McpError> {
        let response = message.unwrap_or_else(|| "pong".to_string());
        let content = format!("Server: winx-code-agent v{}\nResponse: {}", self.version, response);
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    /// Initialize the bash shell environment
    #[tool(description = "Initialize the bash shell environment")]
    async fn initialize(&self, shell: Option<String>) -> Result<CallToolResult, McpError> {
        let shell = shell.unwrap_or_else(|| "bash".to_string());
        
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
                Err(McpError::internal_error(format!("Failed to initialize shell: {}", e)))
            }
        }
    }

    /// Execute a bash command
    #[tool(description = "Execute a command in the bash shell")]
    async fn bash_command(
        &self,
        command: String,
        timeout_seconds: Option<u64>,
    ) -> Result<CallToolResult, McpError> {
        let timeout_secs = timeout_seconds.unwrap_or(30) as f32;
        
        let mut bash_state_guard = self.bash_state.lock().await;
        if bash_state_guard.is_none() {
            return Err(McpError::invalid_request("Shell not initialized. Call initialize first."));
        }

        let bash_state = bash_state_guard.as_mut().unwrap();
        
        match bash_state.execute_interactive(&command, timeout_secs).await {
            Ok(output) => {
                let working_dir = bash_state.cwd.display().to_string();
                let content = format!("Working directory: {}\n\n{}", working_dir, output);
                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(e) => {
                warn!("Command execution failed: {}", e);
                Err(McpError::internal_error(format!("Command execution failed: {}", e)))
            }
        }
    }

    /// Read file contents
    #[tool]
    async fn read_files(&self, paths: Vec<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let mut results = Vec::new();
        
        for path in paths {
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    results.push(serde_json::json!({
                        "path": path,
                        "status": "success",
                        "content": content,
                        "size": content.len()
                    }));
                }
                Err(e) => {
                    results.push(serde_json::json!({
                        "path": path,
                        "status": "error",
                        "error": e.to_string()
                    }));
                }
            }
        }
        
        Ok(serde_json::json!({
            "status": "success",
            "files": results
        }))
    }

    /// Write or edit file contents
    #[tool]
    async fn file_write_or_edit(
        &self,
        path: String,
        content: String,
        create_if_missing: Option<bool>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let create = create_if_missing.unwrap_or(true);
        
        if !create && !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File does not exist: {}", path)
            ) as std::io::Error) as Box<dyn std::error::Error + Send + Sync>);
        }

        match tokio::fs::write(&path, &content).await {
            Ok(_) => {
                info!("File written successfully: {}", path);
                Ok(serde_json::json!({
                    "status": "success",
                    "message": format!("File written successfully: {}", path),
                    "path": path,
                    "size": content.len()
                }))
            }
            Err(e) => {
                warn!("Failed to write file {}: {}", path, e);
                Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
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
