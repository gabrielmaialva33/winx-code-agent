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
        action_json: BashCommandAction::Command {
            command: command.to_string(),
            is_background,
            allow_multi: false,
        },
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

fn running_seconds(response: &str) -> Option<u64> {
    response.lines().find_map(|line| {
        let duration = line.strip_prefix("running for = ")?.strip_suffix(" seconds")?;
        duration.parse().ok()
    })
}

// wcgw parity: a trailing `| tail` is stripped by default (output is truncated
// server-side anyway), so the full command output reaches the model. The opt-out
// (`WINX_KEEP_TAIL_PIPE`) and the regex itself are covered by unit tests in
// `bash_command::tests` — kept out of here to avoid mutating process-wide env in
// concurrent integration tests.
#[tokio::test(flavor = "multi_thread")]
async fn tail_pipe_stripped_by_default() -> Result<()> {
    let thread_id = "pty-tail-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let response = run_command(&bash_state_arc, thread_id, "seq 1 5 | tail -2", false).await?;

    // Stripped → `seq 1 5`, so all five lines show, not just the last two.
    assert_eq!(numeric_output_lines(&response), vec!["1", "2", "3", "4", "5"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_pipe_stripped_from_json() -> Result<()> {
    let thread_id = "pty-tail-json-regression";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let response = run_command_from_json(&bash_state_arc, thread_id, "seq 1 5 | tail -2").await?;

    assert_eq!(numeric_output_lines(&response), vec!["1", "2", "3", "4", "5"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_cannot_execute_in_an_idle_shell() -> Result<()> {
    let thread_id = "pty-idle-send-guard";
    let (bash_state_arc, temp_dir) = setup_bash_state(thread_id).await?;
    let marker = temp_dir.path().join("idle-send-must-not-run");

    let result = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        BashCommand {
            action_json: BashCommandAction::SendText {
                send_text: format!("touch {}", marker.display()),
                bg_command_id: None,
                submit: true,
            },
            wait_for_seconds: Some(0.2),
            thread_id: thread_id.to_string(),
        },
    )
    .await;

    assert!(matches!(result, Err(WinxError::CommandExecutionError(_))));
    sleep(Duration::from_millis(100)).await;
    assert!(!marker.exists(), "send_text must not become an idle-shell command path");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn background_ids_are_owned_by_their_thread() -> Result<()> {
    let owner = "pty-bg-owner";
    let intruder = "pty-bg-intruder";
    let (owner_state, _owner_dir) = setup_bash_state(owner).await?;
    let (intruder_state, _intruder_dir) = setup_bash_state(intruder).await?;

    let bg_response = run_command(&owner_state, owner, "sleep 10", true).await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;

    let result = tools::bash_command::handle_tool_call(
        &intruder_state,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 0.1,
            "thread_id": intruder
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;
    let error = match result {
        Err(error) => error,
        Ok(response) => {
            return Err(WinxError::CommandExecutionError(format!(
                "another thread resolved the owner's background id: {response}"
            )))
        }
    };
    let message = error.to_string();
    assert!(!message.contains("sleep 10"), "error must not disclose another command: {message}");

    let _ = tools::bash_command::handle_tool_call(
        &owner_state,
        serde_json::from_value(json!({
            "action_json": {
                "type": "send_specials",
                "send_specials": ["Ctrl-c"],
                "bg_command_id": bg_id
            },
            "wait_for_seconds": 0.2,
            "thread_id": owner
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;
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

    // The background reaper should have converted the completed shell into a
    // tombstone without needing an unrelated foreground command to trigger GC.
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
async fn status_check_returns_output_emitted_after_initial_response() -> Result<()> {
    let thread_id = "pty-incremental-output";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let initial = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        BashCommand {
            action_json: BashCommandAction::Command {
                command: "sleep 0.4; printf 'late-output\\n'; sleep 5".to_string(),
                is_background: false,
                allow_multi: true,
            },
            wait_for_seconds: Some(0.1),
            thread_id: thread_id.to_string(),
        },
    )
    .await?;
    assert!(!initial.contains("late-output\n"));

    sleep(Duration::from_millis(600)).await;
    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "verbose": true },
            "wait_for_seconds": 0.1,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;
    assert!(
        status.contains("late-output"),
        "status_check must return bytes emitted since the prior response: {status}"
    );

    let _ = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "send_specials", "send_specials": ["Ctrl-c"] },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn first_background_completion_poll_keeps_final_output() -> Result<()> {
    let thread_id = "pty-reaper-reader-race";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let bg_response = run_command(
        &bash_state_arc,
        thread_id,
        "sleep 0.4\nprintf 'final-from-background\\n'",
        true,
    )
    .await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;

    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "status_check",
                "bg_command_id": bg_id,
                "verbose": true
            },
            "wait_for_seconds": 1.5,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    assert!(
        status.contains("final-from-background"),
        "the in-flight reader must keep ownership of final output: {status}"
    );
    assert!(status.contains("status = process exited"), "{status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn background_tombstone_keeps_exit_code_and_its_own_cwd() -> Result<()> {
    let thread_id = "pty-tombstone-metadata";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;
    let target = std::env::temp_dir().canonicalize()?;

    let bg_response =
        run_command(&bash_state_arc, thread_id, &format!("cd {} && false", target.display()), true)
            .await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;
    sleep(Duration::from_millis(500)).await;

    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 0.1,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    assert!(status.contains("exit code = 1"), "{status}");
    assert!(status.contains(&format!("cwd = {}", target.display())), "{status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn background_completion_does_not_replace_main_shell_cwd() -> Result<()> {
    let thread_id = "pty-background-cwd-isolation";
    let (bash_state_arc, temp_dir) = setup_bash_state(thread_id).await?;
    let main_cwd = temp_dir.path().canonicalize()?;
    let target = std::env::temp_dir().canonicalize()?;

    let bg_response = run_command(
        &bash_state_arc,
        thread_id,
        &format!("sleep 0.3\ncd {}\nsleep 0.1", target.display()),
        true,
    )
    .await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;
    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 1.5,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;
    assert!(status.contains(&format!("cwd = {}", target.display())), "{status}");

    let state_cwd = bash_state_arc
        .lock()
        .await
        .as_ref()
        .map(|state| state.cwd.clone())
        .ok_or(WinxError::BashStateNotInitialized)?;
    assert_eq!(state_cwd, main_cwd, "background cwd leaked into the main session state");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn background_tombstone_keeps_truncated_output_pointer() -> Result<()> {
    let thread_id = "pty-tombstone-scratch";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let bg_response =
        run_command(&bash_state_arc, thread_id, "head -c 1100000 /dev/zero | tr '\\0' x", true)
            .await?;
    let bg_id = bg_command_id(&bg_response).ok_or_else(|| {
        WinxError::CommandExecutionError("background response should include id".to_string())
    })?;
    sleep(Duration::from_millis(800)).await;

    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check", "bg_command_id": bg_id },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;
    assert!(status.contains("Output was truncated to fit context"), "{status}");
    assert!(status.contains(".winx/scratch/bash-output-"), "{status}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_foreground_commands_do_not_share_output() -> Result<()> {
    let thread_id = "pty-foreground-gate";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let command = |marker: &'static str| BashCommand {
        action_json: BashCommandAction::Command {
            command: format!("printf '{marker}-start\\n'; sleep 0.3; printf '{marker}-end\\n'"),
            is_background: false,
            allow_multi: true,
        },
        wait_for_seconds: Some(1.0),
        thread_id: thread_id.to_string(),
    };
    let (first, second) = tokio::join!(
        tools::bash_command::handle_tool_call(&bash_state_arc, command("first")),
        tools::bash_command::handle_tool_call(&bash_state_arc, command("second"))
    );
    let first = first?;
    let second = second?;

    assert!(first.contains("first-start") && first.contains("first-end"), "{first}");
    assert!(!first.contains("second-start"), "{first}");
    assert!(second.contains("second-start") && second.contains("second-end"), "{second}");
    assert!(!second.contains("first-start"), "{second}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_d_that_leaves_process_running_is_not_reported_as_failed_interrupt() -> Result<()> {
    let thread_id = "pty-ctrl-d-is-eof";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;
    let initial = run_command(&bash_state_arc, thread_id, "sleep 5", false).await?;
    assert!(initial.contains("status = still running"), "{initial}");

    let eof = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "send_specials", "send_specials": ["Ctrl-d"] },
            "wait_for_seconds": 0.1,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;
    assert!(eof.contains("status = still running"), "{eof}");
    assert!(!eof.contains("Failure interrupting"), "{eof}");

    let _ = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "send_specials", "send_specials": ["Ctrl-c"] },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;
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
async fn status_check_reports_total_command_runtime() -> Result<()> {
    let thread_id = "pty-total-runtime";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;

    let initial = run_command(&bash_state_arc, thread_id, "sleep 5", false).await?;
    assert!(
        initial.contains("status = still running"),
        "sleep should still be active after the initial short wait: {initial}"
    );

    sleep(Duration::from_millis(1_100)).await;
    let status = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": { "type": "status_check" },
            "wait_for_seconds": 0.1,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await?;

    let _ = tools::bash_command::handle_tool_call(
        &bash_state_arc,
        serde_json::from_value(json!({
            "action_json": {
                "type": "send_specials",
                "send_specials": ["Ctrl-c"]
            },
            "wait_for_seconds": 0.2,
            "thread_id": thread_id
        }))
        .map_err(|error| WinxError::ArgumentParseError(error.to_string()))?,
    )
    .await;

    let elapsed = running_seconds(&status).ok_or_else(|| {
        WinxError::CommandExecutionError(format!(
            "status should contain the running duration: {status}"
        ))
    })?;
    assert!(
        elapsed >= 1,
        "duration should include time before this status_check, got {elapsed}s: {status}"
    );

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

#[tokio::test(flavor = "multi_thread")]
async fn screen_drains_the_bounded_pty_queue() -> Result<()> {
    let thread_id = "pty-screen-drain";
    let (bash_state_arc, _temp_dir) = setup_bash_state(thread_id).await?;
    let command = BashCommand {
        action_json: BashCommandAction::Command {
            command: "python3 -c 'import os; [os.write(1, b\"x\" * 4096) for _ in range(2048)]'"
                .to_string(),
            is_background: false,
            allow_multi: false,
        },
        // Return before consuming output so the 1024-chunk channel can fill.
        wait_for_seconds: Some(0.0),
        thread_id: thread_id.to_string(),
    };
    let initial = tools::bash_command::handle_tool_call(&bash_state_arc, command).await?;
    assert!(initial.contains("status = still running"), "expected an active producer: {initial}");

    sleep(Duration::from_millis(150)).await;
    let mut exited = false;
    for _ in 0..20 {
        let screen = tools::bash_command::handle_tool_call(
            &bash_state_arc,
            BashCommand {
                action_json: BashCommandAction::Screen {
                    screen: true,
                    bg_command_id: None,
                    lines: Some(5),
                    diff: false,
                },
                wait_for_seconds: Some(0.0),
                thread_id: thread_id.to_string(),
            },
        )
        .await?;
        if screen.contains("status = process exited") {
            exited = true;
            break;
        }
        sleep(Duration::from_millis(25)).await;
    }

    assert!(exited, "screen polling must drain output so a verbose child can finish");
    Ok(())
}
