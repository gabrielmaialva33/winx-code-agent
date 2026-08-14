#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use winx_code_agent::daemon::DaemonServer;

#[cfg(unix)]
#[derive(Parser)]
#[command(name = "winx-guardian", version, about = "Stable owner of one Winx shell session")]
struct Cli {
    #[arg(long)]
    socket: PathBuf,
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    DaemonServer::bind(cli.socket).await?.serve().await?;
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("winx-guardian requires a Unix platform");
    std::process::exit(1);
}
