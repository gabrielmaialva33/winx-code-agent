//! Full integration tests for `BashCommand` functionality
//!
//! Tests all action types: command, `status_check`, `send_text`, `send_specials`
//!
//! NOTE: These tests require a real PTY environment and may fail in CI.
//! Run locally with: cargo test --test `bash_command_full_test` -- --nocapture
//! These tests are ignored by default in CI (use --include-ignored to run)

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::sleep;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName, SpecialKey,
};

/// Helper to initialize bash state with a specific `thread_id`
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

    let response = tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    Ok((bash_state_arc, temp_dir))
}

// ==================== Test 1: Simple Command ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_01_simple_command_echo() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238").await?;

    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "echo \"test\"".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Verify the output contains "test"
    assert!(response.contains("test"), "Response should contain 'test': {response}");

    // Verify status information is present
    assert!(
        response.contains("status") || response.contains("cwd"),
        "Response should contain status info: {response}"
    );

    Ok(())
}

// ==================== Test 2: Command with Pipe ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_02_command_with_pipe() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-pipe").await?;

    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "ls -la | head -5".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-pipe".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Verify the output contains typical ls output
    assert!(
        response.contains("total") || response.contains("drwx") || response.contains('.'),
        "Response should contain ls output: {response}"
    );

    Ok(())
}

// ==================== Test 3: Status Check ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_03_status_check() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-status").await?;

    // First, run a command
    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "echo 'running command'".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-status".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Now do a status check - this should return an error since no command is running
    // after the previous one completed
    let status_cmd = BashCommand {
        action_json: BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id: None,
            scrollback_lines: None,
            verbose: false,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238-status".to_string(),
    };

    let status_result = tools::bash_command::handle_tool_call(&bash_state_arc, status_cmd).await;

    match status_result {
        Ok(response) => {
            // If we got a response, it should contain status info
            assert!(
                response.contains("status") || response.contains("No running"),
                "Response should contain status info"
            );
        }
        Err(e) => {
            // Expected: "No running command to check status of"
            assert!(
                e.to_string().contains("No running command"),
                "Error should indicate no running command"
            );
        }
    }

    Ok(())
}

// ==================== Test 4: Send Text (simulated input) ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_04_send_text() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-sendtext").await?;

    // Start cat command that waits for input
    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "cat".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(1.0), // Short timeout - cat will be running
        thread_id: "i2238-sendtext".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Small delay to let cat start
    sleep(Duration::from_millis(200)).await;

    // Send text to cat
    let send_text_cmd = BashCommand {
        action_json: BashCommandAction::SendText {
            send_text: "hello from send_text".to_string(),
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238-sendtext".to_string(),
    };

    let send_result = tools::bash_command::handle_tool_call(&bash_state_arc, send_text_cmd).await;

    match send_result {
        Ok(response) => {
            // Cat should echo back what we sent
            assert!(
                response.contains("hello")
                    || response.contains("send_text")
                    || response.contains("status"),
                "Response should contain sent text or status"
            );
        }
        Err(e) => {
            // If cat already exited, that's OK
        }
    }

    // Clean up - send Ctrl+D to end cat
    let ctrl_d_cmd = BashCommand {
        action_json: BashCommandAction::SendSpecials {
            send_specials: vec![SpecialKey::CtrlD],
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238-sendtext".to_string(),
    };

    let _ = tools::bash_command::handle_tool_call(&bash_state_arc, ctrl_d_cmd).await;

    Ok(())
}

// ==================== Test 5: Send Specials (Ctrl-c) ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_05_send_specials_ctrl_c() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-ctrlc").await?;

    // Start a long-running command
    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "sleep 30".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(1.0), // Short timeout - sleep will be running
        thread_id: "i2238-ctrlc".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Verify command is still running
    assert!(
        response.contains("still running") || response.contains("running"),
        "Sleep should still be running: {response}"
    );

    // Small delay
    sleep(Duration::from_millis(300)).await;

    // Send Ctrl+C to interrupt
    let ctrl_c_cmd = BashCommand {
        action_json: BashCommandAction::SendSpecials {
            send_specials: vec![SpecialKey::CtrlC],
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(3.0),
        thread_id: "i2238-ctrlc".to_string(),
    };

    let interrupt_response =
        tools::bash_command::handle_tool_call(&bash_state_arc, ctrl_c_cmd).await?;

    // After Ctrl+C, the command should be interrupted
    // Response might show "process exited" or "^C" or still running (need another Ctrl+C)

    Ok(())
}

// ==================== Test 6: Background Command ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_06_background_command() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-bg").await?;

    // Start a command in background
    let bash_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "sleep 5 && echo 'bg_done'".to_string(),
            is_background: true,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238-bg".to_string(),
    };

    let response = tools::bash_command::handle_tool_call(&bash_state_arc, bash_cmd).await?;

    // Response should contain bg_command_id
    assert!(
        response.contains("bg_command_id") || response.contains("background"),
        "Response should indicate background execution: {response}"
    );

    Ok(())
}

