#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::daemon::{DaemonClient, DaemonServer, DaemonShellRuntime};
use winx_code_agent::errors::{Result, WinxError};
use winx_code_agent::runtime::EmbeddedShellRuntime;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::state::terminal::strip_ansi_codes;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

async fn initialized_state(
    workspace: &TempDir,
    thread_id: &str,
) -> Result<Arc<Mutex<Option<BashState>>>> {
    let state = Arc::new(Mutex::new(None));
    tools::initialize::handle_tool_call(
        &state,
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
    Ok(state)
}

fn probe(thread_id: &str) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::Command {
            command: "printf 'runtime-parity\\n'".to_string(),
            is_background: false,
            allow_multi: false,
        },
        wait_for_seconds: Some(1.0),
        thread_id: thread_id.to_string(),
    }
}

fn normalize_prompt_nonce(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            let Some(start) = line.find("──➤").map(|index| index + "──➤".len()) else {
                return line.to_string();
            };
            let Some(length) = line[start..].find(':') else {
                return line.to_string();
            };
            let end = start + length;
            let nonce = &line[start..end];
            if nonce.len() == 16 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                format!("{}<PROMPT_NONCE>{}", &line[..start], &line[end..])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn embedded_and_uds_runtime_have_the_same_action_contract() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());

    let embedded_state = initialized_state(&workspace, "rpc-parity-embedded").await?;
    let daemon_state = initialized_state(&workspace, "rpc-parity-daemon").await?;

    let embedded = tools::bash_command::handle_tool_call_with_runtime(
        &EmbeddedShellRuntime,
        &embedded_state,
        probe("rpc-parity-embedded"),
    )
    .await?;
    let daemon = tools::bash_command::handle_tool_call_with_runtime(
        &DaemonShellRuntime::new(socket),
        &daemon_state,
        probe("rpc-parity-daemon"),
    )
    .await?;

    server_task.abort();
    assert_eq!(normalize_prompt_nonce(&embedded), normalize_prompt_nonce(&daemon));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_output_cursors_are_independent_per_consumer() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());
    let state = initialized_state(&workspace, "rpc-multi-consumer").await?;
    let runtime = DaemonShellRuntime::new(&socket);

    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        probe("rpc-multi-consumer"),
    )
    .await?;

    let client = DaemonClient::new(socket);
    let sessions = client.list_sessions().await?;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let first = client.read_output("rpc-multi-consumer", "consumer-a").await?;
    let second = client.read_output("rpc-multi-consumer", "consumer-b").await?;
    let first_again = client.read_output("rpc-multi-consumer", "consumer-a").await?;

    server_task.abort();
    assert!(first.output.contains("runtime-parity"), "{first:?}");
    assert_eq!(first.output, second.output);
    assert!(first_again.output.is_empty(), "{first_again:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_drains_output_without_an_attached_consumer() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());
    let state = initialized_state(&workspace, "rpc-continuous-drain").await?;
    let runtime = DaemonShellRuntime::new(&socket);

    tools::bash_command::handle_tool_call_with_runtime(
        &runtime,
        &state,
        BashCommand {
            action_json: BashCommandAction::Command {
                command: "seq 1 200000; printf 'continuous-drain-marker\\n'".to_string(),
                is_background: false,
                allow_multi: true,
            },
            wait_for_seconds: Some(0.0),
            thread_id: "rpc-continuous-drain".to_string(),
        },
    )
    .await?;

    let client = DaemonClient::new(&socket);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if client.list_sessions().await?.iter().any(|session| !session.running) {
                return Result::<()>::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| WinxError::CommandTimeout {
        command: "continuous drain probe".to_string(),
        timeout_seconds: 8,
    })??;

    let recovered = client.read_output("rpc-continuous-drain", "late-attach").await?;
    let listed = client.list_sessions().await?;
    assert!(recovered.output.contains("continuous-drain-marker"), "{recovered:?}");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].shell_pid.is_some(), "{listed:?}");
    assert!(client.kill_session("rpc-continuous-drain").await?);
    assert!(client.list_sessions().await?.is_empty());

    server_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn journal_keeps_identical_output_from_consecutive_commands() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());
    let state = initialized_state(&workspace, "rpc-repeated-output").await?;
    let runtime = DaemonShellRuntime::new(&socket);

    for _ in 0..2 {
        tools::bash_command::handle_tool_call_with_runtime(
            &runtime,
            &state,
            probe("rpc-repeated-output"),
        )
        .await?;
    }

    let output = DaemonClient::new(socket)
        .read_output("rpc-repeated-output", "repeated-reader")
        .await?
        .output;
    server_task.abort();
    assert_eq!(
        output.lines().filter(|line| strip_ansi_codes(line).trim() == "runtime-parity").count(),
        2,
        "{output}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_cursor_is_independent_per_adapter() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());
    let state_a = initialized_state(&workspace, "rpc-status-consumers").await?;
    let state_b = initialized_state(&workspace, "rpc-status-consumers").await?;
    let runtime_a = DaemonShellRuntime::with_consumer_id(&socket, "adapter-a");
    let runtime_b = DaemonShellRuntime::with_consumer_id(&socket, "adapter-b");

    tools::bash_command::handle_tool_call_with_runtime(
        &runtime_a,
        &state_a,
        BashCommand {
            action_json: BashCommandAction::Command {
                command:
                    "printf 'first-consumer-marker\\n'; sleep 0.5; printf 'second-consumer-marker\\n'; sleep 5"
                        .to_string(),
                is_background: false,
                allow_multi: true,
            },
            wait_for_seconds: Some(0.2),
            thread_id: "rpc-status-consumers".to_string(),
        },
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(650)).await;

    let status = |thread_id: &str| BashCommand {
        action_json: BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id: None,
            scrollback_lines: None,
            verbose: false,
        },
        wait_for_seconds: Some(0.1),
        thread_id: thread_id.to_string(),
    };
    let output_a = tools::bash_command::handle_tool_call_with_runtime(
        &runtime_a,
        &state_a,
        status("rpc-status-consumers"),
    )
    .await?;
    let output_b = tools::bash_command::handle_tool_call_with_runtime(
        &runtime_b,
        &state_b,
        status("rpc-status-consumers"),
    )
    .await?;

    let _ = DaemonClient::new(&socket).interrupt_session("rpc-status-consumers").await;
    server_task.abort();
    assert!(output_a.contains("second-consumer-marker"), "{output_a}");
    assert!(output_b.contains("second-consumer-marker"), "{output_b}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_runtime_owns_initialization_and_reset() -> Result<()> {
    let workspace = TempDir::new()?;
    let socket_dir = TempDir::new()?;
    let socket = socket_dir.path().join("winxd.sock");
    let server = DaemonServer::bind(&socket).await?;
    let server_task = tokio::spawn(server.serve());
    let state = Arc::new(Mutex::new(None));
    let runtime = DaemonShellRuntime::new(&socket);

    let initialize = |init_type| Initialize {
        init_type,
        mode_name: ModeName::Wcgw,
        any_workspace_path: workspace.path().to_string_lossy().into_owned(),
        thread_id: "rpc-daemon-initialize".to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    tools::initialize::handle_tool_call_with_runtime(
        &runtime,
        &state,
        initialize(InitializeType::FirstCall),
    )
    .await?;

    let local_has_pty = {
        let state = state.lock().await;
        match state.as_ref() {
            Some(state) => state.pty_shell.lock().await.is_some(),
            None => false,
        }
    };
    let before = DaemonClient::new(&socket).session_info("rpc-daemon-initialize").await?;

    tools::initialize::handle_tool_call_with_runtime(
        &runtime,
        &state,
        initialize(InitializeType::ResetShell),
    )
    .await?;
    let after = DaemonClient::new(&socket).session_info("rpc-daemon-initialize").await?;

    let _ = DaemonClient::new(&socket).kill_session("rpc-daemon-initialize").await;
    server_task.abort();
    assert!(!local_has_pty, "daemon initialization leaked a PTY into the adapter");
    assert!(before.shell_pid.is_some(), "{before:?}");
    assert!(after.shell_pid.is_some(), "{after:?}");
    assert_ne!(before.shell_pid, after.shell_pid, "{before:?} {after:?}");
    Ok(())
}
