//! Shell execution boundary shared by embedded and daemon-backed runtimes.

mod selection;
mod session_store;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::state::pty::SharedPtyShell;
use crate::tools::bash_command::BashCommandResult;
use crate::types::BashCommand;

#[cfg(unix)]
pub use selection::{
    configured_daemon_binary, ensure_control_daemon_at, ensure_daemon_at, restart_control_daemon_at,
};
pub use selection::{
    configured_runtime_mode, configured_shell_runtime, select_runtime_mode, RuntimeMode,
};
pub(crate) use session_store::lock_session_store;
pub use session_store::{SessionStore, ShellTarget};

/// Boxed future returned by [`ShellRuntime`] implementations.
pub type ShellRuntimeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BashCommandResult>> + Send + 'a>>;
pub type ShellRuntimeUnitFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type ShellRuntimeBoolFuture<'a> = Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;
pub type ShellRuntimeDetailedFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BashCommandRuntimeResult>> + Send + 'a>>;

/// Internal orchestration details layered around the stable public
/// `BashCommandResult`. Adding a separate type avoids adding required fields to
/// the existing public result struct.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BashCommandRuntimeResult {
    pub result: BashCommandResult,
    pub compact_output: Option<String>,
    pub command_generation: Option<u64>,
    pub execution_token: Option<ShellExecutionToken>,
    pub generation_bound_actions: bool,
    /// Authoritative runtime truncation state. It is never inferred from
    /// terminal text, which is controlled by the child process.
    pub output_truncated: bool,
}

impl BashCommandRuntimeResult {
    pub fn legacy(result: BashCommandResult) -> Self {
        Self {
            result,
            compact_output: None,
            command_generation: None,
            execution_token: None,
            generation_bound_actions: false,
            output_truncated: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellExecutionToken {
    pub guardian_epoch: String,
    pub session_epoch: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShellActionOptions {
    pub compact_output: bool,
    pub expected_generation: Option<u64>,
    /// Full execution identity used by protocol 1.5 peers. The legacy numeric
    /// generation remains for source and protocol-1.4 compatibility only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_execution: Option<ShellExecutionToken>,
    /// Adapter-to-control precondition. A control process must compare this
    /// with the effective guardian immediately before relaying an action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_guardian_epoch: Option<String>,
    /// Internal safety contract for an execution that is already represented
    /// by an MCP Task and therefore cannot fall back to an unbound process.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_generation_binding: bool,
    /// Stable key for cancelling a launch that is queued inside a remote
    /// guardian before an execution token exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_key: Option<String>,
    /// In-process launch gate. Skipped on the wire; control/guardian safety is
    /// represented by the serializable preconditions above.
    #[serde(skip)]
    pub launch_cancelled: Option<Arc<AtomicBool>>,
    /// Deterministic coverage for the automatic PTY-reset path. Never exists
    /// in production builds or on the daemon wire.
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) force_clear_to_run_failure: bool,
}

impl ShellActionOptions {
    pub(crate) fn is_default(&self) -> bool {
        let is_default = !self.compact_output
            && self.expected_generation.is_none()
            && self.expected_execution.is_none()
            && self.expected_guardian_epoch.is_none()
            && !self.require_generation_binding
            && self.cancellation_key.is_none();
        #[cfg(test)]
        {
            is_default && !self.force_clear_to_run_failure
        }
        #[cfg(not(test))]
        {
            is_default
        }
    }

    pub(crate) fn is_launch_cancelled(&self) -> bool {
        self.launch_cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst))
    }
}

impl PartialEq for ShellActionOptions {
    fn eq(&self, other: &Self) -> bool {
        let equal = self.compact_output == other.compact_output
            && self.expected_generation == other.expected_generation
            && self.expected_execution == other.expected_execution
            && self.expected_guardian_epoch == other.expected_guardian_epoch
            && self.require_generation_binding == other.require_generation_binding
            && self.cancellation_key == other.cancellation_key;
        #[cfg(test)]
        {
            equal && self.force_clear_to_run_failure == other.force_clear_to_run_failure
        }
        #[cfg(not(test))]
        {
            equal
        }
    }
}

impl Eq for ShellActionOptions {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShellSessionConfiguration {
    pub attach_hint: Option<String>,
    pub attached_existing: bool,
}

pub type ShellRuntimeConfigureFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ShellSessionConfiguration>> + Send + 'a>>;

/// State transition requested by the Initialize tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSessionTransition {
    FirstCall,
    ModeChange,
    Reset,
    WorkspaceChange,
}

/// Narrow process-agnostic boundary for `BashCommand` execution.
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

