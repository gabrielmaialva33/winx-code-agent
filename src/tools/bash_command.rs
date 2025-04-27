//! Implementation of the BashCommand tool.
//!
//! This module provides the implementation for the BashCommand tool, which is used
//! to execute shell commands, check command status, and interact with the shell.

use anyhow::Context as AnyhowContext;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{BashCommand, BashCommandAction, SpecialKey};

/// Maximum output length to prevent excessive responses
const MAX_OUTPUT_LENGTH: usize = 100_000;


/// Process simple command execution for a bash command
///
/// This handles command execution, truncating output if necessary, and
/// providing status information.
///
/// # Arguments
///
/// * `command` - The command string to execute
/// * `cwd` - Current working directory for the command
/// * `timeout` - Optional timeout in seconds
///
/// # Returns
///
/// A Result containing the command output and status
#[instrument(level = "debug", skip(command, cwd))]
async fn execute_simple_command(command: &str, cwd: &Path, timeout: Option<f32>) -> Result<String> {
    debug!("Executing command: {}", command);

    // Create command with proper working directory
    let start_time = Instant::now();
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Execute the command
    let output = cmd.output().context("Failed to execute command")?;
    let elapsed = start_time.elapsed();

    // Process output
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut result = format!("{}{}", stdout, stderr);

    // Truncate if too long
    if result.len() > MAX_OUTPUT_LENGTH {
        result = format!(
            "(...truncated)\n{}",
            &result[result.len() - MAX_OUTPUT_LENGTH..]
        );
    }

    // Add status information
    let exit_status = if output.status.success() {
        "Command completed successfully".to_string()
    } else {
        format!("Command failed with status: {}", output.status)
    };

    // Get current working directory
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    debug!("Command executed in {:.2?}", elapsed);
    Ok(format!(
        "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
        result, exit_status, current_dir
    ))
}

/// Process commands requiring screen for background execution
///
/// This handles commands that should be run in a screen session to allow
/// for background execution and later interaction.
///
/// # Arguments
///
/// * `command` - The command string to execute
/// * `cwd` - Current working directory for the command
/// * `screen_name` - Name for the screen session
///
/// # Returns
///
/// A Result containing the initial output and status
#[instrument(level = "debug", skip(command, cwd, screen_name))]
async fn execute_in_screen(command: &str, cwd: &Path, screen_name: &str) -> Result<String> {
    debug!(
        "Executing command in screen session '{}': {}",
        screen_name, command
    );

    // Check if screen is available
    let screen_check = Command::new("which")
        .arg("screen")
        .output()
        .context("Failed to check for screen command")?;

    if !screen_check.status.success() {
        warn!("Screen command not found, falling back to direct execution");
        return execute_simple_command(command, cwd, None).await;
    }

    // Clean up any existing screen with the same name
    let _cleanup = Command::new("screen")
        .args(["-X", "-S", screen_name, "quit"])
        .output();

    // Start a new screen session with the command
    let screen_cmd = format!(
        "screen -dmS {} bash -c '{} ; echo \"Command completed with exit code: $?\" ; sleep 1'",
        screen_name,
        command.replace("'", "'\\''")
    );

    let screen_start = Command::new("sh")
        .arg("-c")
        .arg(&screen_cmd)
        .current_dir(cwd)
        .output()
        .context("Failed to start screen session")?;

    if !screen_start.status.success() {
        let stderr = String::from_utf8_lossy(&screen_start.stderr).to_string();
        error!("Failed to start screen session: {}", stderr);
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to start screen session: {}",
            stderr
        )));
    }

    // Wait briefly for screen to initialize
    sleep(Duration::from_millis(300)).await;

    // Check if screen session is running
    let screen_check = Command::new("screen")
        .args(["-ls"])
        .output()
        .context("Failed to list screen sessions")?;

    let screen_list = String::from_utf8_lossy(&screen_check.stdout).to_string();

    // Get current working directory
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    Ok(format!(
        "Started command in background screen session '{}'.\n\
        Use status_check to get output.\n\n\
        Screen sessions:\n{}\n\
        ---\n\n\
        status = running in background\n\
        cwd = {}\n",
        screen_name, screen_list, current_dir
    ))
}

