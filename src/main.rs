//! Winx - A Rust implementation of WCGW shell tools
//!
//! This crate provides tools for working with a bash shell environment through MCP (Model Context Protocol).
//! It allows AI models like Claude to interact with a shell environment to execute commands and manage
//! resources in a controlled and safe manner.

mod bash;
mod error;
mod service;
mod tools;
mod types;

use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::{self, EnvFilter};

use crate::bash::{BashState, Context, SimpleConsole};
use crate::service::WinxService;

/// Main entry point for the application.
///
/// Sets up logging, initializes the service, and starts the MCP server
/// using stdio for communication.
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting Winx MCP server");

    // Create the bash state
    let bash_state = BashState::new(Box::new(SimpleConsole), "", None, None, None, None, None)?;

    // Create the context and service
    let context = Context::new(bash_state);
    let service = WinxService::new(context);

    // Serve using stdio
    let service = service.serve(stdio()).await?;

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}