    /// Rich adapter path used by MCP orchestration. External runtime
    /// implementations remain source-compatible through this default.
    fn run_action_detailed<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        command: BashCommand,
        _options: ShellActionOptions,
    ) -> ShellRuntimeDetailedFuture<'a> {
        Box::pin(async move {
            self.run_action(bash_state, command).await.map(BashCommandRuntimeResult::legacy)
        })
    }

    fn supports_generation_bound_actions(&self) -> ShellRuntimeBoolFuture<'_> {
        Box::pin(async { Ok(false) })
    }

    /// Session-aware capability probe. Daemon-backed runtimes override this so
    /// the answer comes from the guardian that owns this shell, not merely from
    /// the stable control process in front of it.
    fn supports_generation_bound_actions_for<'a>(
        &'a self,
        _bash_state: &'a Arc<Mutex<Option<BashState>>>,
    ) -> ShellRuntimeBoolFuture<'a> {
        self.supports_generation_bound_actions()
    }

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a>;

    /// Interrupt only the exact command generation owned by a Task. The
    /// conservative default refuses generation-bound interruption.
    fn interrupt_generation<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        expected_generation: Option<u64>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async move {
            if expected_generation.is_some() {
                return Ok(false);
            }
            self.interrupt(bash_state).await?;
            Ok(true)
        })
    }

    /// Interrupt only the complete execution identity. External runtimes keep
    /// working through the generation-only conservative default.
    fn interrupt_execution<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        expected: Option<ShellExecutionToken>,
    ) -> ShellRuntimeBoolFuture<'a> {
        self.interrupt_generation(bash_state, expected.map(|token| token.generation))
    }

    /// Cancel a command that is reserved remotely but has not reached the PTY.
    /// Embedded runtimes use the in-process cancellation flag and need no
    /// additional reservation protocol.
    fn cancel_pending_action<'a>(
        &'a self,
        _bash_state: &'a Arc<Mutex<Option<BashState>>>,
        _cancellation_key: &'a str,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async { Ok(false) })
    }

    /// Release the runtime-owned resources for one logical session.
    fn terminate_session<'a>(&'a self, thread_id: &'a str) -> ShellRuntimeUnitFuture<'a>;
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
            let operation_barrier =
                lock_session_store().operation_barrier(&bash_state.current_thread_id);
            let _operation = operation_barrier.write().await;
            if matches!(
                transition,
                ShellSessionTransition::FirstCall | ShellSessionTransition::Reset
            ) && (transition == ShellSessionTransition::Reset || bash_state.cwd.exists())
            {
                bash_state.init_pty_shell().await?;
            }
            let attach_hint = {
                let shell = bash_state.pty_shell.lock().await;
                shell.as_ref().and_then(|shell| shell.attach_hint.clone())
            };
            Ok(ShellSessionConfiguration { attach_hint, attached_existing: false })
        })
    }

    fn run_action<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> ShellRuntimeFuture<'a> {
        Box::pin(crate::tools::bash_command::handle_embedded_tool_call(bash_state, command))
    }

    fn run_action_detailed<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        command: BashCommand,
        options: ShellActionOptions,
    ) -> ShellRuntimeDetailedFuture<'a> {
        Box::pin(crate::tools::bash_command::handle_embedded_tool_call_detailed(
            bash_state, command, options,
        ))
    }

    fn supports_generation_bound_actions(&self) -> ShellRuntimeBoolFuture<'_> {
        Box::pin(async { Ok(true) })
    }

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(interrupt_embedded(bash_state))
    }

    fn interrupt_generation<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        expected_generation: Option<u64>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(interrupt_embedded_generation(bash_state, expected_generation))
    }

    fn interrupt_execution<'a>(
        &'a self,
        bash_state: &'a Arc<Mutex<Option<BashState>>>,
        expected: Option<ShellExecutionToken>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(interrupt_embedded_execution(bash_state, expected))
    }

    fn terminate_session<'a>(&'a self, thread_id: &'a str) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async move {
            let operation_barrier = lock_session_store().operation_barrier(thread_id);
            let _operation = operation_barrier.write().await;
            let shell = lock_session_store().resolve(thread_id, &ShellTarget::Main);
            if let Some(shell) = shell {
                *shell.lock().await = None;
            }
            Ok(())
        })
    }
}