/// Check the status of a running command in a screen session
///
/// This retrieves the current output from a screen session and returns it
/// with status information.
///
/// # Arguments
///
/// * `screen_name` - Name of the screen session to check
/// * `cwd` - Current working directory
///
/// # Returns
///
/// A Result containing the current output and status
#[instrument(level = "debug", skip(screen_name, cwd))]
async fn check_screen_status(screen_name: &str, cwd: &Path) -> Result<String> {
    debug!("Checking status of screen session: {}", screen_name);

    // Check if screen session exists
    let screen_check = Command::new("screen")
        .args(["-ls"])
        .output()
        .context("Failed to list screen sessions")?;

    let screen_list = String::from_utf8_lossy(&screen_check.stdout).to_string();

    if !screen_list.contains(screen_name) {
        warn!("Screen session '{}' not found", screen_name);
        return Ok(format!(
            "No running command or screen session '{}' not found.\n\
            Current screen sessions:\n{}\n\
            ---\n\n\
            status = no active command\n\
            cwd = {}\n",
            screen_name,
            screen_list,
            cwd.to_string_lossy()
        ));
    }

    // Capture current output from screen session
    let capture_cmd = format!(
        "screen -S {} -X hardcopy /tmp/screen_capture.txt && cat /tmp/screen_capture.txt",
        screen_name
    );

    let capture = Command::new("sh")
        .arg("-c")
        .arg(&capture_cmd)
        .output()
        .context("Failed to capture screen output")?;

    let mut output = String::from_utf8_lossy(&capture.stdout).to_string();

    // Truncate if too long
    if output.len() > MAX_OUTPUT_LENGTH {
        output = format!(
            "(...truncated)\n{}",
            &output[output.len() - MAX_OUTPUT_LENGTH..]
        );
    }

    // Check if command is still running
    let running_check = Command::new("sh")
        .arg("-c")
        .arg(format!("screen -list | grep {}", screen_name))
        .output()
        .context("Failed to check if screen session is running")?;

    let status = if running_check.status.success() {
        "still running"
    } else {
        "process exited"
    };

    Ok(format!(
        "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
        output,
        status,
        cwd.to_string_lossy()
    ))
}

/// Handle sending input to a running command in a screen session
///
/// This sends text or special keys to a running screen session.
///
/// # Arguments
///
/// * `input` - The text or keys to send
/// * `screen_name` - Name of the screen session
/// * `is_special` - Whether the input contains special keys
///
/// # Returns
///
/// A Result containing the status message
#[instrument(level = "debug", skip(input, screen_name))]
async fn send_to_screen(input: &str, screen_name: &str, is_special: bool) -> Result<String> {
    debug!(
        "Sending input to screen session '{}': {}",
        screen_name, input
    );

    // Check if screen session exists
    let screen_check = Command::new("screen")
        .args(["-ls"])
        .output()
        .context("Failed to list screen sessions")?;

    let screen_list = String::from_utf8_lossy(&screen_check.stdout).to_string();

    if !screen_list.contains(screen_name) {
        warn!("Screen session '{}' not found", screen_name);
        return Err(WinxError::CommandExecutionError(format!(
            "No running command or screen session '{}' not found",
            screen_name
        )));
    }

    // Construct the stuff command
    let stuff_cmd = if is_special {
        format!("screen -S {} -X stuff '{}'", screen_name, input)
    } else {
        format!(
            "screen -S {} -X stuff '{}'",
            screen_name,
            input.replace("'", "'\\''")
        )
    };

    // Send input to the screen session
    let stuff = Command::new("sh")
        .arg("-c")
        .arg(&stuff_cmd)
        .output()
        .context("Failed to send input to screen session")?;

    if !stuff.status.success() {
        let stderr = String::from_utf8_lossy(&stuff.stderr).to_string();
        error!("Failed to send input to screen session: {}", stderr);
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to send input to screen session: {}",
            stderr
        )));
    }

    // Capture current output from screen session
    let capture_cmd = format!(
        "screen -S {} -X hardcopy /tmp/screen_capture.txt && cat /tmp/screen_capture.txt",
        screen_name
    );

    let capture = Command::new("sh")
        .arg("-c")
        .arg(&capture_cmd)
        .output()
        .context("Failed to capture screen output")?;

    let mut output = String::from_utf8_lossy(&capture.stdout).to_string();

    // Truncate if too long
    if output.len() > MAX_OUTPUT_LENGTH {
        output = format!(
            "(...truncated)\n{}",
            &output[output.len() - MAX_OUTPUT_LENGTH..]
        );
    }

    Ok(format!(
        "Input sent to screen session '{}'.\n\n{}\n\n---\n\nstatus = command running\n",
        screen_name, output
    ))
}

/// Converts a SpecialKey to its screen stuff input representation
fn special_key_to_screen_input(key: &SpecialKey) -> String {
    match key {
        SpecialKey::Enter => String::from("\r"),
        SpecialKey::KeyUp => String::from("\x1b[A"),
        SpecialKey::KeyDown => String::from("\x1b[B"),
        SpecialKey::KeyLeft => String::from("\x1b[D"),
        SpecialKey::KeyRight => String::from("\x1b[C"),
        SpecialKey::CtrlC => String::from("\x03"),
        SpecialKey::CtrlD => String::from("\x04"),
    }
}

