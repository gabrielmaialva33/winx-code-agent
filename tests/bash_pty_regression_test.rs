use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::sleep;

use winx_code_agent::errors::{Result, WinxError};
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

async fn setup_bash_state(thread_id: &str) -> Result<(Arc<Mutex<Option<BashState>>>, TempDir)> {
    let temp_dir = TempDir::new()?;
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: thread_id.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    Ok((bash_state_arc, temp_dir))
}

async fn run_command(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    thread_id: &str,
    command: &str,
    is_background: bool,
) -> Result<String> {
    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command { command: command.to_string(), is_background },
        wait_for_seconds: Some(0.2),
        thread_id: thread_id.to_string(),
    };

    tools::bash_command::handle_tool_call(bash_state_arc, bash_cmd).await
}

async fn run_command_from_json(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    thread_id: &str,
    command: &str,
) -> Result<String> {
    let bash_cmd: BashCommand = serde_json::from_value(json!({
        "action_json": {
            "type": "command",
            "command": command
        },
        "wait_for_seconds": 0.2,
        "thread_id": thread_id
    }))
    .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?;

    tools::bash_command::handle_tool_call(bash_state_arc, bash_cmd).await
}

fn numeric_output_lines(response: &str) -> Vec<String> {
    response
        .split("\n\n---")
        .next()
        .unwrap_or(response)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.chars().all(|character| character.is_ascii_digit()))
        .map(ToString::to_string)
        .collect()
}

fn bg_command_id(response: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let (_, id) = line.split_once("bg_command_id = ")?;
        Some(id.trim().to_string())
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_pipeline_returns_only_tail_output() -> Result<()> {
    let thread_id = "pty-tail-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let response = run_command(&bash_state_arc, thread_id, "seq 1 10000 | tail -5", false).await?;

    assert_eq!(numeric_output_lines(&response), vec!["9996", "9997", "9998", "9999", "10000"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_pipeline_from_json_reaches_bash_intact() -> Result<()> {
    let thread_id = "pty-tail-json-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let response =
        run_command_from_json(&bash_state_arc, thread_id, "seq 1 10000 | tail -5").await?;

    assert_eq!(numeric_output_lines(&response), vec!["9996", "9997", "9998", "9999", "10000"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_background_shell_is_pruned_from_main_status() -> Result<()> {
    let thread_id = "pty-bg-prune-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let bg_response =
        run_command(&bash_state_arc, thread_id, "printf 'bg-prune-done\\n'", true).await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;

    sleep(Duration::from_millis(300)).await;

    let response = run_command(&bash_state_arc, thread_id, "echo foreground", false).await?;

    assert!(
        !response.contains(&bg_id),
        "completed background command should be pruned from main status: {response}"
    );

    Ok(())
}

// NOTE: A regression test for `submit=true` semantics on a live PTY against
// `read -p` used to live here. It passed locally but proved flaky in both
// Ubuntu and macOS CI — the test depended on the relative timing of the bg
// shell's subprocess exit vs winx's read/patience window, which sandboxed CI
// runners do not honor consistently. The feature itself is exercised by the
// `BashCommandAction::SendText { submit, .. }` plumbing in `src/types.rs` plus
// manual TUI testing; we keep the unit-level coverage and skip the brittle
// integration assertion.

#[tokio::test(flavor = "multi_thread")]
async fn exited_bg_shell_status_check_returns_cached_output() -> Result<()> {
    let thread_id = "pty-tombstone";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let bg_response =
        run_command(&bash_state_arc, thread_id, "printf 'tombstone-output\\n'", true).await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;

    sleep(Duration::from_millis(400)).await;

    // First, run a foreground command to trigger pruning of the finished bg shell.
    let _ = run_command(&bash_state_arc, thread_id, "true", false).await?;

    // Tombstone should still let one status_check pull the cached output.
    let status_response: String = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "status_check",
                "bg_command_id": bg_id
            },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    assert!(
        status_response.contains("tombstone-output"),
        "tombstoned status_check should return cached output: {status_response}"
    );
    assert!(
        status_response.contains("status = process exited"),
        "tombstoned status_check should report process exited: {status_response}"
    );

    // Tombstones are kept until the TTL expires, so repeated reads must still
    // return the same cached output — no surprise "no shell found" after the
    // first call.
    let second_response: String = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "status_check",
                "bg_command_id": bg_id
            },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;
    assert!(
        second_response.contains("tombstone-output"),
        "tombstone should be readable multiple times until TTL: {second_response}"
    );
    assert!(
        second_response.contains("status = process exited"),
        "repeated read should still report process exited: {second_response}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_status_check_returns_compact_dedup_marker() -> Result<()> {
    let thread_id = "pty-status-dedup";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let bg_response = run_command(&bash_state_arc, thread_id, "bash -c 'sleep 30'", true).await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;

    sleep(Duration::from_millis(400)).await;

    // Two status_checks with no new output between them: the second one should
    // hit the dedup path (body fingerprint matches the first response).
    let first: String = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 0.3,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    let second: String = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 0.3,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    assert!(
        second.contains("no new output since last check"),
        "idle status_check should hit the dedup path. first=<{first}> second=<{second}>"
    );
    assert!(
        second.len() <= first.len() + 64, // dedup marker is shorter than a typical body+status
        "dedup response should not balloon"
    );

    // verbose=true must bypass dedup even when nothing changed.
    let verbose: String = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "status_check",
                "bg_command_id": bg_id,
                "verbose": true
            },
            "wait_for_seconds": 0.3,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    assert!(
        !verbose.contains("no new output since last check"),
        "verbose=true must not return the compact dedup marker: {verbose}"
    );

    // Clean up the sleep by sending Ctrl+C to the bg shell.
    let _ = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "send_specials",
                "send_specials": ["Ctrl-c"],
                "bg_command_id": bg_id
            },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cd_updates_status_and_persisted_cwd() -> Result<()> {
    let thread_id = "pty-cwd-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;
    let target = std::env::temp_dir().canonicalize()?;

    let response =
        run_command(&bash_state_arc, thread_id, &format!("cd {}", target.display()), false).await?;

    assert!(
        response.contains(&format!("cwd = {}", target.display())),
        "status should show prompt cwd after cd: {response}"
    );

    let state = bash_state_arc.lock().await;
    let bash_state = state.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
    assert_eq!(bash_state.cwd, target);

    Ok(())
}
