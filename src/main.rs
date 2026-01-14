//! # Winx - High Performance MCP Server
//!
//! Winx is a high-performance Rust implementation of the Model Context Protocol (MCP).
//! It provides core tools for shell execution and file management with extreme efficiency.

mod errors;
mod server;
mod state;
mod tools;
mod types;
mod utils;

use clap::Parser;
use errors::Result;

/// Winx - High Performance MCP Server
#[derive(Parser)]
#[command(name = "winx")]
#[command(author = "Gabriel Maia")]
#[command(version)]
#[command(about = "High-performance MCP server for shell and file operations", long_about = None)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Start MCP server (default)
    Serve {
        /// Enable debug mode with enhanced error reporting
        #[arg(long)]
        debug_mode: bool,
    },
}

/// Logging setup
fn setup_logging(verbose: bool, debug: bool) {
    let level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    setup_logging(cli.verbose, cli.debug);

    match cli.command {
        // MCP Server mode is the default and only mode
        None | Some(Commands::Serve { .. }) => {
            run_server().await
        }
    }
}

/// Executes the MCP server
async fn run_server() -> Result<()> {
    tracing::info!("Starting winx MCP server v{}", env!("CARGO_PKG_VERSION"));

    match server::start_winx_server().await {
        Ok(()) => {
            tracing::info!("Server shutting down normally");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Server error: {}", e);
            Err(errors::WinxError::ShellInitializationError(format!(
                "Failed to start server: {e}"
            )))
        }
    }
}