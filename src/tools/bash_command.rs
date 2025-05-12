//! Implementation of the BashCommand tool.
//!
//! This module provides the implementation for the BashCommand tool, which is used
//! to execute shell commands, check command status, and interact with the shell.

use anyhow::Context as AnyhowContext;
use rand::Rng;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, CommandState};
use crate::state::terminal::render_terminal_output;
use crate::types::{BashCommand, BashCommandAction, SpecialKey};

/// Maximum output length to prevent excessive responses
#[allow(dead_code)]
const MAX_OUTPUT_LENGTH: usize = 100_000;

/// Process simple command execution for a bash command
///
/// This handles command execution, truncating output if necessary, and
/// providing status information. Uses terminal emulation for better output
/// rendering.
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

    let raw_result = format!("{}{}", stdout, stderr);

    // Use terminal emulation to render the output
    let mut result = raw_result.clone();
    if !raw_result.is_empty() {
        let rendered_lines = render_terminal_output(&raw_result);
        if !rendered_lines.is_empty() {
            result = rendered_lines.join("\n");
        }
    }

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

    // Start a new screen session with the command, capturing exit code properly
    let screen_cmd = format!(
        "screen -dmS {} bash -c '{} ; ec=$? ; echo \"Command completed with exit code: $ec\" ; sleep 1 ; exit $ec'",
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

    // Setup automatic cleanup after 1 hour to avoid orphaned sessions
    let cleanup_cmd = format!(
        "sh -c '( sleep 3600 && if screen -list | grep -q \"{}\" ; then screen -X -S {} quit > /dev/null 2>&1 ; fi ) > /dev/null 2>&1 &'",
        screen_name, screen_name
    );

    let _cleanup_proc = Command::new("sh").arg("-c").arg(&cleanup_cmd).spawn();

    // Get current working directory
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    Ok(format!(
        "Started command in background screen session '{}'.\n\
        Use status_check to get output.\n\n\
        Screen sessions:\n{}\n\
        Note: Background process will be automatically terminated if still running after 1 hour.\n\
        ---\n\n\
        status = running in background\n\
        cwd = {}\n",
        screen_name, screen_list, current_dir
    ))
}

/// Execute a screen command, which creates a detached session
///
/// This is a specialized handler for screen commands, which allows background
/// process execution with the ability to reattach later.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `command` - The screen command to execute
///
/// # Returns
///
/// A Result containing the command output or an error
async fn execute_screen_command(bash_state: &mut BashState, command: &str) -> Result<String> {
    // Generate a unique screen session name if not specified
    let mut screen_name = format!("winx_{}", rand::rng().random_range(1000..10000));

    // Extract screen name if specified in command
    if command.contains(" -S ") {
        let parts: Vec<&str> = command.split(" -S ").collect();
        if parts.len() > 1 {
            let name_parts: Vec<&str> = parts[1].split_whitespace().collect();
            if !name_parts.is_empty() {
                screen_name = name_parts[0].to_string();
            }
        }
    }

    debug!(
        "Executing screen command with session name: {}",
        screen_name
    );

    // First, check if screen is available
    let cmd_result = execute_interactive_command(bash_state, "which screen", None).await;
    if let Err(e) = cmd_result {
        warn!(
            "Screen command not found, falling back to direct execution: {}",
            e
        );
        return execute_interactive_command(bash_state, command, None).await;
    }

    // Clean up any existing screen with same name
    let cleanup_cmd = format!("screen -X -S {} quit 2>/dev/null || true", screen_name);
    let _ = execute_interactive_command(bash_state, &cleanup_cmd, None).await;

    // Modified command to ensure we get useful output and proper cleanup
    let modified_command = if command.contains(" -d") || command.contains(" -dm") {
        // It's already a detached session, leave as is but ensure we track the session name
        if command.contains(" -S ") {
            // Command already specifies session name
            command.to_string()
        } else {
            // Add session name to track it
            command.replace("screen ", &format!("screen -S {} ", screen_name))
        }
    } else {
        // Add detached flag to prevent hanging and ensure we have a session name
        if command.contains(" -S ") {
            command.replace("screen ", "screen -dm ")
        } else {
            format!(
                "screen -dm -S {} {}",
                screen_name,
                command.strip_prefix("screen ").unwrap_or(command)
            )
        }
    };

    // Execute the screen command
    let result = execute_interactive_command(bash_state, &modified_command, None).await?;

    // Setup automatic cleanup after 1 hour to avoid orphaned sessions
    let cleanup_cmd = format!(
        "( sleep 3600 && if screen -list | grep -q '{}' ; then screen -X -S {} quit > /dev/null 2>&1 ; fi ) > /dev/null 2>&1 &",
        screen_name, screen_name
    );

    // Run the cleanup command without waiting for result
    let _ = execute_interactive_command(bash_state, &cleanup_cmd, None).await;

    // List screen sessions to confirm
    let list_cmd = "screen -ls";
    let screen_list = match execute_interactive_command(bash_state, list_cmd, None).await {
        Ok(output) => output,
        Err(_) => "Failed to list screen sessions".to_string(),
    };

    // Format nice response
    let success_msg = format!(
        "Screen session '{}' started.\n\n{}\n\n\
        To reattach: screen -r {}\n\
        To terminate: screen -X -S {} quit\n\n\
        Note: Background process will be automatically terminated if still running after 1 hour.\n\n\
        Current screen sessions:\n{}\n",
        screen_name, result, screen_name, screen_name, screen_list
    );

    Ok(success_msg)
}

