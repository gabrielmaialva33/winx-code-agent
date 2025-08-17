//! Winx MCP Server implementation using rmcp 0.5.0
//! Following the tool_router pattern from official examples

use rmcp::{
    model::*,
    tool, tool_router,
    transport::stdio,
    ServerHandler,
    ServiceExt,
    ErrorData as McpError,
};
use std::sync::{Arc, Mutex};
use std::future::Future;
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

#[tool_router]
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
    #[tool(description = "Initialize the shell environment with workspace path and configuration")]
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
    #[tool(description = "Execute a bash command with stateful session management")]
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
            .map_err(|e| McpError::internal_error(e.to_string()))?;

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
    #[tool(description = "Read full file content of one or more files with optional line ranges")]
    async fn read_files(
        &self,
        paths: Vec<String>,
        include_line_numbers: Option<bool>,
    ) -> Result<CallToolResult, McpError> {
        if paths.is_empty() {
            return Err(McpError::invalid_params("No file paths provided"));
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
    #[tool(description = "Write content to a file, creating directories if needed")]
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
                .map_err(|e| McpError::internal_error(e.to_string()))?;
        }

        tokio::fs::write(&expanded_path, &content)
            .await
            .map_err(|e| McpError::internal_error(e.to_string()))?;

        // Set executable if requested
        if is_executable.unwrap_or(false) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = tokio::fs::metadata(&expanded_path)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string()))?
                    .permissions();
                perms.set_mode(perms.mode() | 0o755);
                tokio::fs::set_permissions(&expanded_path, perms)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string()))?;
            }
        }

        let result = format!("File written successfully: {}", path);
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

// Implement ServerHandler for the service
#[tool_handler]
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            name: "winx-code-agent".to_string(),
            version: self.version.clone(),
            instructions: Some("Winx is a high-performance Rust implementation of WCGW for code agents. It provides shell execution and file management capabilities.".into()),
            capabilities: ServerCapabilities::builder().tools().build(),
            ..Default::default()
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