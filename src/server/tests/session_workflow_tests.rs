//! Real PTY coverage for shared foreground sessions and independent work.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use rmcp::model::CallToolRequestParams;
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::daemon::{DaemonServer, DaemonShellRuntime};
use crate::runtime::{EmbeddedShellRuntime, ShellActionOptions, ShellRuntime};
use crate::server::{SessionIsolation, WinxService};
use crate::tool_policy::ToolPolicy;

const DEADLINE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
enum RuntimeKind {
    Embedded,
    Daemon,
    StatusError,
    StatusPending,
}

struct UnavailableStatusRuntime {
    inner: EmbeddedShellRuntime,
    pending: bool,
}

impl ShellRuntime for UnavailableStatusRuntime {
    fn configure_session<'a>(
        &'a self,
        state: &'a mut crate::state::BashState,
        transition: crate::runtime::ShellSessionTransition,
    ) -> crate::runtime::ShellRuntimeConfigureFuture<'a> {
        self.inner.configure_session(state, transition)
    }

    fn attach_session<'a>(
        &'a self,
        state: &'a mut crate::state::BashState,
    ) -> crate::runtime::ShellRuntimeConfigureFuture<'a> {
        self.inner.attach_session(state)
    }

    fn run_action<'a>(
        &'a self,
        state: &'a Arc<tokio::sync::Mutex<Option<crate::state::BashState>>>,
        command: crate::types::BashCommand,
    ) -> crate::runtime::ShellRuntimeFuture<'a> {
        self.inner.run_action(state, command)
    }

    fn interrupt<'a>(
        &'a self,
        state: &'a Arc<tokio::sync::Mutex<Option<crate::state::BashState>>>,
    ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
        self.inner.interrupt(state)
    }

    fn terminate_session<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
        self.inner.terminate_session(thread_id)
    }

    fn describe_session<'a>(
        &'a self,
        _state: &'a Arc<tokio::sync::Mutex<Option<crate::state::BashState>>>,
    ) -> crate::runtime::ShellRuntimeStatusFuture<'a> {
        if self.pending {
            Box::pin(std::future::pending())
        } else {
            Box::pin(async {
                Err(crate::errors::WinxError::CommandExecutionError(
                    "injected status observation failure".to_string(),
                ))
            })
        }
    }
}

type ServerTask = tokio::task::JoinHandle<crate::errors::Result<()>>;

struct WorkflowSession {
    _workspace: TempDir,
    root: PathBuf,
    _socket_dir: Option<TempDir>,
    service: WinxService,
    runtime: Arc<dyn ShellRuntime>,
    thread_id: String,
    server_task: Option<ServerTask>,
}

impl WorkflowSession {
    async fn new(kind: RuntimeKind) -> anyhow::Result<Self> {
        let workspace = tempfile::tempdir()?;
        let root = workspace.path().canonicalize()?;
        let (runtime, socket_dir, server_task): (
            Arc<dyn ShellRuntime>,
            Option<TempDir>,
            Option<ServerTask>,
        ) = match kind {
            RuntimeKind::Embedded => (Arc::new(EmbeddedShellRuntime), None, None),
            RuntimeKind::Daemon => {
                let socket_dir = tempfile::tempdir()?;
                let socket = socket_dir.path().join("winxd.sock");
                let server = DaemonServer::bind(&socket).await?;
                (
                    Arc::new(DaemonShellRuntime::new(&socket)),
                    Some(socket_dir),
                    Some(tokio::spawn(server.serve())),
                )
            }
            RuntimeKind::StatusError | RuntimeKind::StatusPending => (
                Arc::new(UnavailableStatusRuntime {
                    inner: EmbeddedShellRuntime,
                    pending: matches!(kind, RuntimeKind::StatusPending),
                }),
                None,
                None,
            ),
        };
        let mut session = Self {
            _workspace: workspace,
            root,
            _socket_dir: socket_dir,
            service: WinxService::with_runtime(SessionIsolation::Lenient, runtime.clone()),
            runtime,
            thread_id: format!("session-workflow-{:016x}", rand::random::<u64>()),
            server_task,
        };
        let initialized = session.initialize(false).await?;
        ensure!(initialized["status"] == "completed", "{initialized}");
        session.thread_id = initialized["data"]["thread_id"]
            .as_str()
            .context("Initialize omitted thread_id")?
            .to_string();
        Ok(session)
    }