/// Execute a command in the background using screen if available
///
/// This creates a detached screen session to run the command, and captures
/// the initial output to return to the user.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `command` - The command to execute
///
/// # Returns
///
/// A Result containing the command output or an error
async fn execute_background_command(bash_state: &mut BashState, command: &str) -> Result<String> {
    debug!("Executing background command: {}", command);

    // Generate a unique screen session name for this background job
    let screen_name = format!("winx_bg_{}", rand::rng().random_range(1000..10000));

    // Check if screen is available
    let screen_check = execute_interactive_command(bash_state, "which screen", None).await;

    if screen_check.is_ok() {
        // Create a modified command that runs inside a screen session with proper cleanup
        // This allows the command to continue running after we detach, but ensures cleanup
        let wrapped_command = format!(
            "screen -dm -S {} bash -c '{} ; ec=$? ; echo \"Command completed with status code: $ec\" > /tmp/{}_result ; exit $ec'",
            screen_name,
            command.replace("'", "'\\''"),  // Escape single quotes
            screen_name
        );

        // Start the command in a detached screen
        execute_interactive_command(bash_state, &wrapped_command, None).await?;

        // Check that the screen session started
        let screen_list = execute_interactive_command(bash_state, "screen -ls", None).await?;

        // Register a cleanup command to run after a timeout (e.g., terminate if orphaned)
        let cleanup_command = format!(
            "( sleep 3600 && if screen -list | grep -q '{}' ; then screen -X -S {} quit > /dev/null 2>&1 ; rm -f /tmp/{}_result ; fi ) > /dev/null 2>&1 &",
            screen_name, screen_name, screen_name
        );

        // Run the cleanup command in the background to avoid waiting
        let _ = execute_interactive_command(bash_state, &cleanup_command, None).await;

        // Format a nice response about the background process
        let response = format!(
            "Command started in background screen session '{}'.\n\
            \n\
            You can check its status later with:\n\
            - `screen -ls` to see if it's still running\n\
            - `screen -r {}` to attach to it (detach with Ctrl+A, d)\n\
            - `screen -X -S {} quit` to terminate it\n\
            - `cat /tmp/{}_result` to see the exit status when finished\n\
            \n\
            Note: Background process will be automatically terminated if still running after 1 hour.\n\
            \n\
            Current screen sessions:\n{}",
            screen_name, screen_name, screen_name, screen_name, screen_list
        );

        Ok(response)
    } else {
        // Screen is not available, fall back to normal execution
        warn!("Screen not available for background execution, falling back to normal execution");
        execute_interactive_command(bash_state, command, None).await
    }
}

