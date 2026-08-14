#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::daemon::{DaemonClient, DaemonShellRuntime, SessionInfo};
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

const TEST_NAME: &str = "daemon_session_survives_sigkill_of_adapter";
const THREAD_ID: &str = "daemon-lifecycle-e2e";

fn spawn_daemon(socket: &Path) -> anyhow::Result<std::process::Child> {
    Ok(Command::new(env!("CARGO_BIN_EXE_winxd"))
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
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

#[tokio::test(flavor = "multi_thread")]
async fn daemon_session_survives_sigkill_of_adapter() -> anyhow::Result<()> {
    if std::env::var_os("WINX_E2E_ADAPTER_CHILD").is_some() {
        let socket = std::env::var_os("WINX_E2E_SOCKET").expect("child socket env");
        let workspace = std::env::var_os("WINX_E2E_WORKSPACE").expect("child workspace env");
        let ready = std::env::var_os("WINX_E2E_READY").expect("child ready env");
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
    assert_eq!(after.cwd, workspace.path().join("nested").canonicalize()?.to_string_lossy());
    assert!(recovered.output.contains("adapter-before"), "{recovered:?}");
    assert!(recovered.output.contains("adapter-after"), "{recovered:?}");
    Ok(())
}
