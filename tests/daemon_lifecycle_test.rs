#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::daemon::{DaemonClient, DaemonShellRuntime, SessionInfo};
use winx_code_agent::runtime::{restart_control_daemon_at, ShellActionOptions, ShellRuntime};
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

const TEST_NAME: &str = "daemon_session_survives_sigkill_of_adapter";
const THREAD_ID: &str = "daemon-lifecycle-e2e";

fn daemon_command(socket: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_winxd"));
    command
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

fn spawn_daemon(socket: &Path) -> anyhow::Result<std::process::Child> {
    Ok(daemon_command(socket).spawn()?)
}

fn spawn_daemon_with_limits(
    socket: &Path,
    max_guardians: usize,
    idle_ttl_secs: u64,
    unused_idle_ttl_secs: u64,
) -> anyhow::Result<std::process::Child> {
    Ok(daemon_command(socket)
        .env("WINX_MAX_GUARDIANS", max_guardians.to_string())
        .env("WINX_SESSION_IDLE_TTL_SECS", idle_ttl_secs.to_string())
        .env("WINX_UNUSED_SESSION_IDLE_TTL_SECS", unused_idle_ttl_secs.to_string())
        .env("WINX_GUARDIAN_SWEEP_INTERVAL_SECS", "60")
        .spawn()?)
}

fn command(thread_id: &str, script: String, wait: f32) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::Command {
            command: script,
            is_background: false,
            allow_multi: true,
        },
        wait_for_seconds: Some(wait),
        thread_id: thread_id.to_string(),
    }
}

async fn run_adapter_child(socket: &Path, workspace: &Path, ready: &Path) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(None::<BashState>));
    let runtime = DaemonShellRuntime::new(socket);
    tools::initialize::handle_tool_call_with_runtime(
        &runtime,
        &state,
        Initialize {
            init_type: InitializeType::FirstCall,
            mode_name: ModeName::Wcgw,
            any_workspace_path: workspace.to_string_lossy().into_owned(),
            thread_id: THREAD_ID.to_string(),
            code_writer_config: None,
            initial_files_to_read: vec![],
            task_id_to_resume: String::new(),
        },
    )
    .await?;

    let nested = workspace.join("nested");
    std::fs::create_dir_all(&nested)?;
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        command(THREAD_ID, format!("cd {}", nested.display()), 2.0),
    )
    .await?;
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        command(
            THREAD_ID,
            "printf 'adapter-before\\n'; sleep 1; printf 'adapter-after\\n'; sleep 30".to_string(),
            0.0,
        ),
    )
    .await?;

    let info = DaemonClient::new(socket).session_info(THREAD_ID).await?;
    std::fs::write(ready, serde_json::to_vec(&info)?)?;
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn wait_for_file(path: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for {}", path.display())
}

async fn wait_for_daemon(client: &DaemonClient) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if client.hello().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for winxd")
}

async fn wait_for_guardian_socket(runtime_dir: &Path) -> anyhow::Result<std::path::PathBuf> {
    let guardian_dir = runtime_dir.join("guardians");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut entries) = tokio::fs::read_dir(&guardian_dir).await {
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "sock") {
                    return Ok(path);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for a guardian socket")
}