/// Check the status of a running command in a screen session
///
/// This retrieves the current output from a screen session and returns it
/// with status information. Uses terminal emulation for better output
/// rendering.
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

    let raw_output = String::from_utf8_lossy(&capture.stdout).to_string();

    // Use terminal emulation to render the output
    let mut output = raw_output.clone();
    if !raw_output.is_empty() {
        let rendered_lines = render_terminal_output(&raw_output);
        if !rendered_lines.is_empty() {
            output = rendered_lines.join("\n");
        }
    }

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
/// Uses terminal emulation for better output rendering.
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

    // Give a small delay to allow the screen session to process the input
    sleep(Duration::from_millis(100)).await;

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

    let raw_output = String::from_utf8_lossy(&capture.stdout).to_string();

    // Use terminal emulation to render the output
    let mut output = raw_output.clone();
    if !raw_output.is_empty() {
        let rendered_lines = render_terminal_output(&raw_output);
        if !rendered_lines.is_empty() {
            output = rendered_lines.join("\n");
        }
    }

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
#[allow(dead_code)]
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

    // Check if chat_id is empty - handle this case first
    if bash_command.chat_id.is_empty() {
        error!("Empty chat_id provided in BashCommand");
        return Err(WinxError::ChatIdMismatch(
            "Error: No saved bash state found for chat ID \"\". Please initialize first with this ID.".to_string()
        ));
    }

    // We need to extract data from bash_state and make a clone to work with
    // to avoid holding the MutexGuard across await points

    // Data to extract from bash_state
    let mut bash_state: BashState;

    // Lock bash state to extract data
    {
        let bash_state_guard = bash_state_arc.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
        })?;

        // Ensure bash state is initialized
        let state = match &*bash_state_guard {
            Some(state) => state,
            None => {
                error!("BashState not initialized");
                return Err(WinxError::BashStateNotInitialized);
            }
        };

        // Clone the bash state to work with
        bash_state = state.clone();
    }

    // Verify chat ID matches
    if bash_command.chat_id != bash_state.current_chat_id {
        warn!(
            "Chat ID mismatch: expected {}, got {}",
            bash_state.current_chat_id, bash_command.chat_id
        );
        return Err(WinxError::ChatIdMismatch(format!(
            "Error: No saved bash state found for chat ID \"{}\". Please initialize first with this ID.",
            bash_command.chat_id
        )));
    }

    // Process the command based on action type
    match &bash_command.action_json {
        BashCommandAction::Command { command } => {
            debug!("Processing Command action: {}", command);

            // Verify command is allowed in current mode
            match &bash_state.bash_command_mode.allowed_commands {
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

            // Check for screen command specifically to handle it specially
            if command.trim().starts_with("screen ") {
                info!("Detected screen command, using special handling");
                execute_screen_command(&mut bash_state, command).await
            }
            // Check if command should run in background (contains &)
            else if command.contains(" & ")
                || command.ends_with(" &")
                || command.contains(" bg ")
                || command.contains(" &> ")
                || (command.contains(" > ") && command.contains(" < "))
            {
                info!("Command contains background operator, using background execution");
                execute_background_command(&mut bash_state, command).await
            } else {
                // Normal command execution
                execute_interactive_command(&mut bash_state, command, bash_command.wait_for_seconds)
                    .await
            }
        }
        BashCommandAction::StatusCheck { status_check: _ } => {
            debug!("Processing StatusCheck action");
            check_command_status(&mut bash_state).await
        }
        BashCommandAction::SendText { send_text } => {
            debug!("Processing SendText action: {}", send_text);
            if send_text.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_text cannot be empty".to_string(),
                ));
            }

            send_text_to_interactive(&mut bash_state, send_text).await
        }
        BashCommandAction::SendSpecials { send_specials } => {
            debug!("Processing SendSpecials action: {:?}", send_specials);
            if send_specials.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_specials cannot be empty".to_string(),
                ));
            }

            send_special_keys_to_interactive(&mut bash_state, send_specials).await
        }
        BashCommandAction::SendAscii { send_ascii } => {
            debug!("Processing SendAscii action: {:?}", send_ascii);
            if send_ascii.is_empty() {
                return Err(WinxError::CommandExecutionError(
                    "send_ascii cannot be empty".to_string(),
                ));
            }

            send_ascii_to_interactive(&mut bash_state, send_ascii).await
        }
    }
}

