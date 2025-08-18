//! Winx MCP Server implementation using rmcp 0.5.0
//! Enhanced server with NVIDIA AI integration

use rmcp::{model::*, transport::stdio, tool, ServerHandler, ServiceExt};
use serde_json::Value;
use std::future::Future;
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

// ServerHandler implementation with NVIDIA tools
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-code-agent".into(),
                version: self.version.clone(),
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::default(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents with NVIDIA AI integration. \
                Provides shell execution, file management, and AI-powered code analysis capabilities.".into(),
            ),
        }
    }
}

/// Core tool implementations
impl WinxService {
    /// Simple ping tool for testing connectivity
    #[tool]
    async fn ping(&self, message: Option<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let response = message.unwrap_or_else(|| "pong".to_string());
        Ok(serde_json::json!({
            "status": "success",
            "message": response,
            "server": "winx-code-agent",
            "version": self.version
        }))
    }

    /// Initialize the bash shell environment
    #[tool]
    async fn initialize(&self, shell: Option<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let shell = shell.unwrap_or_else(|| "bash".to_string());
        
        let mut bash_state_guard = self.bash_state.lock().await;
        if bash_state_guard.is_some() {
            return Ok(serde_json::json!({
                "status": "already_initialized",
                "message": "Shell environment is already initialized"
            }));
        }

        let mut state = crate::state::BashState::new();
        match state.init_interactive_bash() {
            Ok(_) => {
                *bash_state_guard = Some(state);
                info!("Shell environment initialized with {}", shell);
                Ok(serde_json::json!({
                    "status": "success",
                    "message": format!("Shell environment initialized with {}", shell),
                    "shell": shell
                }))
            }
            Err(e) => {
                warn!("Failed to initialize shell: {}", e);
                Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to initialize shell: {}", e)
                ) as std::io::Error) as Box<dyn std::error::Error + Send + Sync>)
            }
        }
    }

    /// Execute a bash command
    #[tool]
    async fn bash_command(
        &self,
        command: String,
        timeout_seconds: Option<u64>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let timeout_secs = timeout_seconds.unwrap_or(30) as f32;
        
        let mut bash_state_guard = self.bash_state.lock().await;
        if bash_state_guard.is_none() {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Shell not initialized. Call initialize first."
            ) as std::io::Error) as Box<dyn std::error::Error + Send + Sync>);
        }

        let bash_state = bash_state_guard.as_mut().unwrap();
        
        match bash_state.execute_interactive(&command, timeout_secs).await {
            Ok(output) => {
                Ok(serde_json::json!({
                    "status": "success",
                    "output": output,
                    "working_directory": bash_state.cwd.display().to_string()
                }))
            }
            Err(e) => {
                warn!("Command execution failed: {}", e);
                Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Command execution failed: {}", e)
                ) as std::io::Error) as Box<dyn std::error::Error + Send + Sync>)
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
