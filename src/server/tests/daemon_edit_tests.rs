use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use rmcp::model::CallToolRequestParams;
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::daemon::{DaemonServer, DaemonShellRuntime};
use crate::runtime::{ShellActionOptions, ShellRuntime};
use crate::server::{SessionIsolation, WinxService};
use crate::tool_policy::ToolPolicy;

struct DaemonEditSession {
    workspace: TempDir,
    _socket_dir: TempDir,
    service: WinxService,
    runtime: Arc<DaemonShellRuntime>,
    thread_id: String,
    server_task: tokio::task::JoinHandle<crate::errors::Result<()>>,
}

impl DaemonEditSession {
    async fn new() -> anyhow::Result<Self> {
        let workspace = tempfile::tempdir()?;
        std::fs::write(workspace.path().join("target.txt"), "original\n")?;
        let socket_dir = tempfile::tempdir()?;
        let socket = socket_dir.path().join("winxd.sock");
        let server = DaemonServer::bind(&socket).await?;
        let runtime = Arc::new(DaemonShellRuntime::new(&socket));
        let mut session = Self {
            workspace,
            _socket_dir: socket_dir,
            service: WinxService::with_runtime(SessionIsolation::Lenient, runtime.clone()),
            runtime,
            thread_id: format!("daemon-edit-{:016x}", rand::random::<u64>()),
            server_task: tokio::spawn(server.serve()),
        };
        let initialized = session.initialize().await?;
        assert_eq!(initialized["status"], "completed", "{initialized}");
        session.thread_id = initialized["data"]["thread_id"]
            .as_str()
            .context("Initialize omitted thread_id")?
            .to_string();
        let read = session.call("ReadFiles", json!({"file_paths": [session.target()]})).await?;
        assert_eq!(read["status"], "completed", "{read}");
        Ok(session)
    }

    fn target(&self) -> PathBuf {
        self.workspace.path().join("target.txt")
    }

    async fn call(&self, name: &'static str, mut arguments: Value) -> anyhow::Result<Value> {
        arguments["thread_id"] = json!(self.thread_id);
        arguments["workspace_root"] = json!(self.workspace.path());
        let request = CallToolRequestParams::new(name)
            .with_arguments(arguments.as_object().context("tool arguments")?.clone());
        self.service
            .execute_tool_call(
                request,
                ShellActionOptions::default(),
                ToolPolicy::default().edit_permissions(),
            )
            .await?
            .result
            .structured_content
            .context("tool omitted structured content")
    }

    async fn initialize(&self) -> anyhow::Result<Value> {
        self.call(
            "Initialize",
            json!({
                "type": "first_call",
                "any_workspace_path": self.workspace.path(),
                "mode_name": "wcgw"
            }),
        )
        .await
    }

    async fn edit(&self, verify_command: Option<&str>) -> anyhow::Result<Value> {
        let mut arguments = json!({
            "files": [{"file_path": self.target(), "mode": "replace", "content": "edited\n"}]
        });
        if let Some(command) = verify_command {
            arguments["verify_command"] = json!(command);
            arguments["verify_wait_for_seconds"] = json!(5);
        }
        self.call("EditFiles", arguments).await
    }

    async fn bash(&self, command: &str) -> anyhow::Result<Value> {
        self.call(
            "BashCommand",
            json!({
                "action_json": {"type": "command", "command": command},
                "wait_for_seconds": 5
            }),
        )
        .await
    }

    async fn undo(&self, undo_id: &str) -> anyhow::Result<Value> {
        self.call(
            "EditFiles",
            json!({"files": [{
                "file_path": self.target(),
                "mode": "undo",
                "undo_id": undo_id
            }]}),
        )
        .await
    }

    async fn stop(&self) -> anyhow::Result<()> {
        self.runtime.terminate_session(&self.thread_id).await?;
        Ok(())
    }
}