/// Send text to an interactive process
///
/// This function sends text input to a running interactive process.
/// It handles error cases robustly and provides detailed status information.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `text` - The text to send
///
/// # Returns
///
/// A Result containing the command output with status information
async fn send_text_to_interactive(bash_state: &mut BashState, text: &str) -> Result<String> {
    debug!("Sending text to interactive process: {}", text);

    // Validate input
    if text.trim().is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Cannot send empty text to interactive process".to_string(),
        ));
    }

    // Acquire lock with timeout and better error handling
    let bash_guard = match tokio::time::timeout(
        std::time::Duration::from_secs(5), // 5 second timeout for lock acquisition
        async {
            bash_state.interactive_bash.lock().map_err(|e| {
                WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
            })
        },
    )
    .await
    {
        Ok(Ok(guard)) => guard,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(WinxError::BashStateLockError(
                "Timed out waiting to acquire bash state lock".to_string(),
            ))
        }
    };

    // Cannot clone a reference to InteractiveBash, we need to check command state here
    let command_state = match bash_guard.as_ref() {
        Some(bash) => bash.command_state.clone(),
        None => return Err(WinxError::BashStateNotInitialized),
    };

    // Check if a command is running
    if let CommandState::Idle = command_state {
        return Err(WinxError::CommandExecutionError(
            "No command is currently running to send text to. Start a command first before sending input.".to_string()
        ));
    }

    // Get command info for better error messages
    let command_info = match &command_state {
        CommandState::Running {
            command,
            start_time,
        } => {
            let elapsed = start_time
                .elapsed()
                .unwrap_or_else(|_| std::time::Duration::from_secs(0));
            format!("'{}' (running for {:.2?})", command, elapsed)
        }
        _ => "unknown".to_string(),
    };

    // Drop guard and acquire mutable reference to bash state
    drop(bash_guard);
    let mut bash_guard = bash_state.interactive_bash.lock().map_err(|e| {
        WinxError::BashStateLockError(format!("Failed to lock bash state for writing: {}", e))
    })?;

    let bash = bash_guard
        .as_mut()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Send the text to the process
    if let Some(mut stdin) = bash.process.stdin.take() {
        // Try to send the text with detailed error handling
        let text_result = std::io::Write::write_all(&mut stdin, text.as_bytes());
        let newline_result = std::io::Write::write_all(&mut stdin, b"\n");
        let flush_result = stdin.flush();

        // Return stdin to the process regardless of write success
        bash.process.stdin = Some(stdin);

        // Check for errors in send operations
        if let Err(e) = text_result {
            return Err(WinxError::CommandExecutionError(format!(
                "Failed to write text to process {}: {}",
                command_info, e
            )));
        }

        if let Err(e) = newline_result {
            return Err(WinxError::CommandExecutionError(format!(
                "Failed to write newline to process {}: {}",
                command_info, e
            )));
        }

        if let Err(e) = flush_result {
            return Err(WinxError::CommandExecutionError(format!(
                "Failed to flush stdin for process {}: {}",
                command_info, e
            )));
        }

        // Read output after sending with error handling
        let result = bash.read_output(0.5);
        let (output, complete) = match result {
            Ok((output, complete)) => (output, complete),
            Err(e) => {
                return Err(WinxError::CommandExecutionError(format!(
                    "Failed to read output after sending text: {}",
                    e
                )))
            }
        };

        // Process the output through terminal emulation
        let rendered_output = crate::state::terminal::incremental_text(&output, "");

        // Check if the process is still alive
        let is_alive = bash.is_alive();

        // Add comprehensive status information
        let status = if !is_alive || complete {
            "process exited"
        } else {
            "still running"
        };

        // Get elapsed time if available
        let elapsed_info = if let CommandState::Running { start_time, .. } = &bash.command_state {
            if let Ok(elapsed) = start_time.elapsed() {
                format!(" (running for {:.2?})", elapsed)
            } else {
                "".to_string()
            }
        } else {
            "".to_string()
        };

        // Assemble final result with more details
        let final_result = format!(
            "Text sent: {}\n\n{}\n\n---\n\nstatus = {}{}\ncwd = {}\n",
            text,
            rendered_output,
            status,
            elapsed_info,
            bash_state.cwd.display()
        );

        Ok(final_result)
    } else {
        Err(WinxError::CommandExecutionError(format!(
            "Failed to get stdin for process {}. The process may not accept input.",
            command_info
        )))
    }
}