    async fn call(&self, name: &'static str, mut arguments: Value) -> anyhow::Result<Value> {
        arguments["thread_id"] = json!(self.thread_id);
        arguments["workspace_root"] = json!(self.root);
        self.service
            .execute_tool_call(
                CallToolRequestParams::new(name)
                    .with_arguments(arguments.as_object().context("tool arguments")?.clone()),
                ShellActionOptions::default(),
                ToolPolicy::default().edit_permissions(),
            )
            .await?
            .result
            .structured_content
            .context("tool omitted structured content")
    }

    async fn initialize(&self, with_initial_file: bool) -> anyhow::Result<Value> {
        let mut arguments = json!({
            "type": "first_call",
            "any_workspace_path": self.root,
            "mode_name": "wcgw"
        });
        if with_initial_file {
            arguments["initial_files_to_read"] = json!([self.root.join("foreground.ready")]);
        }
        self.call("Initialize", arguments).await
    }

    async fn command(&self, command: &str, background: bool) -> anyhow::Result<Value> {
        self.call(
            "BashCommand",
            json!({
                "action_json": {
                    "type": "command", "command": command,
                    "is_background": background, "allow_multi": true
                },
                "wait_for_seconds": 0.05
            }),
        )
        .await
    }

    async fn status(&self, background_id: Option<&str>) -> anyhow::Result<Value> {
        self.call(
            "BashCommand",
            json!({
                "action_json": {
                    "type": "status_check", "status_check": true,
                    "bg_command_id": background_id
                },
                "wait_for_seconds": 0.05
            }),
        )
        .await
    }