impl Drop for DaemonEditSession {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

fn undo_id(result: &Value) -> anyhow::Result<&str> {
    result["data"]["undo_ids"][0].as_str().context("edit omitted undo handle")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_edit_without_verification_reports_applied_across_replay_and_undo(
) -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let edited = session.edit(None).await?;
    assert_eq!(edited["status"], "completed", "{edited}");
    assert_eq!(edited["data"]["edit_applied"], true, "{edited}");
    assert_eq!(edited["data"]["mutation_transition"], "committed", "{edited}");
    assert_eq!(std::fs::read_to_string(session.target())?, "edited\n");
    let receipt_id =
        edited["data"]["mutation_receipt_id"].as_str().context("edit omitted mutation receipt")?;
    let undo_handle = undo_id(&edited)?;

    let replay = session.edit(None).await?;
    assert_eq!(replay["status"], "completed", "{replay}");
    assert_eq!(replay["data"]["edit_applied"], true, "{replay}");
    assert_eq!(replay["data"]["mutation_transition"], "replayed", "{replay}");
    assert_eq!(replay["data"]["mutation_replayed"], true, "{replay}");
    assert_eq!(replay["data"]["mutation_receipt_id"], receipt_id, "{replay}");
    assert_eq!(replay["data"]["undo_ids"][0], undo_handle, "{replay}");
    assert_eq!(std::fs::read_to_string(session.target())?, "edited\n");

    let undone = session.undo(undo_handle).await?;
    assert_eq!(undone["status"], "completed", "{undone}");
    assert_eq!(undone["data"]["edit_applied"], true, "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "original\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_command_preserves_undo_and_updates_cwd() -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let edited = session.edit(None).await?;
    assert_eq!(edited["status"], "completed", "{edited}");
    let undo_id = undo_id(&edited)?;
    let child = session.workspace.path().join("child");
    std::fs::create_dir(&child)?;

    let command = session.bash("cd child").await?;
    assert_eq!(command["status"], "completed", "{command}");
    assert_eq!(command["data"]["exit_code"], 0, "{command}");
    // The shell reports the resolved path; macOS temp dirs live behind a
    // `/var -> /private/var` symlink.
    assert_eq!(command["data"]["cwd"], json!(child.canonicalize()?), "{command}");

    let replay = session.edit(None).await?;
    assert_eq!(replay["data"]["mutation_transition"], "replayed", "{replay}");
    assert_eq!(replay["data"]["undo_ids"][0], undo_id, "{replay}");

    let undone = session.undo(undo_id).await?;
    assert_eq!(undone["status"], "completed", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "original\n");
    session.stop().await
}

async fn verified_edit_can_be_undone(command: &str, passed: bool) -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let edited = session.edit(Some(command)).await?;
    assert_eq!(
        edited["status"],
        if passed { "completed" } else { "completed_with_issues" },
        "{edited}"
    );
    assert_eq!(edited["data"]["verification_passed"], passed, "{edited}");
    let undone = session.undo(undo_id(&edited)?).await?;
    assert_eq!(undone["status"], "completed", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "original\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_successful_verification_preserves_undo() -> anyhow::Result<()> {
    verified_edit_can_be_undone("true", true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_failed_verification_preserves_undo() -> anyhow::Result<()> {
    verified_edit_can_be_undone("sh -c 'exit 7'", false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_undo_rejects_content_changed_by_shell() -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let edited = session.edit(None).await?;
    let command = session.bash("printf 'external\\n' > target.txt").await?;
    assert_eq!(command["status"], "completed", "{command}");
    assert_eq!(command["data"]["exit_code"], 0, "{command}");
    let undone = session.undo(undo_id(&edited)?).await?;
    assert_eq!(undone["status"], "conflict", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "external\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reattach_does_not_restore_undo_handles() -> anyhow::Result<()> {
    let mut session = DaemonEditSession::new().await?;
    let edited = session.edit(None).await?;
    session.service = WinxService::with_runtime(SessionIsolation::Lenient, session.runtime.clone());
    let reattached = session.initialize().await?;
    assert_eq!(reattached["status"], "completed", "{reattached}");
    let undone = session.undo(undo_id(&edited)?).await?;
    assert_eq!(undone["status"], "not_found", "{undone}");
    assert_eq!(undone["errorCode"], "undo_expired", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "edited\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_response_preserves_edits_and_reads_made_during_command() -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let late_target = session.workspace.path().join("late.txt");
    std::fs::write(&late_target, "late original\n")?;
    let started = session.workspace.path().join("command-started");
    let release = session.workspace.path().join("command-release");
    let local_work = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        let edited = session.edit(None).await?;
        assert_eq!(edited["status"], "completed", "{edited}");
        let read = session.call("ReadFiles", json!({"file_paths": [late_target]})).await?;
        assert_eq!(read["status"], "completed", "{read}");
        std::fs::write(release, "release")?;
        Ok::<_, anyhow::Error>((edited, read))
    };
    let (command, local) = tokio::join!(
        session.bash(
            "sh -c 'touch command-started; while [ ! -e command-release ]; do sleep 0.01; done'"
        ),
        local_work
    );
    let command = command?;
    assert_eq!(command["status"], "completed", "{command}");
    assert_eq!(command["data"]["exit_code"], 0, "{command}");
    let (edited, read) = local?;
    let late_edit = session
        .call(
            "EditFiles",
            json!({"files": [{
                "file_path": late_target,
                "mode": "line_patch",
                "expected_revision": read["data"]["files"][0]["revision"],
                "patches": [{"start_line": 1, "delete_lines": 1, "replacement": "late edited\n"}]
            }]}),
        )
        .await?;
    assert_eq!(late_edit["status"], "completed", "{late_edit}");
    assert_eq!(std::fs::read_to_string(late_target)?, "late edited\n");
    let undone = session.undo(undo_id(&edited)?).await?;
    assert_eq!(undone["status"], "completed", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "original\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_status_poll_keeps_running_verification_bound_to_its_execution() -> anyhow::Result<()>
{
    let session = DaemonEditSession::new().await?;
    let started = session
        .call(
            "EditFiles",
            json!({
                "files": [{
                    "file_path": session.target(), "mode": "replace", "content": "edited\n"
                }],
                "verify_command": "sh -c 'printf x >> verification-runs; while [ ! -e verification-release ]; do sleep 0.01; done'",
                "verify_wait_for_seconds": 0.01
            }),
        )
        .await?;
    assert_eq!(started["status"], "running", "{started}");
    let verify_arguments = started["nextAction"]["arguments"].clone();
    let polled = session
        .call(
            "BashCommand",
            json!({
                "action_json": {"type": "status_check", "status_check": true},
                "wait_for_seconds": 0.01
            }),
        )
        .await?;
    assert_eq!(polled["status"], "running", "{polled}");
    std::fs::write(session.workspace.path().join("verification-release"), "release")?;

    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let result = session.call("BashCommand", verify_arguments.clone()).await?;
            if result["status"] != "running" {
                break Ok::<_, anyhow::Error>(result);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(completed["status"], "completed", "{completed}");
    assert_eq!(completed["data"]["exit_code"], 0, "{completed}");
    assert_eq!(
        std::fs::read_to_string(session.workspace.path().join("verification-runs"))?,
        "x",
        "polling must not relaunch an already running verification"
    );
    let undone = session.undo(undo_id(&started)?).await?;
    assert_eq!(undone["status"], "completed", "{undone}");
    assert_eq!(std::fs::read_to_string(session.target())?, "original\n");
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_repeated_content_restores_each_distinct_edit_in_order() -> anyhow::Result<()> {
    let session = DaemonEditSession::new().await?;
    let mut ids = Vec::new();
    for content in ["second\n", "original\n", "final\n"] {
        let edited = session
            .call(
                "EditFiles",
                json!({"files": [{
                    "file_path": session.target(), "mode": "replace", "content": content
                }]}),
            )
            .await?;
        assert_eq!(edited["status"], "completed", "{edited}");
        ids.push(undo_id(&edited)?.to_string());
    }

    for (index, expected) in [(2, "original\n"), (1, "second\n"), (0, "original\n")] {
        let undone = session.undo(&ids[index]).await?;
        assert_eq!(undone["status"], "completed", "{undone}");
        assert_eq!(std::fs::read_to_string(session.target())?, expected);
        let next = index.checked_sub(1).map(|previous| ids[previous].as_str());
        assert_eq!(undone["data"]["next_undo_id"].as_str(), next, "{undone}");
    }
    session.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reattach_replays_completed_verification_without_executing_it_again() -> anyhow::Result<()>
{
    let mut session = DaemonEditSession::new().await?;
    let command = "printf x >> verification-runs";
    let edited = session.edit(Some(command)).await?;
    assert_eq!(edited["status"], "completed", "{edited}");
    assert_eq!(std::fs::read_to_string(session.workspace.path().join("verification-runs"))?, "x");

    session.service = WinxService::with_runtime(SessionIsolation::Lenient, session.runtime.clone());
    let reattached = session.initialize().await?;
    assert_eq!(reattached["status"], "completed", "{reattached}");
    let replay = session.edit(Some(command)).await?;
    if replay["nextAction"]["tool"] == "BashCommand" {
        let verified =
            session.call("BashCommand", replay["nextAction"]["arguments"].clone()).await?;
        assert_eq!(verified["status"], "completed", "{verified}");
    }
    assert_eq!(
        std::fs::read_to_string(session.workspace.path().join("verification-runs"))?,
        "x",
        "reattach must preserve the durable completed-verification receipt"
    );
    assert_eq!(replay["status"], "completed", "{replay}");
    assert_eq!(replay["data"]["mutation_transition"], "replayed", "{replay}");
    session.stop().await
}
