use std::path::PathBuf;

use clap::Parser;
use winx_code_agent::daemon::DaemonServer;

#[derive(Parser)]
#[command(name = "winx-guardian", version, about = "Stable owner of one Winx shell session")]
struct Cli {
    #[arg(long)]
    socket: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    DaemonServer::bind(cli.socket).await?.serve().await?;
    Ok(())
}
