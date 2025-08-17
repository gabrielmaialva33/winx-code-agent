//! Winx MCP Server implementation using rmcp 0.5.0
//! Minimal working server implementation for now

use rmcp::{model::*, transport::stdio, ServerHandler, ServiceExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::state::BashState;

/// Winx service with shared bash state
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
}

// Minimal ServerHandler implementation for compilation
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-code-agent".into(),
                version: self.version.clone().into(),
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::default(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents.".into(),
            ),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create and run the server with STDIO transport
    let service = WinxService::new().serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {}", e);
    })?;
    service.waiting().await?;

    Ok(())
}