/// Send special keys to an interactive process
///
/// This function sends special keys to a running interactive process.
/// It includes robust error handling and detailed status information.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `keys` - The special keys to send
///
/// # Returns
///
/// A Result containing the command output with detailed status
async fn send_special_keys_to_interactive(
    bash_state: &mut BashState,
    keys: &[SpecialKey],
) -> Result<String> {
    debug!("Sending special keys to interactive process: {:?}", keys);

    // Validate input
    if keys.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Cannot send empty key list to interactive process".to_string(),
        ));
    }

    // Acquire lock with timeout and better error handling
    let bash_guard = match tokio::time::timeout(
        std::time::Duration::from_secs(5), // 5 second timeout for lock acquisition
        async {
            bash_state.interactive_bash.lock().map_err(|e| {
                WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
            })
        },
    )
    .await
    {
        Ok(Ok(guard)) => guard,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(WinxError::BashStateLockError(
                "Timed out waiting to acquire bash state lock".to_string(),
            ))
        }
    };

    // Cannot clone a reference to InteractiveBash, we need to check command state here
    let command_state = match bash_guard.as_ref() {
        Some(bash) => bash.command_state.clone(),
        None => return Err(WinxError::BashStateNotInitialized),
    };

    // Check if a command is running
    if let CommandState::Idle = command_state {
        return Err(WinxError::CommandExecutionError(
            "No command is currently running to send keys to. Start a command first before sending input.".to_string()
        ));
    }

    // Get command info for better error messages
    let command_info = match &command_state {
        CommandState::Running {
            command,
            start_time,
        } => {
            let elapsed = start_time
                .elapsed()
                .unwrap_or_else(|_| std::time::Duration::from_secs(0));
            format!("'{}' (running for {:.2?})", command, elapsed)
        }
        _ => "unknown".to_string(),
    };

    // Drop guard and acquire mutable reference to bash state
    drop(bash_guard);
    let mut bash_guard = bash_state.interactive_bash.lock().map_err(|e| {
        WinxError::BashStateLockError(format!("Failed to lock bash state for writing: {}", e))
    })?;

    let bash = bash_guard
        .as_mut()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Process each key
    let mut key_descriptions = Vec::new();
    let mut key_errors = Vec::new();

    for key in keys {
        // Handle special case for Ctrl+C to use the interrupt method
        if *key == SpecialKey::CtrlC {
            match bash.send_interrupt() {
                Ok(_) => key_descriptions.push("Ctrl+C (interrupt)".to_string()),
                Err(e) => {
                    key_errors.push(format!("Failed to send Ctrl+C interrupt: {}", e));
                    // Continue with other keys even if one fails
                }
            }
            continue;
        }

        // Send the key
        if let Some(mut stdin) = bash.process.stdin.take() {
            // Convert key to bytes
            let key_bytes = match key {
                SpecialKey::Enter => b"\n".to_vec(),
                SpecialKey::KeyUp => b"\x1b[A".to_vec(),
                SpecialKey::KeyDown => b"\x1b[B".to_vec(),
                SpecialKey::KeyLeft => b"\x1b[D".to_vec(),
                SpecialKey::KeyRight => b"\x1b[C".to_vec(),
                SpecialKey::CtrlD => b"\x04".to_vec(),
                _ => Vec::new(),
            };

            // Send the key with error handling
            let write_result = std::io::Write::write_all(&mut stdin, &key_bytes);
            let flush_result = stdin.flush();

            // Return stdin to the process regardless of write success
            bash.process.stdin = Some(stdin);

            // Process results
            if let Err(e) = write_result {
                key_errors.push(format!("Failed to send key {:?}: {}", key, e));
            } else if let Err(e) = flush_result {
                key_errors.push(format!(
                    "Failed to flush stdin after sending key {:?}: {}",
                    key, e
                ));
            } else {
                // Key was sent successfully
                key_descriptions.push(format!("{:?}", key));
            }
        } else {
            key_errors.push(format!(
                "Failed to get stdin for process when sending key {:?}",
                key
            ));
            // Try to continue with other keys even if this one failed
        }
    }

    // Check if we have any errors that would prevent continuing
    if key_descriptions.is_empty() && !key_errors.is_empty() {
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to send any keys to process {}. Errors: {}",
            command_info,
            key_errors.join("; ")
        )));
    }

    // Read output after sending all keys
    let result = bash.read_output(0.5);
    let (output, complete) = match result {
        Ok((output, complete)) => (output, complete),
        Err(e) => {
            return Err(WinxError::CommandExecutionError(format!(
                "Failed to read output after sending keys: {}",
                e
            )))
        }
    };

    // Process the output through terminal emulation
    let rendered_output = crate::state::terminal::incremental_text(&output, "");

    // Check if the process is still alive
    let is_alive = bash.is_alive();

    // Add comprehensive status information
    let status = if !is_alive || complete {
        "process exited"
    } else {
        "still running"
    };

    // Get elapsed time if available
    let elapsed_info = if let CommandState::Running { start_time, .. } = &bash.command_state {
        if let Ok(elapsed) = start_time.elapsed() {
            format!(" (running for {:.2?})", elapsed)
        } else {
            "".to_string()
        }
    } else {
        "".to_string()
    };

    // Include any errors in the output
    let error_info = if !key_errors.is_empty() {
        format!(
            "\n\nWarning: Some keys could not be sent: {}",
            key_errors.join("; ")
        )
    } else {
        "".to_string()
    };

    // Assemble final result with more details
    let final_result = format!(
        "Special keys sent: {}{}\n\n{}\n\n---\n\nstatus = {}{}\ncwd = {}\n",
        key_descriptions.join(", "),
        error_info,
        rendered_output,
        status,
        elapsed_info,
        bash_state.cwd.display()
    );

    Ok(final_result)
}