async fn interrupt_embedded(bash_state: &Arc<Mutex<Option<BashState>>>) -> Result<()> {
    let _ = interrupt_embedded_generation(bash_state, None).await?;
    Ok(())
}

const INTERRUPT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const INTERRUPT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_INTERRUPT_ATTEMPTS: u32 = 3;

/// Interrupt one exact foreground generation and wait until its prompt is
/// visible. PTYs can occasionally deliver Ctrl+C while a short-lived child is
/// handing terminal control back to its parent, so retry the signal while the
/// same generation is still running. The caller retains the session operation
/// barrier, which prevents any retry from reaching a newer command.
pub(crate) async fn interrupt_pty_until_recovered(
    shell: &SharedPtyShell,
    expected_generation: Option<u64>,
) -> Result<bool> {
    let deadline = Instant::now() + INTERRUPT_RECOVERY_TIMEOUT;
    let mut next_interrupt = Instant::now();
    let mut attempts = 0_u32;

    loop {
        let now = Instant::now();
        let recovered = {
            let mut guard = shell.lock().await;
            let Some(pty) = guard.as_mut() else { return Ok(false) };
            if expected_generation.is_some_and(|expected| pty.command_generation() != expected) {
                return Ok(false);
            }

            if pty.poll_output_nonblocking() || !pty.command_running {
                if attempts == 0 {
                    return Ok(false);
                }
                true
            } else {
                if attempts < MAX_INTERRUPT_ATTEMPTS && now >= next_interrupt {
                    pty.send_interrupt().map_err(|error| {
                        WinxError::CommandExecutionError(format!(
                            "failed to interrupt shell: {error}"
                        ))
                    })?;
                    attempts += 1;
                    next_interrupt = Instant::now() + INTERRUPT_RETRY_INTERVAL;
                    if attempts > 1 {
                        tracing::debug!(attempts, "retrying Ctrl+C for active PTY generation");
                    }
                }
                false
            }
        };

        if recovered {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(INTERRUPT_POLL_INTERVAL).await;
    }

    Err(WinxError::CommandExecutionError(
        "interrupted shell did not return to a prompt within 3 seconds".to_string(),
    ))
}

async fn interrupt_embedded_generation(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    expected_generation: Option<u64>,
) -> Result<bool> {
    interrupt_embedded_guarded(bash_state, expected_generation, None).await
}

async fn interrupt_embedded_execution(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    expected: Option<ShellExecutionToken>,
) -> Result<bool> {
    interrupt_embedded_guarded(
        bash_state,
        expected.as_ref().map(|token| token.generation),
        expected.as_ref(),
    )
    .await
}

async fn interrupt_embedded_guarded(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    expected_generation: Option<u64>,
    expected_execution: Option<&ShellExecutionToken>,
) -> Result<bool> {
    // Match configure_session's BashState -> operation barrier lock order and
    // retain the read side from incarnation capture through interruption.
    let (shell, _operation) = {
        let state = bash_state.lock().await;
        let Some(state) = state.as_ref() else { return Ok(false) };
        let operation_barrier = lock_session_store().operation_barrier(&state.current_thread_id);
        let operation = operation_barrier.read_owned().await;
        (state.pty_shell.clone(), operation)
    };
    {
        let guard = shell.lock().await;
        if let Some(pty) = guard.as_ref() {
            if expected_execution.is_some_and(|expected| {
                expected.guardian_epoch != "embedded"
                    || expected.session_epoch != format!("{:016x}", pty.incarnation())
            }) {
                return Ok(false);
            }
        }
    }
    interrupt_pty_until_recovered(&shell, expected_generation).await
}
