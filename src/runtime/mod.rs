//! Shell execution boundary shared by embedded and daemon-backed runtimes.

mod selection;
mod session_store;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::errors::Result;
use crate::state::bash_state::BashState;
use crate::types::BashCommand;

pub use selection::{
    configured_runtime_mode, configured_shell_runtime, ensure_daemon_at, select_runtime_mode,
    RuntimeMode,
};
pub(crate) use session_store::lock_session_store;
pub use session_store::{SessionStore, ShellTarget};

/// Boxed future returned by [`ShellRuntime`] implementations.
pub type ShellRuntimeFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
pub type ShellRuntimeUnitFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type ShellRuntimeConfigureFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>>> + Send + 'a>>;

/// State transition requested by the Initialize tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSessionTransition {
    FirstCall,
    ModeChange,
    Reset,
    WorkspaceChange,
}

/// Narrow process-agnostic boundary for BashCommand execution.
pub trait ShellRuntime: Send + Sync {
    fn configure_session<'a>(
        &'a self,
        bash_state: &'a mut BashState,
        transition: ShellSessionTransition,
    ) -> ShellRuntimeConfigureFuture<'a>;

    fn run_action<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> ShellRuntimeFuture<'a>;

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a>;
}

/// Current in-process runtime. It intentionally delegates to the existing
/// implementation so introducing the boundary does not change behavior.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedShellRuntime;

impl ShellRuntime for EmbeddedShellRuntime {
    fn configure_session<'a>(
        &'a self,
        bash_state: &'a mut BashState,
        transition: ShellSessionTransition,
    ) -> ShellRuntimeConfigureFuture<'a> {
        Box::pin(async move {
            if matches!(
                transition,
                ShellSessionTransition::FirstCall | ShellSessionTransition::Reset
            ) {
                if transition == ShellSessionTransition::Reset || bash_state.cwd.exists() {
                    bash_state.init_pty_shell().await?;
                }
            }
            let attach_hint = {
                let shell = bash_state.pty_shell.lock().await;
                shell.as_ref().and_then(|shell| shell.attach_hint.clone())
            };
            Ok(attach_hint)
        })
    }

    fn run_action<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> ShellRuntimeFuture<'a> {
        Box::pin(crate::tools::bash_command::handle_embedded_tool_call(bash_state, command))
    }

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(interrupt_embedded(bash_state))
    }
}

async fn interrupt_embedded(bash_state: &Arc<Mutex<Option<BashState>>>) -> Result<()> {
    let shell = {
        let state = bash_state.lock().await;
        state.as_ref().map(|state| state.pty_shell.clone())
    };
    let Some(shell) = shell else { return Ok(()) };
    {
        let mut guard = shell.lock().await;
        if let Some(pty) = guard.as_mut() {
            pty.send_interrupt().map_err(|error| {
                crate::errors::WinxError::CommandExecutionError(format!(
                    "failed to interrupt shell: {error}"
                ))
            })?;
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let recovered = {
            let mut guard = shell.lock().await;
            match guard.as_mut() {
                Some(pty) => pty.poll_output_nonblocking() || !pty.command_running,
                None => true,
            }
        };
        if recovered {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(crate::errors::WinxError::CommandExecutionError(
        "interrupted shell did not return to a prompt within 3 seconds".to_string(),
    ))
}
