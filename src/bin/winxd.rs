#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use winx_code_agent::daemon::{default_socket_path, ControlServer};

#[cfg(unix)]
#[derive(Parser)]
#[command(name = "winxd", version, about = "Long-lived Winx shell daemon")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    tracing::info!(socket = %socket.display(), "starting winxd");
    ControlServer::bind(socket).await?.serve().await?;
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("winxd requires a Unix platform");
    std::process::exit(1);
}
