#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::daemon::{DaemonClient, DaemonShellRuntime, SessionInfo};
use winx_code_agent::runtime::restart_control_daemon_at;
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
) -> anyhow::Result<std::process::Child> {
    Ok(daemon_command(socket)
        .env("WINX_MAX_GUARDIANS", max_guardians.to_string())
        .env("WINX_SESSION_IDLE_TTL_SECS", idle_ttl_secs.to_string())
        .env("WINX_GUARDIAN_SWEEP_INTERVAL_SECS", "1")
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
        let mut next_seq = 0;
        let mut gap = false;
        loop {
            let output = client.read_output(THREAD_ID, "adapter-b").await?;
            captured.push_str(&output.output);
            next_seq = output.next_seq;
            gap |= output.gap;
            if captured.contains("adapter-after") {
                return winx_code_agent::Result::Ok(winx_code_agent::daemon::JournalRead {
                    output: captured,
                    next_seq,
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
async fn daemon_enforces_guardian_quota_and_releases_capacity() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 1, 86_400)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let _first = initialize_daemon_session(&socket, workspace.path(), "quota_first").await?;
    let second_state = Arc::new(Mutex::new(None::<BashState>));
    let runtime = DaemonShellRuntime::new(&socket);
    let second = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: workspace.path().to_string_lossy().into_owned(),
        thread_id: "quota_second".to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    let Err(error) =
        tools::initialize::handle_tool_call_with_runtime(&runtime, &second_state, second.clone())
            .await
    else {
        anyhow::bail!("second guardian unexpectedly exceeded the configured quota");
    };
    assert!(error.to_string().contains("guardian limit reached"), "{error}");

    assert!(client.kill_session("quota_first").await?);
    tools::initialize::handle_tool_call_with_runtime(&runtime, &second_state, second).await?;
    assert!(client.kill_session("quota_second").await?);
    assert!(!client.kill_session("quota_missing").await?);

    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn default_prune_seeds_legacy_guardian_activity_before_expiring_it() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 4, 86_400)?;
    let client = DaemonClient::new(&socket);
    wait_for_daemon(&client).await?;

    let thread_id = "legacy_metadata";
    let _state = initialize_daemon_session(&socket, workspace.path(), thread_id).await?;
    let guardian_dir = runtime_dir.path().join("guardians");
    let mut entries = std::fs::read_dir(&guardian_dir)?;
    let metadata = entries
        .find_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                .then_some(path)
        })
        .ok_or_else(|| anyhow::anyhow!("guardian metadata was not created"))?;
    std::fs::remove_file(&metadata)?;

    let default_prune = client.prune_sessions(None).await?;
    assert!(default_prune.removed_thread_ids.is_empty(), "{default_prune:?}");
    assert!(metadata.exists(), "legacy guardian metadata should be reseeded");

    let explicit_prune = client.prune_sessions(Some(0)).await?;
    assert_eq!(explicit_prune.removed_thread_ids, vec![thread_id]);
    daemon.kill()?;
    let _ = daemon.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_prune_removes_idle_but_preserves_active_sessions() -> anyhow::Result<()> {
    let workspace = TempDir::new()?;
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let mut daemon = spawn_daemon_with_limits(&socket, 4, 86_400)?;
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