/// Send ASCII characters to an interactive process
///
/// This function sends ASCII characters to a running interactive process.
/// It includes robust error handling and detailed status information.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `ascii_codes` - The ASCII codes to send
///
/// # Returns
///
/// A Result containing the command output with detailed status
async fn send_ascii_to_interactive(
    bash_state: &mut BashState,
    ascii_codes: &[u8],
) -> Result<String> {
    debug!("Sending ASCII to interactive process: {:?}", ascii_codes);

    // Validate input
    if ascii_codes.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Cannot send empty ASCII code list to interactive process".to_string(),
        ));
    }

    // Acquire lock with timeout and better error handling
    let bash_guard = match tokio::time::timeout(
        std::time::Duration::from_secs(5), // 5 second timeout for lock acquisition
        async {
            bash_state.interactive_bash.lock().map_err(|e| {
                WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
            })
        },
    )
    .await
    {
        Ok(Ok(guard)) => guard,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(WinxError::BashStateLockError(
                "Timed out waiting to acquire bash state lock".to_string(),
            ))
        }
    };

    // Cannot clone a reference to InteractiveBash, we need to check command state here
    let command_state = match bash_guard.as_ref() {
        Some(bash) => bash.command_state.clone(),
        None => return Err(WinxError::BashStateNotInitialized),
    };

    // Check if a command is running
    if let CommandState::Idle = command_state {
        return Err(WinxError::CommandExecutionError(
            "No command is currently running to send ASCII to. Start a command first before sending input.".to_string()
        ));
    }

    // Get command info for better error messages
    let command_info = match &command_state {
        CommandState::Running {
            command,
            start_time,
        } => {
            let elapsed = start_time
                .elapsed()
                .unwrap_or_else(|_| std::time::Duration::from_secs(0));
            format!("'{}' (running for {:.2?})", command, elapsed)
        }
        _ => "unknown".to_string(),
    };

    // Drop guard and acquire mutable reference to bash state
    drop(bash_guard);
    let mut bash_guard = bash_state.interactive_bash.lock().map_err(|e| {
        WinxError::BashStateLockError(format!("Failed to lock bash state for writing: {}", e))
    })?;

    let bash = bash_guard
        .as_mut()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Track codes that were successfully sent
    let mut sent_codes = Vec::new();
    let mut send_errors = Vec::new();

    // Handle special case for Ctrl+C (ASCII 3)
    let contains_ctrl_c = ascii_codes.contains(&3);
    if contains_ctrl_c {
        match bash.send_interrupt() {
            Ok(_) => sent_codes.push(3),
            Err(e) => send_errors.push(format!("Failed to send Ctrl+C interrupt: {}", e)),
        }
    }

    // Send the ASCII codes
    if let Some(mut stdin) = bash.process.stdin.take() {
        for &code in ascii_codes {
            if code != 3 {
                // Skip Ctrl+C as it's handled by send_interrupt
                match std::io::Write::write_all(&mut stdin, &[code]) {
                    Ok(_) => sent_codes.push(code),
                    Err(e) => {
                        send_errors.push(format!("Failed to send ASCII code {}: {}", code, e))
                    }
                }
            }
        }

        // Flush stdin with error handling
        if let Err(e) = stdin.flush() {
            send_errors.push(format!("Failed to flush stdin: {}", e));
        }

        // Return stdin to the process
        bash.process.stdin = Some(stdin);
    } else {
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to get stdin for process {}. The process may not accept input.",
            command_info
        )));
    }

    // Check if we've sent anything successfully
    if sent_codes.is_empty() && !send_errors.is_empty() {
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to send any ASCII codes to process {}. Errors: {}",
            command_info,
            send_errors.join("; ")
        )));
    }

    // Read output after sending with error handling
    let result = bash.read_output(0.5);
    let (output, complete) = match result {
        Ok((output, complete)) => (output, complete),
        Err(e) => {
            return Err(WinxError::CommandExecutionError(format!(
                "Failed to read output after sending ASCII codes: {}",
                e
            )))
        }
    };

    // Process the output through terminal emulation
    let rendered_output = crate::state::terminal::incremental_text(&output, "");

    // Check if the process is still alive
    let is_alive = bash.is_alive();

    // Add comprehensive status information
    let status = if !is_alive || complete {
        "process exited"
    } else {
        "still running"
    };

    // Get elapsed time if available
    let elapsed_info = if let CommandState::Running { start_time, .. } = &bash.command_state {
        if let Ok(elapsed) = start_time.elapsed() {
            format!(" (running for {:.2?})", elapsed)
        } else {
            "".to_string()
        }
    } else {
        "".to_string()
    };

    // Format ASCII codes for display with enhanced readability
    let ascii_display = sent_codes
        .iter()
        .map(|code| match code {
            3 => "^C (Ctrl+C)".to_string(),
            4 => "^D (Ctrl+D)".to_string(),
            9 => "\\t (tab)".to_string(),
            10 => "\\n (newline)".to_string(),
            13 => "\\r (carriage return)".to_string(),
            27 => "ESC (escape)".to_string(),
            32..=126 => format!("{} ({})", code, *code as char),
            _ => format!("{} (0x{:02x})", code, code),
        })
        .collect::<Vec<_>>()
        .join(", ");

    // Include any errors in the output
    let error_info = if !send_errors.is_empty() {
        format!(
            "\n\nWarning: Some ASCII codes could not be sent: {}",
            send_errors.join("; ")
        )
    } else {
        "".to_string()
    };

    // Assemble final result with more details
    let final_result = format!(
        "ASCII codes sent: {}{}\n\n{}\n\n---\n\nstatus = {}{}\ncwd = {}\n",
        ascii_display,
        error_info,
        rendered_output,
        status,
        elapsed_info,
        bash_state.cwd.display()
    );

    Ok(final_result)
}

