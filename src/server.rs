//! Server module for the Winx application.
//!
//! This module provides functionality for starting and managing the Model Context Protocol
//! server using stdio transport. It handles the lifecycle of the server and all
//! communication with the client.

use rmcp::{transport::stdio, ServiceExt};

use crate::errors::{Result, WinxError};
use crate::tools;

/// Configuration for the server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Whether to use a simulated environment for testing
    pub simulation_mode: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            simulation_mode: false,
        }
    }
}

/// Runs the MCP server using the stdio transport
///
/// This function initializes the Winx service, connects it to the stdio transport,
/// and waits for the service to complete. It handles proper error reporting and
/// graceful shutdown.
///
/// # Returns
///
/// Returns a Result indicating whether the server ran successfully.
///
/// # Errors
///
/// Returns an error if the server fails to start or encounters an error during operation.
pub async fn run_server() -> Result<()> {
    // Measure startup time
    let start_time = std::time::Instant::now();

    // Initialize the Winx service
    tracing::debug!("Initializing server...");
    let service = tools::WinxService::new()
        .serve(stdio())
        .await
        .map_err(|e| {
            WinxError::ShellInitializationError(format!("Failed to start MCP service: {}", e))
        })?;

    // Log successful startup
    let startup_duration = start_time.elapsed();
    tracing::info!(
        "Server started and connected successfully in {:.2?}",
        startup_duration
    );

    // Create a task to periodically report the server status
    let status_reporter = tokio::spawn(async {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 minutes
        loop {
            interval.tick().await;
            tracing::debug!("Server is running (periodic status report)");
        }
    });

    // Wait for the service to complete, with proper error handling
    match service.waiting().await {
        Ok(_) => {
            tracing::info!("Server completed normally");
            // Cancel the status reporter
            status_reporter.abort();
            Ok(())
        }
        Err(e) => {
            // Cancel the status reporter
            status_reporter.abort();
            Err(WinxError::ShellInitializationError(format!(
                "Server error: {}",
                e
            )))
        }
    }
}

/// Initializes the server with a custom configuration
///
/// This is a more flexible version of run_server that allows customizing the server behavior.
///
/// # Arguments
///
/// * `config` - The server configuration
///
/// # Returns
///
/// Returns a Result indicating whether the server ran successfully.
pub async fn run_server_with_config(config: ServerConfig) -> Result<()> {
    tracing::info!("Starting server with custom configuration: {:?}", config);

    if config.simulation_mode {
        tracing::warn!("Running in simulation mode - some features may be limited");
        // In a real implementation, you would use a different service implementation
        // or mock certain components
    }

    run_server().await
}