    async fn completed(&self, background_id: Option<&str>) -> anyhow::Result<Value> {
        tokio::time::timeout(DEADLINE, async {
            loop {
                let result = self.status(background_id).await?;
                if result["status"] != "running" {
                    return Ok(result);
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("command did not complete after its barrier was released")?
    }

    fn release_foreground(&self) -> std::io::Result<()> {
        std::fs::write(self.root.join("foreground.release"), [])
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        // Keep cleanup running even if the body returned an error (including RED).
        let released = self.release_foreground();
        let terminated = self.runtime.terminate_session(&self.thread_id).await;
        let (slot, _) = self.service.session_for(&self.thread_id).await;
        slot.lock().await.take();
        if let Some(task) = self.server_task.take() {
            task.abort();
            let _ = task.await;
        }
        released?;
        terminated?;
        Ok(())
    }
}

impl Drop for WorkflowSession {
    fn drop(&mut self) {
        // Fallback for a panic/cancellation; ordinary failures take `close` above.
        let _ = self.release_foreground();
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
    }
}

async fn wait_for_file(path: &Path) -> anyhow::Result<()> {
    tokio::time::timeout(DEADLINE, async {
        while !path.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .with_context(|| format!("command did not create {}", path.display()))
}

async fn assert_attached_workflow(session: &WorkflowSession) -> anyhow::Result<()> {
    let started = session
        .command(
            "sh -c 'printf ready > foreground.ready; while [ ! -e foreground.release ]; do sleep 0.01; done; printf completed > foreground.done'",
            false,
        )
        .await?;
    ensure!(started["status"] == "running", "{started}");
    let generation = started["data"]["command_generation"]
        .as_u64()
        .context("foreground omitted command_generation")?;
    wait_for_file(&session.root.join("foreground.ready")).await?;

    // Both attach response forms must report the same busy session. The barrier
    // keeps the command alive; the timeout detects a lock held across the probe.
    for with_initial_file in [false, true] {
        let attached = tokio::time::timeout(DEADLINE, session.initialize(with_initial_file))
            .await
            .context("Initialize waited for the busy foreground command")??;
        ensure!(attached["status"] == "completed", "{attached}");
        ensure!(attached["data"]["initialize_transition"] == "attached_existing", "{attached}");
        ensure!(attached["data"]["session_status"]["command_running"] == true, "{attached}");
        ensure!(attached["data"]["shell_reset_performed"] == false, "{attached}");
        ensure!(!session.root.join("foreground.done").exists());
    }

    let existing = session.status(None).await?;
    ensure!(existing["status"] == "running", "{existing}");
    ensure!(existing["data"]["command_generation"] == generation, "{existing}");

    let independent = "printf x >> independent.marker";
    let blocked = session.command(independent, false).await?;
    ensure!(blocked["errorCode"] == "command_already_running", "{blocked}");
    ensure!(blocked["data"]["command_started"] == false, "{blocked}");
    ensure!(!session.root.join("independent.marker").exists());

    let background = session.command(independent, true).await?;
    let background_id =
        background["data"]["bg_command_id"].as_str().context("background omitted bg_command_id")?;
    let execution_id =
        background["data"]["execution_id"].as_str().context("background omitted execution_id")?;
    let background_generation = background["data"]["command_generation"]
        .as_u64()
        .context("background omitted command_generation")?;
    ensure!(background["data"]["history_recorded"] == true, "{background}");
    let completed = session.completed(Some(background_id)).await?;
    ensure!(completed["status"] == "completed", "{completed}");
    ensure!(completed["data"]["exit_code"] == 0, "{completed}");
    ensure!(completed["data"]["execution_id"] == execution_id, "{completed}");
    ensure!(completed["data"]["command_generation"] == background_generation, "{completed}");
    ensure!(completed["data"]["history_recorded"] == true, "{completed}");
    ensure!(std::fs::read_to_string(session.root.join("independent.marker"))? == "x");
    ensure!(!session.root.join("foreground.done").exists());

    let existing = session.status(None).await?;
    ensure!(existing["status"] == "running", "{existing}");
    ensure!(existing["data"]["command_generation"] == generation, "{existing}");
    session.release_foreground()?;
    let finished = session.completed(None).await?;
    ensure!(finished["status"] == "completed", "{finished}");
    ensure!(finished["data"]["exit_code"] == 0, "{finished}");
    ensure!(finished["data"]["command_generation"] == generation, "{finished}");
    ensure!(std::fs::read_to_string(session.root.join("foreground.done"))? == "completed");
    let idle = session.initialize(false).await?;
    ensure!(idle["data"]["session_status"]["command_running"] == false, "{idle}");
    Ok(())
}

async fn run_workflow(kind: RuntimeKind) -> anyhow::Result<()> {
    let mut session = WorkflowSession::new(kind).await?;
    let result = assert_attached_workflow(&session).await;
    let cleanup = session.close().await;
    result?;
    cleanup
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_attach_observes_busy_shell_without_interrupting_work() -> anyhow::Result<()> {
    run_workflow(RuntimeKind::Embedded).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_attach_observes_busy_shell_without_interrupting_work() -> anyhow::Result<()> {
    run_workflow(RuntimeKind::Daemon).await
}

async fn assert_unknown_status_preserves_busy_session(
    session: &WorkflowSession,
) -> anyhow::Result<()> {
    let started = session
        .command(
            "sh -c 'printf ready > foreground.ready; while [ ! -e foreground.release ]; do sleep 0.01; done; printf completed > foreground.done'",
            false,
        )
        .await?;
    ensure!(started["status"] == "running", "{started}");
    wait_for_file(&session.root.join("foreground.ready")).await?;

    let attached = tokio::time::timeout(DEADLINE, session.initialize(false))
        .await
        .context("an unavailable status probe blocked Initialize")??;
    ensure!(attached["status"] == "completed", "{attached}");
    ensure!(attached["data"]["initialize_transition"] == "attached_existing", "{attached}");
    ensure!(
        attached["data"]["session_status"].is_null(),
        "an unavailable observation must remain unknown, not report an idle shell: {attached}"
    );
    ensure!(attached["data"]["shell_reset_performed"] == false, "{attached}");
    ensure!(
        !session.root.join("foreground.done").exists(),
        "the original foreground must still be waiting at its barrier"
    );

    session.release_foreground()?;
    let completed = session.completed(None).await?;
    ensure!(completed["status"] == "completed", "{completed}");
    ensure!(completed["data"]["exit_code"] == 0, "{completed}");
    ensure!(
        std::fs::read_to_string(session.root.join("foreground.done"))? == "completed",
        "the original command must survive the failed status observation"
    );
    Ok(())
}

async fn run_unknown_status_case(kind: RuntimeKind) -> anyhow::Result<()> {
    let mut session = WorkflowSession::new(kind).await?;
    let result = assert_unknown_status_preserves_busy_session(&session).await;
    let cleanup = session.close().await;
    result?;
    cleanup
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_session_survives_status_observation_error() -> anyhow::Result<()> {
    run_unknown_status_case(RuntimeKind::StatusError).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_session_survives_status_observation_timeout() -> anyhow::Result<()> {
    run_unknown_status_case(RuntimeKind::StatusPending).await
}
