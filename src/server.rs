//! Server module for the Winx application.
//!
//! This module provides functionality for starting and managing the Model Context Protocol
//! server using stdio transport. It handles the lifecycle of the server and all
//! communication with the client.

use rmcp::{transport::stdio, ServiceExt};

use crate::errors::{Result, WinxError};
use crate::tools;
use std::time::SystemTime;

/// Configuration for the server
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    /// Whether to use a simulated environment for testing
    pub simulation_mode: bool,
    /// Enable debug mode with enhanced error reporting
    pub debug_mode: bool,
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
    // Use default configuration
    run_server_with_config(ServerConfig::default()).await
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

    if config.debug_mode {
        tracing::info!("Running in debug mode - enhanced error reporting enabled");

        // Enable stack traces for errors
        std::env::set_var("RUST_BACKTRACE", "1");

        // Create a debug log file
        let _debug_log = std::fs::File::create("winx_debug.log").map_err(|e| {
            WinxError::ShellInitializationError(format!("Failed to create debug log: {}", e))
        })?;

        // Log system information
        let sys_info = format!(
            "System Info:\n\
            - OS: {}\n\
            - Arch: {}\n\
            - Version: {}\n\
            - Debug Mode: {}\n\
            - Simulation Mode: {}\n\
            - Rust Version: {}\n\
            - Time: {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            env!("CARGO_PKG_VERSION"),
            config.debug_mode,
            config.simulation_mode,
            "unknown",
            SystemTime::now(),
        );

        // Log to console and file
        tracing::info!("Debug System Info:\n{}", sys_info);

        if let Err(e) = std::fs::write("winx_debug_info.txt", sys_info) {
            tracing::warn!("Failed to write debug info to file: {}", e);
        }
    }

    // Use timeout for the server startup
    let start_time = std::time::Instant::now();

    // Initialize the Winx service
    tracing::debug!("Initializing server...");

    // Create service with debug mode if enabled
    let service = tools::WinxService::new();

    // Use a timeout for server initialization to avoid hanging
    let service_future = service.serve(stdio());
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(30), // 30 second timeout
        service_future,
    )
    .await
    .map_err(|_| {
        WinxError::ShellInitializationError(
            "Server initialization timed out after 30 seconds".to_string(),
        )
    })?
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
    let status_reporter = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 minutes
        loop {
            interval.tick().await;

            // Add more detailed status in debug mode
            if config.debug_mode {
                let mem_usage = match std::process::Command::new("ps")
                    .args(["o", "rss=", "-p", &std::process::id().to_string()])
                    .output()
                {
                    Ok(output) => {
                        let output_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if let Ok(kb) = output_str.parse::<u64>() {
                            format!("{:.2} MB", kb as f64 / 1024.0)
                        } else {
                            "Unknown".to_string()
                        }
                    }
                    Err(_) => "Unknown".to_string(),
                };

                tracing::debug!(
                    "Server is running (periodic status report) - Memory usage: {}, Uptime: {:.2?}",
                    mem_usage,
                    start_time.elapsed()
                );
            } else {
                tracing::debug!("Server is running (periodic status report)");
            }
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

            // More detailed error reporting in debug mode
            if config.debug_mode {
                tracing::error!("Server error details: {:?}", e);

                // Try to log error details to file
                if let Err(log_err) = std::fs::write(
                    "winx_error_log.txt",
                    format!("Error time: {:?}\nError: {:?}\n", SystemTime::now(), e),
                ) {
                    tracing::warn!("Failed to write error log: {}", log_err);
                }
            }

            Err(WinxError::ShellInitializationError(format!(
                "Server error: {}",
                e
            )))
        }
    }
}
