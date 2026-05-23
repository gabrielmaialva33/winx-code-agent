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
