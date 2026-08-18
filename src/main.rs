//! # Winx - Remote-first MCP runtime
//!
//! Winx exposes durable shell, file, and code-navigation tools over hardened
//! Streamable HTTP, with stdio retained for local MCP clients.

use std::io::Write;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

use clap::Parser;
#[cfg(unix)]
use winx_code_agent::daemon::{default_socket_path, DaemonClient};
#[cfg(unix)]
use winx_code_agent::runtime::{configured_daemon_binary, restart_control_daemon_at};
use winx_code_agent::{start_winx_server, Result, WinxError};

/// Winx - Remote-first MCP runtime
#[derive(Parser)]
#[command(name = "winx-code-agent")]
#[command(author = "Gabriel Maia")]
#[command(version)]
#[command(
    about = "Remote-first MCP runtime with Streamable HTTP, durable PTYs, and guarded shell/file tools",
    long_about = None
)]
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
        /// Serve over Streamable HTTP instead of stdio, for remote MCP clients
        /// (e.g. `ChatGPT` developer-mode connectors). Requires --token.
        #[arg(long)]
        http: bool,

        /// Address for the HTTP transport. Keep it on loopback and put an
        /// HTTPS tunnel in front — do not expose 0.0.0.0 on untrusted networks.
        #[arg(long, default_value = "127.0.0.1:8000")]
        bind: String,

        /// Shared secret required on every HTTP request. Prefer --token-file so
        /// the secret is not visible in process arguments. Falls back to
        /// `WINX_HTTP_TOKEN` when no other credential source is provided.
        #[arg(long, conflicts_with_all = ["token_file", "principal_config"])]
        token: Option<String>,

        /// Read the single-principal bearer token from a chmod-600 file.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["token", "principal_config"])]
        token_file: Option<PathBuf>,

        /// TOML file containing multiple [[principals]] entries, each backed by
        /// `token_file` or `token_env`. Sessions and Tasks are isolated per principal.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["token", "token_file"])]
        principal_config: Option<PathBuf>,

        /// Permit a token shorter than 32 bytes. Intended only for local tests.
        #[arg(long)]
        allow_weak_token: bool,

        /// Permit binding HTTP to a non-loopback address. Off by default because
        /// this endpoint provides shell and filesystem access.
        #[arg(long)]
        allow_non_loopback: bool,

        /// Session key used by remote first-call initialization. `workspace`
        /// reuses one durable shell per principal/project; `thread` trusts the
        /// caller's `thread_id` and requires explicit lifecycle management.
        #[arg(
            long,
            value_enum,
            default_value_t = winx_code_agent::config::HttpSessionAffinity::Workspace
        )]
        session_affinity: winx_code_agent::config::HttpSessionAffinity,

        /// Maximum HTTP requests executing concurrently.
        #[arg(
            long,
            default_value_t = winx_code_agent::http_server::DEFAULT_MAX_CONCURRENCY
        )]
        max_concurrency: usize,

        /// Fixed-window request limit per source IP.
        #[arg(
            long,
            default_value_t = winx_code_agent::http_server::DEFAULT_REQUESTS_PER_MINUTE
        )]
        requests_per_minute: u32,

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
    #[cfg(unix)]
    List {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Read a session's output using an independent consumer cursor
    #[cfg(unix)]
    Attach {
        thread_id: String,
        #[arg(long, default_value = "cli")]
        consumer: String,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Stop and remove one or every daemon-owned shell session
    #[cfg(unix)]
    Kill {
        #[arg(required_unless_present = "all")]
        thread_id: Option<String>,
        #[arg(long, conflicts_with = "thread_id")]
        all: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Remove idle or unreachable daemon-owned shell sessions
    #[cfg(unix)]
    Prune {
        /// Override the configured idle TTL for this run. Zero prunes every idle session.
        #[arg(long)]
        idle_seconds: Option<u64>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Restart winxd without terminating per-session guardians or PTYs
    #[cfg(unix)]
    RestartDaemon {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Print a redacted runtime/configuration diagnostic report
    Doctor,
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

    let filter = tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into());
    // Usage events (one per authenticated HTTP request / tool call) stay
    // visible at the default WARN level so persistent usage logs never depend
    // on the verbosity flags. The directive is static and cannot fail to parse.
    let filter = match "winx::usage=info".parse() {
        Ok(directive) => filter.add_directive(directive),
        Err(_) => filter,
    };

    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    // WINX_LOG_FORMAT=json emits one JSON object per line, for journald + jq.
    if winx_code_agent::config::env_text("WINX_LOG_FORMAT")
        .is_some_and(|format| format.eq_ignore_ascii_case("json"))
    {
        builder.json().init();
    } else {
        builder.with_ansi(true).init();
    }
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
            http: true,
            bind,
            token,
            token_file,
            principal_config,
            allow_weak_token,
            allow_non_loopback,
            session_affinity,
            max_concurrency,
            requests_per_minute,
            allowed_host,
            allow_query_token,
        }) => {
            run_http_server(
                bind,
                token,
                token_file,
                principal_config,
                allow_weak_token,
                allow_non_loopback,
                session_affinity,
                max_concurrency,
                requests_per_minute,
                allowed_host,
                allow_query_token,
            )
            .await
        }
        #[cfg(unix)]
        Some(Commands::List { socket }) => run_list(socket).await,
        #[cfg(unix)]
        Some(Commands::Attach { thread_id, consumer, follow, socket }) => {
            run_attach(socket, thread_id, consumer, follow).await
        }
        #[cfg(unix)]
        Some(Commands::Kill { thread_id, all, socket }) => run_kill(socket, thread_id, all).await,
        #[cfg(unix)]
        Some(Commands::Prune { idle_seconds, socket }) => run_prune(socket, idle_seconds).await,
        #[cfg(unix)]
        Some(Commands::RestartDaemon { socket }) => run_restart_daemon(socket).await,
        Some(Commands::Doctor) => run_doctor().await,
        // Default: stdio transport for local MCP clients.
        None | Some(Commands::Serve { .. }) => run_server().await,
    }
}