/// Execute an interactive command
///
/// This function executes a command in the interactive bash shell.
/// It includes robust error handling and detailed status information.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `command` - The command to execute
/// * `timeout` - Optional timeout in seconds
///
/// # Returns
///
/// A Result containing the command output with detailed status
async fn execute_interactive_command(
    bash_state: &mut BashState,
    command: &str,
    timeout: Option<f32>,
) -> Result<String> {
    debug!("Executing interactive command: {}", command);

    // Validate input
    if command.trim().is_empty() && timeout.is_none() {
        // This is effectively a status check, not a command execution
        return check_command_status(bash_state).await;
    }

    // Check if the command is a known background or long-running command that benefits from screen
    let needs_background = command.contains("watch ")
        || command.contains("top ")
        || command.contains("sleep ")
        || command.contains("while ")
        || command.contains("for ")
        || command.contains("tail -f ");

    if needs_background && !command.contains(" & ") && !command.ends_with(" &") {
        info!(
            "Command '{}' detected as potentially long-running, suggesting background execution",
            command
        );
        // Add a hint message to the output
        let result = bash_state
            .execute_interactive(command, timeout.unwrap_or(0.0))
            .await?;

        // Only add hint if command is still running
        if result.contains("status = still running") {
            let hint = "\nHint: This command appears to be long-running. Consider using screen or & to run it in the background.\n";
            return Ok(result.replace("---\n\n", &format!("{}---\n\n", hint)));
        }
        return Ok(result);
    }

    // Check for potentially problematic commands and provide warnings
    let dangerous_commands = ["rm -rf", "rm -r", "find / -delete", "> /dev/sda"];
    for dangerous in dangerous_commands {
        if command.contains(dangerous) {
            warn!("Potentially dangerous command detected: {}", command);
            // Execute but add a warning to the output
            let result = bash_state
                .execute_interactive(command, timeout.unwrap_or(0.0))
                .await?;
            let warning = format!(
                "\nWarning: The command '{}' contains potentially dangerous operations ({}). Make sure you understand the consequences.\n",
                command, dangerous
            );
            return Ok(result.replace("---\n\n", &format!("{}---\n\n", warning)));
        }
    }

    // For normal commands, use execute_interactive with improved timeout handling
    let effective_timeout = match timeout {
        Some(t) => {
            if t > 0.0 {
                t
            } else {
                30.0
            }
        } // Default 30s if invalid
        None => {
            // Estimate appropriate timeout based on command complexity
            if command.contains("|") || command.contains("&&") || command.contains(";") {
                // Complex command with pipes or multiple steps needs more time
                60.0
            } else if command.contains("find") || command.contains("grep -r") {
                // Search commands might take longer
                45.0
            } else {
                // Default for simple commands
                30.0
            }
        }
    };

    // Execute the command with enriched error handling
    match bash_state
        .execute_interactive(command, effective_timeout)
        .await
    {
        Ok(output) => {
            // Check for common error patterns in output and enhance with suggestions
            if output.contains("command not found") {
                let cmd_name = command.split_whitespace().next().unwrap_or(command);
                let suggestion = format!(
                    "\nThe command '{}' was not found. Consider installing it with package manager or checking PATH variable.\n",
                    cmd_name
                );
                return Ok(output.replace("---\n\n", &format!("{}---\n\n", suggestion)));
            } else if output.contains("permission denied") {
                let suggestion =
                    "\nPermission denied. Consider using sudo if appropriate for this command.\n";
                return Ok(output.replace("---\n\n", &format!("{}---\n\n", suggestion)));
            }

            Ok(output)
        }
        Err(e) => {
            // Convert anyhow::Error to WinxError
            let err: WinxError = e.into();

            // Enhance error messages with context
            match &err {
                WinxError::CommandExecutionError(msg) => {
                    if msg.contains("already running") {
                        // Already have a running command - provide more helpful info
                        Err(WinxError::CommandExecutionError(format!(
                            "{}. To interact with the running command, use send_text, send_specials, or status_check.",
                            msg
                        )))
                    } else {
                        Err(err)
                    }
                }
                _ => Err(err),
            }
        }
    }
}