/// Handles the BashCommand tool call
///
/// This function processes the BashCommand tool call, which executes shell
/// commands and interacts with the shell environment.
///
/// # Arguments
///
/// * `bash_state_arc` - Shared reference to the bash state
/// * `bash_command` - The bash command parameters
///
/// # Returns
///
/// A Result containing the response message to send to the client
///
/// # Errors
///
/// Returns an error if the command execution fails for any reason
#[instrument(level = "info", skip(bash_state_arc, bash_command))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
) -> Result<String> {
    info!("BashCommand tool called with: {:?}", bash_command);

    // We need to extract all the data we need from the bash state before awaiting
    // to avoid holding the MutexGuard across await points

    // Data to extract from bash_state
    let current_chat_id;
    let cwd;
    let allowed_commands;

    // Lock bash state to extract data
    {
        let bash_state_guard = bash_state_arc.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
        })?;

        // Ensure bash state is initialized
        let bash_state = match &*bash_state_guard {
            Some(state) => state,
            None => {
                error!("BashState not initialized");
                return Err(WinxError::BashStateNotInitialized);
            }
        };

        // Extract needed data
        current_chat_id = bash_state.current_chat_id.clone();
        cwd = bash_state.cwd.clone();
        allowed_commands = bash_state.bash_command_mode.allowed_commands.clone();
    }

    // Verify chat ID matches
    if bash_command.chat_id != current_chat_id {
        warn!(
            "Chat ID mismatch: expected {}, got {}",
            current_chat_id, bash_command.chat_id
        );
        return Err(WinxError::ChatIdMismatch(format!(
            "Chat ID mismatch. Expected: {}, Got: {}. Please use the correct chat ID or initialize with a new session.",
            current_chat_id, bash_command.chat_id
        )));
    }

    // Generate a consistent screen session name
    let screen_name = format!(
        "winx_{}_{}",
        current_chat_id,
        current_chat_id.chars().fold(0, |acc, c| acc + c as u32) % 10000
    );

    // Process the command based on action type
    match &bash_command.action_json {
        BashCommandAction::Command { command } => {
            debug!("Processing Command action: {}", command);

            // Verify command is allowed in current mode
            match &allowed_commands {
                crate::types::AllowedCommands::All(s) if s == "all" => {
                    // All commands allowed
                }
                crate::types::AllowedCommands::List(allowed) => {
                    // Check if command is in allowed list
                    let cmd_parts: Vec<&str> = command.split_whitespace().collect();
                    if let Some(cmd) = cmd_parts.first() {
                        if !allowed.iter().any(|a| a == cmd) {
                            error!("Command '{}' not allowed in current mode", cmd);
                            return Err(WinxError::CommandNotAllowed(format!(
                                "Command '{}' not allowed in current mode. Allowed commands: {:?}",
                                cmd, allowed
                            )));
                        }
                    }
                }
                _ => {
                    error!("No commands allowed in current mode");
                    return Err(WinxError::CommandNotAllowed(
                        "No commands allowed in current mode".to_string(),
                    ));
                }
            }

            // Check if command should run in background (contains &)
            if command.contains(" & ") || command.ends_with(" &") {
                info!("Command contains background operator, using screen instead");
                // Replace & with nothing and run in screen
                let clean_command = command.replace(" & ", " ").replace(" &", "");
                execute_in_screen(&clean_command, &cwd, &screen_name).await
            } else {
                // Normal command execution
                execute_simple_command(command, &cwd, bash_command.wait_for_seconds).await
            }
        }
        BashCommandAction::StatusCheck { status_check: _ } => {
            debug!("Processing StatusCheck action");
            check_screen_status(&screen_name, &cwd).await
        }
        BashCommandAction::SendText { send_text } => {
            debug!("Processing SendText action: {}", send_text);
            if send_text.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_text cannot be empty".to_string(),
                ));
            }

            // Add a newline if not present
            let text = if send_text.ends_with('\n') {
                send_text.clone()
            } else {
                format!("{}\n", send_text)
            };

            send_to_screen(&text, &screen_name, false).await
        }
        BashCommandAction::SendSpecials { send_specials } => {
            debug!("Processing SendSpecials action: {:?}", send_specials);
            if send_specials.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_specials cannot be empty".to_string(),
                ));
            }

            // Convert special keys to screen input
            let mut special_input = String::new();
            for key in send_specials {
                special_input.push_str(&special_key_to_screen_input(key));
            }

            send_to_screen(&special_input, &screen_name, true).await
        }
        BashCommandAction::SendAscii { send_ascii } => {
            debug!("Processing SendAscii action: {:?}", send_ascii);
            if send_ascii.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_ascii cannot be empty".to_string(),
                ));
            }

            // Convert ASCII codes to string
            let ascii_input: String = send_ascii.iter().map(|&c| c as char).collect();

            send_to_screen(&ascii_input, &screen_name, true).await
        }
    }
}