async fn wait_for_process_exit(pid: u32) -> anyhow::Result<()> {
    let pid = i32::try_from(pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // SAFETY: signal 0 only checks existence and the pid came from the
        // authenticated same-UID guardian handshake.
        let result = unsafe { libc::kill(pid, 0) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("guardian process {pid} did not exit")
}

fn terminate_daemon_pid(pid: u32) -> anyhow::Result<()> {
    let pid = i32::try_from(pid)?;
    // SAFETY: the pid came from the authenticated same-UID daemon handshake.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.into())
    }
}

async fn initialize_daemon_session(
    socket: &Path,
    workspace: &Path,
    thread_id: &str,
) -> anyhow::Result<Arc<Mutex<Option<BashState>>>> {
    let state = Arc::new(Mutex::new(None::<BashState>));
    let runtime = DaemonShellRuntime::new(socket);
    tools::initialize::handle_tool_call_with_runtime(
        &runtime,
        &state,
        Initialize {
            init_type: InitializeType::FirstCall,
            mode_name: ModeName::Wcgw,
            any_workspace_path: workspace.to_string_lossy().into_owned(),
            thread_id: thread_id.to_string(),
            code_writer_config: None,
            initial_files_to_read: vec![],
            task_id_to_resume: String::new(),
        },
    )
    .await?;
    Ok(state)
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_session_survives_sigkill_of_adapter() -> anyhow::Result<()> {
    if std::env::var_os("WINX_E2E_ADAPTER_CHILD").is_some() {
        let socket = std::env::var_os("WINX_E2E_SOCKET")
            .ok_or_else(|| anyhow::anyhow!("child socket env is missing"))?;
        let workspace = std::env::var_os("WINX_E2E_WORKSPACE")
            .ok_or_else(|| anyhow::anyhow!("child workspace env is missing"))?;
        let ready = std::env::var_os("WINX_E2E_READY")
            .ok_or_else(|| anyhow::anyhow!("child ready env is missing"))?;
        return run_adapter_child(Path::new(&socket), Path::new(&workspace), Path::new(&ready))
            .await;
    }

    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let ready = runtime_dir.path().join("adapter-ready.json");
    let mut daemon = spawn_daemon(&socket)?;

    let client = DaemonClient::new(&socket);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if client.list_sessions().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let mut adapter = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(TEST_NAME)
        .arg("--nocapture")
        .env("WINX_E2E_ADAPTER_CHILD", "1")
        .env("WINX_E2E_SOCKET", &socket)
        .env("WINX_E2E_WORKSPACE", workspace.path())
        .env("WINX_E2E_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;

    wait_for_file(&ready, Duration::from_secs(10)).await?;
    let before: SessionInfo = serde_json::from_slice(&std::fs::read(&ready)?)?;
    adapter.kill()?;
    let _ = adapter.wait()?;
    daemon.kill()?;
    let _ = daemon.wait()?;
    daemon = spawn_daemon(&socket)?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if client.list_sessions().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        let mut captured = String::new();
        let mut gap = false;
        loop {
            let output = client.read_output(THREAD_ID, "adapter-b").await?;
            captured.push_str(&output.output);
            gap |= output.gap;
            if captured.contains("adapter-after") {
                return winx_code_agent::Result::Ok(winx_code_agent::daemon::JournalRead {
                    output: captured,
                    next_seq: output.next_seq,
                    gap,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let after = client.session_info(THREAD_ID).await?;

    let _ = client.kill_session(THREAD_ID).await;
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(before.shell_pid.is_some(), "{before:?}");
    assert!(before.command_id.is_some(), "{before:?}");
    assert_eq!(before.shell_pid, after.shell_pid);
    assert_eq!(before.command_id, after.command_id);
    assert_eq!(
        Path::new(&after.cwd).canonicalize()?,
        workspace.path().join("nested").canonicalize()?
    );
    assert!(recovered.output.contains("adapter-before"), "{recovered:?}");
    assert!(recovered.output.contains("adapter-after"), "{recovered:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn planned_control_restart_preserves_guardian_shell() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut original_daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "restart_guardian";
    let state = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let runtime = DaemonShellRuntime::new(&socket);
    let before_output = tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        command(thread_id, "printf 'before-restart\\n'".to_string(), 2.0),
    )
    .await?;
    let before = client.session_info(thread_id).await?;

    let restarted =
        restart_control_daemon_at(&socket, Path::new(env!("CARGO_BIN_EXE_winxd"))).await?;
    let _ = original_daemon.wait()?;
    let after = DaemonClient::new(&socket).session_info(thread_id).await?;
    let after_output = tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        command(thread_id, "printf 'after-restart\\n'".to_string(), 2.0),
    )
    .await?;

    let cleanup = DaemonClient::new(&socket).kill_session(thread_id).await;
    terminate_daemon_pid(restarted.daemon_pid)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(cleanup?, "guardian session should exist during cleanup");
    assert!(before_output.contains("before-restart"), "{before_output}");
    assert!(after_output.contains("after-restart"), "{after_output}");
    assert_eq!(before.shell_pid, after.shell_pid, "guardian PTY owner changed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn stable_control_rejects_stale_action_after_external_guardian_recreate() -> anyhow::Result<()>
{
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "external_guardian_recreate";
    let state = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let runtime = DaemonShellRuntime::new(&socket);
    let before = runtime
        .run_action_detailed(
            &state,
            command(thread_id, "printf before-recreate".to_string(), 2.0),
            ShellActionOptions::default(),
        )
        .await?;
    assert!(before.result.output.contains("before-recreate"), "{before:?}");

    let guardian_socket = wait_for_guardian_socket(runtime_dir.path()).await?;
    let old_guardian = DaemonClient::new(&guardian_socket).hello().await?;
    terminate_daemon_pid(old_guardian.daemon_pid)?;
    wait_for_process_exit(old_guardian.daemon_pid).await?;

    let mut replacement = Command::new(env!("CARGO_BIN_EXE_winx-guardian"))
        .arg("--socket")
        .arg(&guardian_socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let replacement_client = DaemonClient::new(&guardian_socket);
    wait_for_daemon(&replacement_client).await?;
    let new_guardian = replacement_client.hello().await?;
    assert_ne!(old_guardian.daemon_epoch, new_guardian.daemon_epoch);

    let marker = workspace.path().join("stale-action.marker");
    let stale_result = runtime
        .run_action_detailed(
            &state,
            command(thread_id, format!("touch {}", marker.display()), 2.0),
            ShellActionOptions {
                require_generation_binding: true,
                ..ShellActionOptions::default()
            },
        )
        .await;
    let Err(stale_error) = stale_result else {
        anyhow::bail!("the cached guardian epoch reached the replacement guardian")
    };
    assert!(stale_error.to_string().contains("guardian epoch changed"), "{stale_error}");
    assert!(!marker.exists(), "a stale action reached the replacement guardian");

    let recovered = runtime
        .run_action_detailed(
            &state,
            command(thread_id, "printf recovered-recreate".to_string(), 2.0),
            ShellActionOptions::default(),
        )
        .await?;
    assert!(recovered.result.output.contains("recovered-recreate"), "{recovered:?}");

    replacement.kill()?;
    let _ = replacement.wait()?;
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_reclaims_unused_guardian_under_quota_pressure() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 1, 86_400, 86_400)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let _first = initialize_daemon_session(&socket, workspace.path(), "quota_first").await?;
    let before = client.session_info("quota_first").await?;
    assert!(!before.ever_ran_command, "{before:?}");

    let _second = initialize_daemon_session(&socket, workspace.path(), "quota_second").await?;
    assert!(
        client.session_info("quota_first").await.is_err(),
        "the unused guardian should have been reclaimed"
    );
    let second = client.session_info("quota_second").await?;
    assert!(!second.ever_ran_command, "{second:?}");
    assert!(client.kill_session("quota_second").await?);

    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_never_reclaims_an_active_guardian_for_quota() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 1, 86_400, 86_400)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let active = initialize_daemon_session(&socket, workspace.path(), "quota_active").await?;
    let runtime = DaemonShellRuntime::new(&socket);
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &active,
        command("quota_active", "sleep 30".to_string(), 0.0),
    )
    .await?;

    let second_state = Arc::new(Mutex::new(None::<BashState>));
    let second = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: workspace.path().to_string_lossy().into_owned(),
        thread_id: "quota_blocked".to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    let Err(error) =
        tools::initialize::handle_tool_call_with_runtime(&runtime, &second_state, second).await
    else {
        anyhow::bail!("an active guardian unexpectedly lost the only quota slot");
    };
    assert!(error.to_string().contains("guardian limit reached"), "{error}");
    assert!(error.to_string().contains("prune --idle-seconds 0"), "{error}");
    assert!(client.session_info("quota_active").await?.running);

    client.interrupt_session("quota_active").await?;
    assert!(client.kill_session("quota_active").await?);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn default_prune_uses_short_ttl_for_never_used_sessions() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 4, 86_400, 1)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "unused_ttl";
    let _state = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let pruned = client.prune_sessions(None).await?;
    assert_eq!(pruned.removed_thread_ids, vec![thread_id]);
    assert!(client.list_sessions().await?.is_empty());

    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn guardian_activity_clock_advances_after_a_command() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "activity_clock";
    let state = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let before = client.session_info(thread_id).await?;
    assert!(before.created_at_unix_ms.is_some(), "{before:?}");
    assert!(before.last_activity_unix_ms.is_some(), "{before:?}");
    assert!(before.last_command_at_unix_ms.is_none(), "{before:?}");
    assert!(!before.ever_ran_command, "{before:?}");

    tokio::time::sleep(Duration::from_millis(10)).await;
    let runtime = DaemonShellRuntime::new(&socket);
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        command(thread_id, "printf activity-clock".to_string(), 2.0),
    )
    .await?;
    let after = client.session_info(thread_id).await?;
    assert!(after.ever_ran_command, "{after:?}");
    assert!(after.last_command_at_unix_ms.is_some(), "{after:?}");
    assert!(after.last_activity_unix_ms >= before.last_activity_unix_ms, "{before:?} {after:?}");

    assert!(client.kill_session(thread_id).await?);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_first_call_attaches_without_resetting_the_guardian_pty() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let nested = workspace.path().join("nested");
    std::fs::create_dir_all(&nested)?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "attach_or_create";
    let first = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let runtime = DaemonShellRuntime::new(&socket);
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &first,
        command(thread_id, format!("cd {}", nested.display()), 2.0),
    )
    .await?;
    let before = client.session_info(thread_id).await?;

    // Keep one action RPC and the guardian operation barrier occupied. A
    // reconnecting adapter must still attach immediately on an independent
    // channel instead of waiting for the foreground command to finish.
    let marker = workspace.path().join("active-command.marker");
    let active_runtime = runtime.clone();
    let active_state = Arc::clone(&first);
    let active = tokio::spawn(async move {
        tools::bash_command::handle_tool_call_with_runtime(
            &active_runtime,
            &active_state,
            command(thread_id, format!("touch {}; sleep 2", marker.display()), 3.0),
        )
        .await
    });
    let marker = workspace.path().join("active-command.marker");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("foreground command did not reach the PTY"))?;

    let local_response = tokio::time::timeout(
        Duration::from_secs(1),
        tools::initialize::handle_tool_call_with_runtime(
            &runtime,
            &first,
            Initialize {
                init_type: InitializeType::FirstCall,
                mode_name: ModeName::Wcgw,
                any_workspace_path: workspace.path().to_string_lossy().into_owned(),
                thread_id: thread_id.to_string(),
                code_writer_config: None,
                initial_files_to_read: vec![],
                task_id_to_resume: String::new(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("local reattach waited for the foreground command"))??;
    assert!(local_response.contains("Attached to the existing durable Winx session"));

    let second = Arc::new(Mutex::new(None::<BashState>));
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        tools::initialize::handle_tool_call_with_runtime(
            &runtime,
            &second,
            Initialize {
                init_type: InitializeType::FirstCall,
                mode_name: ModeName::Wcgw,
                any_workspace_path: workspace.path().to_string_lossy().into_owned(),
                thread_id: thread_id.to_string(),
                code_writer_config: None,
                initial_files_to_read: vec![],
                task_id_to_resume: String::new(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("fresh reattach waited for the foreground command"))??;
    active.await??;
    let after = client.session_info(thread_id).await?;
    assert!(response.contains("Attached to the existing durable Winx session"), "{response}");
    assert!(response.contains("Context and instructions are unchanged"), "{response}");
    assert!(!response.contains("# Workspace structure"), "{response}");
    assert!(!response.contains("# Winx orchestration contract"), "{response}");
    assert_eq!(before.shell_pid, after.shell_pid, "reattach replaced the PTY owner");
    assert_eq!(Path::new(&after.cwd).canonicalize()?, nested.canonicalize()?);
    // The guardian snapshot reports the shell's logical cwd; on macOS the
    // tempdir sits behind the /var -> /private/var symlink, so compare
    // canonical forms on both sides.
    let attached_cwd =
        second.lock().await.as_ref().map(|state| state.cwd.canonicalize()).transpose()?;
    assert_eq!(attached_cwd, Some(nested.canonicalize()?));

    assert!(client.kill_session(thread_id).await?);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One complete four-chat concurrency schedule is intentional.
async fn four_parallel_chats_remain_fast_and_project_coherent() -> anyhow::Result<()> {
    let workspaces = TempDir::new()?;
    let project_a = workspaces.path().join("project-a");
    let project_b = workspaces.path().join("project-b");
    let project_c = workspaces.path().join("project-c");
    let project_d = workspaces.path().join("project-d");
    for workspace in [&project_a, &project_b, &project_c, &project_d] {
        std::fs::create_dir_all(workspace)?;
    }

    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    // Model four ChatGPT conversations arriving together, each bound to a
    // different canonical project. They share one control daemon but must get
    // independent guardians and PTYs.
    let (chat_a, chat_b, chat_c, chat_d) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            initialize_daemon_session(&socket, &project_a, "parallel_chat_a"),
            initialize_daemon_session(&socket, &project_b, "parallel_chat_b"),
            initialize_daemon_session(&socket, &project_c, "parallel_chat_c"),
            initialize_daemon_session(&socket, &project_d, "parallel_chat_d"),
        )
    })
    .await
    .map_err(|_| anyhow::anyhow!("four concurrent chats did not initialize promptly"))??;

    let initial = tokio::try_join!(
        client.session_info("parallel_chat_a"),
        client.session_info("parallel_chat_b"),
        client.session_info("parallel_chat_c"),
        client.session_info("parallel_chat_d"),
    )?;
    let mut shell_pids =
        vec![initial.0.shell_pid, initial.1.shell_pid, initial.2.shell_pid, initial.3.shell_pid];
    shell_pids.sort_unstable();
    shell_pids.dedup();
    assert_eq!(shell_pids.len(), 4, "parallel projects shared a PTY: {initial:?}");

    let runtime = DaemonShellRuntime::new(&socket);
    let busy_marker = project_a.join("busy.marker");
    let active_runtime = runtime.clone();
    let active_state = Arc::clone(&chat_a);
    let active_marker = busy_marker.clone();
    let active = tokio::spawn(async move {
        tools::bash_command::handle_tool_call_with_runtime(
            &active_runtime,
            &active_state,
            command(
                "parallel_chat_a",
                format!(
                    "touch {}; printf 'chat-a-start\\n'; sleep 4; printf 'chat-a-done\\n'",
                    active_marker.display()
                ),
                5.0,
            ),
        )
        .await
    });
    wait_for_file(&busy_marker, Duration::from_secs(2)).await?;

    // A reconnect on the busy chat and useful work in the other three chats
    // must not queue behind chat A's four-second foreground command.
    let reattached_a = Arc::new(Mutex::new(None::<BashState>));
    let (attach_output, output_b, output_c, output_d) =
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::try_join!(
                tools::initialize::handle_tool_call_with_runtime(
                    &runtime,
                    &reattached_a,
                    Initialize {
                        init_type: InitializeType::FirstCall,
                        mode_name: ModeName::Wcgw,
                        any_workspace_path: project_a.to_string_lossy().into_owned(),
                        thread_id: "parallel_chat_a".to_string(),
                        code_writer_config: None,
                        initial_files_to_read: vec![],
                        task_id_to_resume: String::new(),
                    },
                ),
                tools::bash_command::handle_tool_call_with_runtime(
                    &runtime,
                    &chat_b,
                    command("parallel_chat_b", "printf 'chat-b-only\\n'".to_string(), 1.0),
                ),
                tools::bash_command::handle_tool_call_with_runtime(
                    &runtime,
                    &chat_c,
                    command("parallel_chat_c", "printf 'chat-c-only\\n'".to_string(), 1.0),
                ),
                tools::bash_command::handle_tool_call_with_runtime(
                    &runtime,
                    &chat_d,
                    command("parallel_chat_d", "printf 'chat-d-only\\n'".to_string(), 1.0),
                ),
            )
        })
        .await
        .map_err(|_| anyhow::anyhow!("one busy chat head-of-line blocked another project"))??;

    assert!(attach_output.contains("Attached to the existing durable Winx session"));
    for (output, own, foreign) in [
        (&output_b, "chat-b-only", ["chat-c-only", "chat-d-only"]),
        (&output_c, "chat-c-only", ["chat-b-only", "chat-d-only"]),
        (&output_d, "chat-d-only", ["chat-b-only", "chat-c-only"]),
    ] {
        assert!(output.contains(own), "missing {own}: {output}");
        assert!(
            foreign.into_iter().all(|marker| !output.contains(marker)),
            "cross-chat output leaked into {own}: {output}"
        );
    }

    let while_busy = tokio::try_join!(
        client.session_info("parallel_chat_a"),
        client.session_info("parallel_chat_b"),
        client.session_info("parallel_chat_c"),
        client.session_info("parallel_chat_d"),
    )?;
    assert!(while_busy.0.running, "chat A stopped being busy too early: {while_busy:?}");
    for (before, after, expected_workspace) in [
        (&initial.0, &while_busy.0, &project_a),
        (&initial.1, &while_busy.1, &project_b),
        (&initial.2, &while_busy.2, &project_c),
        (&initial.3, &while_busy.3, &project_d),
    ] {
        assert_eq!(before.shell_pid, after.shell_pid, "chat changed guardian: {after:?}");
        assert_eq!(Path::new(&after.cwd).canonicalize()?, expected_workspace.canonicalize()?);
    }

    let active_output = active.await??;
    assert!(active_output.contains("chat-a-start"), "{active_output}");
    assert!(active_output.contains("chat-a-done"), "{active_output}");
    assert!(!active_output.contains("chat-b-only"), "{active_output}");

    for thread_id in ["parallel_chat_a", "parallel_chat_b", "parallel_chat_c", "parallel_chat_d"] {
        assert!(client.kill_session(thread_id).await?);
    }
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_workspace_chat_gets_fast_busy_result_instead_of_queuing() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon(&socket)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "shared_workspace_chat";
    let first_chat = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let second_chat = Arc::new(Mutex::new(None::<BashState>));
    let runtime = DaemonShellRuntime::new(&socket);
    tools::initialize::handle_tool_call_with_runtime(
        &runtime,
        &second_chat,
        Initialize {
            init_type: InitializeType::FirstCall,
            mode_name: ModeName::Wcgw,
            any_workspace_path: workspace.path().to_string_lossy().into_owned(),
            thread_id: thread_id.to_string(),
            code_writer_config: None,
            initial_files_to_read: vec![],
            task_id_to_resume: String::new(),
        },
    )
    .await?;

    let busy_marker = workspace.path().join("shared-busy.marker");
    let forbidden_marker = workspace.path().join("queued-command-must-not-run.marker");
    let active_runtime = runtime.clone();
    let active_state = Arc::clone(&first_chat);
    let active_marker = busy_marker.clone();
    let active = tokio::spawn(async move {
        tools::bash_command::handle_tool_call_with_runtime(
            &active_runtime,
            &active_state,
            command(thread_id, format!("touch {}; sleep 4", active_marker.display()), 5.0),
        )
        .await
    });
    wait_for_file(&busy_marker, Duration::from_secs(2)).await?;

    let rejected = tokio::time::timeout(
        Duration::from_secs(1),
        tools::bash_command::handle_tool_call_with_runtime(
            &runtime,
            &second_chat,
            command(thread_id, format!("touch {}", forbidden_marker.display()), 1.0),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("shared-session command queued behind the active chat"))?;
    let rejected = match rejected {
        Err(error) => error,
        Ok(output) => anyhow::bail!("second foreground command unexpectedly ran: {output}"),
    };
    assert!(
        matches!(rejected, winx_code_agent::errors::WinxError::CommandAlreadyRunning { .. }),
        "unexpected admission result: {rejected}"
    );
    assert!(!forbidden_marker.exists(), "rejected command was launched later");

    active.await??;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!forbidden_marker.exists(), "rejected command remained queued in the guardian");

    assert!(client.kill_session(thread_id).await?);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_prune_removes_idle_but_preserves_active_sessions() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 4, 86_400, 86_400)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let _idle = initialize_daemon_session(&socket, workspace.path(), "prune_idle").await?;
    let pruned = client.prune_sessions(Some(0)).await?;
    assert_eq!(pruned.removed_thread_ids, vec!["prune_idle"]);
    assert!(client.list_sessions().await?.is_empty());

    let active = initialize_daemon_session(&socket, workspace.path(), "prune_active").await?;
    let runtime = DaemonShellRuntime::new(&socket);
    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &active,
        command("prune_active", "sleep 5".to_string(), 0.0),
    )
    .await?;
    let pruned = client.prune_sessions(Some(0)).await?;
    assert!(pruned.removed_thread_ids.is_empty(), "{pruned:?}");
    assert_eq!(pruned.skipped_active_thread_ids, vec!["prune_active"]);

    client.interrupt_session("prune_active").await?;
    assert!(client.kill_session("prune_active").await?);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}