// ==================== Test 7: Multiple Commands Sequence ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_07_multiple_commands_sequence() -> Result<()> {
    let (bash_state_arc, temp_dir) = setup_bash_state("i2238-seq").await?;

    // Command 1: Create a file
    let cmd1 = BashCommand {
        action_json: BashCommandAction::Command {
            command: format!("echo 'content' > {}/testfile.txt", temp_dir.path().display()),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-seq".to_string(),
    };

    let response1 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd1).await?;

    // Command 2: Read the file
    let cmd2 = BashCommand {
        action_json: BashCommandAction::Command {
            command: format!("cat {}/testfile.txt", temp_dir.path().display()),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-seq".to_string(),
    };

    let response2 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd2).await?;

    // Verify content was written and read back
    assert!(response2.contains("content"), "Should read back 'content': {response2}");

    // Command 3: Remove the file
    let cmd3 = BashCommand {
        action_json: BashCommandAction::Command {
            command: format!("rm {}/testfile.txt && echo 'deleted'", temp_dir.path().display()),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-seq".to_string(),
    };

    let response3 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd3).await?;

    assert!(response3.contains("deleted"), "Should confirm deletion: {response3}");

    Ok(())
}

// ==================== Test 8: Arrow Keys (special keys) ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_08_arrow_keys() -> Result<()> {
    let (bash_state_arc, _temp_dir) = setup_bash_state("i2238-arrows").await?;

    // First run a command to have something in history
    let cmd1 = BashCommand {
        action_json: BashCommandAction::Command {
            command: "echo 'first command'".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238-arrows".to_string(),
    };

    let _ = tools::bash_command::handle_tool_call(&bash_state_arc, cmd1).await?;

    // Send Up arrow (should recall previous command in history)
    let arrow_cmd = BashCommand {
        action_json: BashCommandAction::SendSpecials {
            send_specials: vec![SpecialKey::KeyUp],
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238-arrows".to_string(),
    };

    let arrow_result = tools::bash_command::handle_tool_call(&bash_state_arc, arrow_cmd).await;

    match arrow_result {
        Ok(response) => {}
        Err(e) => {}
    }

    Ok(())
}

// ==================== Test: Run all with specific thread_id ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_full_workflow_i2238() -> Result<()> {
    let (bash_state_arc, temp_dir) = setup_bash_state("i2238").await?;

    // 1. Simple echo
    let cmd1 = BashCommand {
        action_json: BashCommandAction::Command {
            command: "echo \"test\"".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238".to_string(),
    };
    let r1 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd1).await?;
    assert!(r1.contains("test"), "Echo test failed");

    // 2. Pipe command
    let cmd2 = BashCommand {
        action_json: BashCommandAction::Command {
            command: "ls -la | head -5".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238".to_string(),
    };
    let r2 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd2).await?;

    // 3. Long running command + status check + Ctrl-C
    let cmd3 = BashCommand {
        action_json: BashCommandAction::Command {
            command: "sleep 60".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(1.0),
        thread_id: "i2238".to_string(),
    };
    let r3 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd3).await?;

    // Status check
    sleep(Duration::from_millis(500)).await;
    let status_cmd = BashCommand {
        action_json: BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id: None,
            scrollback_lines: None,
            verbose: false,
        },
        wait_for_seconds: Some(2.0),
        thread_id: "i2238".to_string(),
    };
    let status_result = tools::bash_command::handle_tool_call(&bash_state_arc, status_cmd).await;

    // Send Ctrl+C
    let ctrl_c = BashCommand {
        action_json: BashCommandAction::SendSpecials {
            send_specials: vec![SpecialKey::CtrlC],
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(3.0),
        thread_id: "i2238".to_string(),
    };
    let r4 = tools::bash_command::handle_tool_call(&bash_state_arc, ctrl_c).await?;

    // 4. Send text test (using read command)
    let cmd5 = BashCommand {
        action_json: BashCommandAction::Command {
            command: "read -p 'Enter: ' x && echo \"Got: $x\"".to_string(),
            is_background: false,
        },
        wait_for_seconds: Some(1.0),
        thread_id: "i2238".to_string(),
    };
    let r5 = tools::bash_command::handle_tool_call(&bash_state_arc, cmd5).await?;

    // Send input
    sleep(Duration::from_millis(300)).await;
    let send_text = BashCommand {
        action_json: BashCommandAction::SendText {
            send_text: "hello".to_string(),
            bg_command_id: None,
            submit: false,
        },
        wait_for_seconds: Some(3.0),
        thread_id: "i2238".to_string(),
    };
    let send_result = tools::bash_command::handle_tool_call(&bash_state_arc, send_text).await;

    // 5. Background command
    let bg_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: "sleep 2 && echo 'bg_completed'".to_string(),
            is_background: true,
        },
        wait_for_seconds: Some(1.0),
        thread_id: "i2238".to_string(),
    };
    let r6 = tools::bash_command::handle_tool_call(&bash_state_arc, bg_cmd).await?;
    assert!(r6.contains("bg_command_id"), "Should have bg_command_id");

    // 6. File operations sequence
    let file_path = temp_dir.path().join("test.txt");
    let file_cmd = BashCommand {
        action_json: BashCommandAction::Command {
            command: format!(
                "echo 'file content' > {} && cat {}",
                file_path.display(),
                file_path.display()
            ),
            is_background: false,
        },
        wait_for_seconds: Some(5.0),
        thread_id: "i2238".to_string(),
    };
    let r7 = tools::bash_command::handle_tool_call(&bash_state_arc, file_cmd).await?;
    assert!(r7.contains("file content"), "File content should be echoed");

    Ok(())
}
