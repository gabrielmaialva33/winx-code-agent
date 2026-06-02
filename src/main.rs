//! # Winx - High Performance MCP Server
//!
//! Winx is a high-performance Rust implementation of the Model Context Protocol (MCP).
//! It provides core tools for shell execution and file management with extreme efficiency.

use clap::Parser;
use winx_code_agent::{start_winx_server, Result, WinxError};

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
    /// Start MCP server (default: stdio transport for local clients)
    Serve {
        /// Enable debug mode with enhanced error reporting
        #[arg(long)]
        debug_mode: bool,

        /// Serve over Streamable HTTP instead of stdio, for remote MCP clients
        /// (e.g. `ChatGPT` developer-mode connectors). Requires --token.
        #[arg(long)]
        http: bool,

        /// Address for the HTTP transport. Keep it on loopback and put an
        /// HTTPS tunnel in front — do not expose 0.0.0.0 on untrusted networks.
        #[arg(long, default_value = "127.0.0.1:8000")]
        bind: String,

        /// Shared secret required on every HTTP request
        /// (`Authorization: Bearer <token>` or `?token=<token>`).
        /// Falls back to the `WINX_HTTP_TOKEN` env var.
        #[arg(long)]
        token: Option<String>,

        /// Extra Host authority to accept (your tunnel hostname, e.g.
        /// abc.trycloudflare.com). Repeatable. Loopback is always allowed.
        #[arg(long = "allowed-host")]
        allowed_host: Vec<String>,
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
        Some(Commands::Serve { http: true, bind, token, allowed_host, .. }) => {
            run_http_server(bind, token, allowed_host).await
        }
        // Default: stdio transport for local MCP clients.
        None | Some(Commands::Serve { .. }) => run_server().await,
    }
}

/// Executes the remote MCP server over Streamable HTTP.
async fn run_http_server(
    bind: String,
    token: Option<String>,
    allowed_hosts: Vec<String>,
) -> Result<()> {
    let token = token.or_else(|| std::env::var("WINX_HTTP_TOKEN").ok()).unwrap_or_default();
    tracing::info!("Starting winx remote MCP (HTTP) v{} on {bind}", env!("CARGO_PKG_VERSION"));

    winx_code_agent::http_server::start_http_server(&bind, token, allowed_hosts)
        .await
        .map_err(|e| WinxError::ShellInitializationError(format!("HTTP server failed: {e}")))
}

/// Executes the MCP server
async fn run_server() -> Result<()> {
    tracing::info!("Starting winx MCP server v{}", env!("CARGO_PKG_VERSION"));

    match start_winx_server().await {
        Ok(()) => {
            tracing::info!("Server shutting down normally");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Server error: {}", e);
            Err(WinxError::ShellInitializationError(format!("Failed to start server: {e}")))
        }
    }
}