#[cfg(unix)]
fn daemon_client(socket: Option<PathBuf>) -> DaemonClient {
    DaemonClient::new(socket.unwrap_or_else(default_socket_path))
}

#[cfg(unix)]
async fn run_list(socket: Option<PathBuf>) -> Result<()> {
    let sessions = daemon_client(socket).list_sessions().await?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &sessions)
        .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    writeln!(std::io::stdout().lock())?;
    Ok(())
}

#[cfg(unix)]
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

#[cfg(unix)]
async fn run_kill(socket: Option<PathBuf>, thread_id: Option<String>, all: bool) -> Result<()> {
    let client = daemon_client(socket);
    if all {
        let sessions = client.list_sessions().await?;
        let mut killed = 0usize;
        for session in sessions {
            if client.kill_session(&session.thread_id).await? {
                killed += 1;
            }
        }
        writeln!(std::io::stdout().lock(), "killed {killed} session(s)")?;
        return Ok(());
    }

    let thread_id = thread_id
        .ok_or_else(|| WinxError::InvalidInput("provide a thread_id or pass --all".to_string()))?;
    let killed = client.kill_session(&thread_id).await?;
    writeln!(std::io::stdout().lock(), "{}", if killed { "killed" } else { "not found" })?;
    Ok(())
}

#[cfg(unix)]
async fn run_prune(socket: Option<PathBuf>, idle_seconds: Option<u64>) -> Result<()> {
    let result = daemon_client(socket).prune_sessions(idle_seconds).await?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &result)
        .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    writeln!(std::io::stdout().lock())?;
    Ok(())
}

#[cfg(unix)]
async fn run_restart_daemon(socket: Option<PathBuf>) -> Result<()> {
    let socket = socket.unwrap_or_else(default_socket_path);
    let binary = configured_daemon_binary()?;
    let hello = restart_control_daemon_at(&socket, &binary).await?;
    writeln!(
        std::io::stdout().lock(),
        "restarted winxd pid={} epoch={}",
        hello.daemon_pid,
        hello.daemon_epoch
    )?;
    Ok(())
}

async fn run_doctor() -> Result<()> {
    let runtime = winx_code_agent::runtime::configured_runtime_mode().map_or_else(
        |error| format!("invalid: {error}"),
        |mode| format!("{mode:?}").to_ascii_lowercase(),
    );
    let mut report = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH
        },
        "cwd": std::env::current_dir().ok().map(|path| path.display().to_string()),
        "runtime": runtime,
        "environment": {
            "sandbox_requested": winx_code_agent::config::env_flag("WINX_SANDBOX"),
            "redaction_disabled": winx_code_agent::config::env_flag("WINX_NO_REDACT"),
            "compression_disabled": winx_code_agent::config::env_flag("WINX_NO_COMPRESS"),
            "http_token_configured": winx_code_agent::config::env_text("WINX_HTTP_TOKEN").is_some()
        }
    });

    #[cfg(unix)]
    {
        let socket = default_socket_path();
        let daemon = match DaemonClient::new(&socket).hello().await {
            Ok(hello) => serde_json::json!({
                "reachable": true,
                "socket": socket.display().to_string(),
                "pid": hello.daemon_pid,
                "epoch": hello.daemon_epoch,
                "protocol": format!("{}.{}", hello.protocol_major, hello.protocol_minor),
                "capabilities": hello.capabilities
            }),
            Err(error) => serde_json::json!({
                "reachable": false,
                "socket": socket.display().to_string(),
                "error": error.to_string()
            }),
        };
        report["daemon"] = daemon;
        report["daemon_binary"] = match configured_daemon_binary() {
            Ok(path) => serde_json::json!({"available": true, "path": path.display().to_string()}),
            Err(error) => serde_json::json!({"available": false, "error": error.to_string()}),
        };
    }

    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
        .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    writeln!(std::io::stdout().lock())?;
    Ok(())
}

/// Executes the remote MCP server over Streamable HTTP.
#[allow(clippy::too_many_arguments)]
async fn run_http_server(
    bind: String,
    token: Option<String>,
    token_file: Option<PathBuf>,
    principal_config: Option<PathBuf>,
    allow_weak_token: bool,
    allow_non_loopback: bool,
    session_affinity: winx_code_agent::config::HttpSessionAffinity,
    max_concurrency: usize,
    requests_per_minute: u32,
    allowed_hosts: Vec<String>,
    allow_query_token: bool,
) -> Result<()> {
    if token.is_some() {
        tracing::warn!("--token exposes the HTTP secret in process arguments; prefer --token-file");
    }
    let principals = winx_code_agent::config::load_http_principals(
        principal_config.as_deref(),
        token,
        token_file.as_deref(),
        allow_weak_token,
    )?;
    let options = winx_code_agent::http_server::HttpServerOptions {
        bind: bind.clone(),
        principals,
        extra_hosts: allowed_hosts,
        allow_query_token,
        allow_non_loopback,
        session_affinity,
        max_concurrency,
        requests_per_minute,
    };
    tracing::info!("Starting winx remote MCP (HTTP) v{} on {bind}", env!("CARGO_PKG_VERSION"));

    winx_code_agent::http_server::start_http_server_with_options(options).await.map_err(|error| {
        WinxError::ShellInitializationError(format!("HTTP server failed: {error}"))
    })
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
