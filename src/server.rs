use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};

use crate::tools;

/// Runs the MCP server using the stdio transport
pub async fn run_server() -> Result<()> {
    // Initialize our service
    tracing::debug!("Initializing server...");
    let service = tools::WinxService::new().serve(stdio()).await?;

    tracing::info!("Server started and connected successfully");

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}