/// Check the status of a running command
///
/// This function checks the status of a running command, providing detailed
/// information about the command, its output, and its current state.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
///
/// # Returns
///
/// A Result containing the command status with detailed information
async fn check_command_status(bash_state: &mut BashState) -> Result<String> {
    debug!("Checking command status");

    // We can't hold the lock across an await, so we need to extract all information
    // before any awaits happen
    let command_info: Option<(String, std::time::Duration, bool)>;

    // Use a scope to limit the lock lifetime
    {
        // Get command info from BashState
        let bash_guard = match bash_state.interactive_bash.lock() {
            Ok(guard) => guard,
            Err(e) => {
                return Err(WinxError::BashStateLockError(format!(
                    "Failed to lock bash state: {}",
                    e
                )))
            }
        };

        // Extract command info
        command_info = match bash_guard.as_ref() {
            Some(bash) => match &bash.command_state {
                CommandState::Running {
                    command,
                    start_time,
                } => {
                    let elapsed = start_time
                        .elapsed()
                        .unwrap_or_else(|_| std::time::Duration::from_secs(0));
                    Some((command.clone(), elapsed, true))
                }
                CommandState::Idle => {
                    // Check if we have a last command
                    if !bash.last_command.is_empty() {
                        Some((
                            bash.last_command.clone(),
                            std::time::Duration::from_secs(0),
                            false,
                        ))
                    } else {
                        None
                    }
                }
            },
            None => None,
        };

        // Drop the guard immediately to release the lock
        drop(bash_guard);
    }

    // Get background job count separately
    let bg_jobs = bash_state.check_background_jobs().unwrap_or_default();

    // Now we can safely use await
    let mut result = bash_state.execute_interactive("", 0.0).await?;

    // Enhance the status output with more details
    if let Some((cmd, elapsed, is_running)) = command_info {
        let status_line = if is_running {
            format!("Command '{}' is running (for {:.2?})", cmd, elapsed)
        } else {
            format!("Last command was '{}' (completed)", cmd)
        };

        // Add more detailed command info
        result = result.replace("---\n\n", &format!("---\n\n{}\n", status_line));
    }

    // Add background job information if available
    if bg_jobs > 0 {
        let bg_info = format!("Background jobs: {}\n", bg_jobs);
        result = result.replace("---\n\n", &format!("---\n\n{}", bg_info));
    }

    Ok(result)
}
