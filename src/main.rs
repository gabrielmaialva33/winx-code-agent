//! # Winx - High Performance MCP Server
//!
//! Winx is a high-performance Rust implementation of the Model Context Protocol (MCP).
//! It provides core tools for shell execution and file management with extreme efficiency.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use winx_code_agent::daemon::{default_socket_path, DaemonClient};
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

        /// Shared secret required on every HTTP request, sent as
        /// `Authorization: Bearer <token>`. Falls back to the `WINX_HTTP_TOKEN`
        /// env var.
        #[arg(long)]
        token: Option<String>,

        /// Extra Host authority to accept (your tunnel hostname, e.g.
        /// abc.trycloudflare.com). Repeatable. Loopback is always allowed.
        #[arg(long = "allowed-host")]
        allowed_host: Vec<String>,

        /// Also accept the token via a `?token=<token>` query parameter, for
        /// clients that only take a URL (e.g. `ChatGPT` MCP connectors). OFF by
        /// default: the header is preferred because a query token leaks into
        /// tunnel/proxy access logs and browser history. Use only on
        /// ephemeral/trusted tunnels.
        #[arg(long)]
        allow_query_token: bool,
    },

    /// List shell sessions owned by winxd
    List {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Read a session's output using an independent consumer cursor
    Attach {
        thread_id: String,
        #[arg(long, default_value = "cli")]
        consumer: String,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Stop and remove a daemon-owned shell session
    Kill {
        thread_id: String,
        #[arg(long)]
        socket: Option<PathBuf>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    setup_logging(cli.verbose, cli.debug);

    // Opt-in Landlock sandbox (WINX_SANDBOX=1), confining writes to the cwd (the
    // workspace) + /tmp. Applied BEFORE the tokio runtime is built, so its worker
    // threads - and the PTY shell forked from one of them - inherit the Landlock
    // domain (restrict_self only covers the calling thread and its future
    // children, and a #[tokio::main] runtime's workers already exist by the time
    // the async body runs). No-op unless the env var is set.
    winx_code_agent::sandbox::apply_if_requested();

    tokio::runtime::Runtime::new()
        .map_err(|e| {
            WinxError::ShellInitializationError(format!("failed to start tokio runtime: {e}"))
        })?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Serve {
            http: true, bind, token, allowed_host, allow_query_token, ..
        }) => run_http_server(bind, token, allowed_host, allow_query_token).await,
        Some(Commands::List { socket }) => run_list(socket).await,
        Some(Commands::Attach { thread_id, consumer, follow, socket }) => {
            run_attach(socket, thread_id, consumer, follow).await
        }
        Some(Commands::Kill { thread_id, socket }) => run_kill(socket, thread_id).await,
        // Default: stdio transport for local MCP clients.
        None | Some(Commands::Serve { .. }) => run_server().await,
    }
}

fn daemon_client(socket: Option<PathBuf>) -> DaemonClient {
    DaemonClient::new(socket.unwrap_or_else(default_socket_path))
}

async fn run_list(socket: Option<PathBuf>) -> Result<()> {
    let sessions = daemon_client(socket).list_sessions().await?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &sessions)
        .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    writeln!(std::io::stdout().lock())?;
    Ok(())
}

async fn run_attach(
    socket: Option<PathBuf>,
    thread_id: String,
    consumer: String,
    follow: bool,
) -> Result<()> {
    let client = daemon_client(socket);
    loop {
        let read = client.read_output(&thread_id, &consumer).await?;
        if read.gap {
            writeln!(std::io::stderr().lock(), "(...daemon scrollback truncated...)")?;
        }
        if !read.output.is_empty() {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(read.output.as_bytes())?;
            stdout.flush()?;
        }
        if !follow {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_kill(socket: Option<PathBuf>, thread_id: String) -> Result<()> {
    let killed = daemon_client(socket).kill_session(&thread_id).await?;
    writeln!(std::io::stdout().lock(), "{}", if killed { "killed" } else { "not found" })?;
    Ok(())
}

/// Executes the remote MCP server over Streamable HTTP.
async fn run_http_server(
    bind: String,
    token: Option<String>,
    allowed_hosts: Vec<String>,
    allow_query_token: bool,
) -> Result<()> {
    let token = token.or_else(|| std::env::var("WINX_HTTP_TOKEN").ok()).unwrap_or_default();
    tracing::info!("Starting winx remote MCP (HTTP) v{} on {bind}", env!("CARGO_PKG_VERSION"));

    winx_code_agent::http_server::start_http_server(&bind, token, allowed_hosts, allow_query_token)
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
