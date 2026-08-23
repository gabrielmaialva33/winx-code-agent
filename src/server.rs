//! Winx MCP server facade.
//!
//! Protocol catalog, session lifecycle, task handling, MCP dispatch, and tool
//! adapters live in focused submodules under `server/`.

mod catalog;
mod coherence;
mod handler;
mod outcomes;
mod principal;
mod sessions;
mod tasks;
mod tool_dispatch;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use rmcp::{
    transport::{async_rw::AsyncRwTransport, stdio},
    ServiceExt,
};
use tokio::sync::Mutex;
use tracing::info;

use crate::runtime::{configured_shell_runtime, ShellRuntime};
use crate::state::task_state::TaskRegistry;
use crate::state::BashState;
use sessions::SessionRegistry;

pub use sessions::SessionIsolation;

/// Shared shell state for one logical Winx session.
pub type SharedBashState = Arc<Mutex<Option<BashState>>>;

/// Winx service shared by stdio or HTTP transports.
#[derive(Clone)]
pub struct WinxService {
    sessions: Arc<Mutex<SessionRegistry>>,
    tasks: Arc<Mutex<TaskRegistry>>,
    root_bootstrap: Arc<Mutex<()>>,
    shell_runtime: Arc<dyn ShellRuntime>,
    /// Version information advertised in the MCP handshake.
    pub version: String,
    isolation: SessionIsolation,
}

/// Create and start the Winx MCP server.
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    start_winx_server_with_runtime(configured_shell_runtime().await?).await
}

/// Create and start the stdio MCP server with an explicit shell runtime.
pub async fn start_winx_server_with_runtime(
    runtime: Arc<dyn ShellRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server");
    let service = WinxService::with_runtime(SessionIsolation::Lenient, runtime);
    let (stdin, stdout) = stdio();
    let transport = AsyncRwTransport::new_server(stdin, stdout);
    let server = service.serve(transport).await?;
    server.waiting().await?;
    Ok(())
}
