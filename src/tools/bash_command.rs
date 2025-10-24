//! Implementation of the BashCommand tool.
//!
//! This module provides the implementation for the BashCommand tool, which is used
//! to execute shell commands, check command status, and interact with the shell.

use anyhow::Context as AnyhowContext;
use rand::Rng;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, instrument, warn}; // Replace std::sync::Mutex

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, CommandState};
use crate::types::{BashCommand, BashCommandAction, SpecialKey};
use crate::utils::command_safety::{CommandContext, CommandSafety};

/// Maximum output length to prevent excessive responses
#[allow(dead_code)]
const MAX_OUTPUT_LENGTH: usize = 100_000;

/// Common status messages to avoid repeated allocations
const STATUS_SUCCESS: &str = "Command completed successfully";
const STATUS_RUNNING: &str = "running in background";
const STATUS_STILL_RUNNING: &str = "still running";
const STATUS_PROCESS_EXITED: &str = "process exited";
const STATUS_IDLE: &str = "no active command";
const STATUS_UNKNOWN: &str = "unknown";
const CWD_UNKNOWN: &str = "Unknown";

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
        STATUS_SUCCESS.to_string()
    } else {
        format!("Command failed with status: {}", output.status)
    };

    // Get current working directory
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| CWD_UNKNOWN.to_string());

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
        return Err(WinxError::CommandExecutionError {
            message: Arc::new("Failed to start screen session".to_string()),
        });
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
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| CWD_UNKNOWN.to_string());

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
        Current screen sessions:\n{}",
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
            command.replace("'", "'\\''"), // Escape single quotes
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
        return Err(WinxError::CommandExecutionError {
            message: Arc::new("Screen session not found".to_string()),
        });
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
        return Err(WinxError::CommandExecutionError {
            message: Arc::new("Failed to send input to screen session".to_string()),
        });
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

    if bash_command.chat_id.is_empty() {
        error!("Empty chat_id provided in BashCommand");
        return Err(WinxError::ChatIdMismatch {
            message: Arc::new(
                "Error: No saved bash state found for chat ID \"\". Please initialize first with this ID.".to_string(),
            ),
        });
    }

    let bash_state_guard = bash_state_arc.lock().await;
    let mut state = match &*bash_state_guard {
        Some(state) => state.clone(),
        None => {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        }
    };

    // Verify chat ID matches
    if bash_command.chat_id != state.current_chat_id {
        warn!(
            "Chat ID mismatch: expected {}, got {}",
            state.current_chat_id, bash_command.chat_id
        );
        return Err(WinxError::ChatIdMismatch {
            message: Arc::new(format!(
                "Error: No saved bash state found for chat ID \"{}\". Please initialize first with this ID.",
                bash_command.chat_id
            )),
        });
    }

    // Process the command based on action type
    match &bash_command.action_json {
        BashCommandAction::Command { command } => {
            debug!("Processing Command action: {}", command);

            // Enhanced command validation using WCGW-style mode checking
            if !state.is_command_allowed(command) {
                error!("Command '{}' not allowed in current mode", command);
                let violation_message =
                    state.get_mode_violation_message("command execution", command);
                return Err(WinxError::CommandNotAllowed {
                    message: Arc::new(violation_message),
                });
            }

            // WCGW-style command safety analysis
            let command_context = CommandContext::new(command);

            // Check if command should be allowed (interactive detection)
            if let Err(e) = command_context.should_allow_execution() {
                warn!("Command safety check failed for '{}': {}", command, e);

                // Add helpful message about alternatives
                let enhanced_error = match e {
                    WinxError::InteractiveCommandDetected { command: cmd } => {
                        WinxError::InteractiveCommandDetected {
                            command: Arc::new(format!(
                                "{} - Consider using non-interactive flags (e.g., git commit -m 'message') or automation tools",
                                cmd
                            )),
                        }
                    }
                    _ => e,
                };
                return Err(enhanced_error);
            }

            // Log safety warnings
            if !command_context.warnings.is_empty() {
                for warning in &command_context.warnings {
                    warn!("Command safety warning: {}", warning);
                }
            }

            // Check for screen command specifically to handle it specially
            if command.trim().starts_with("screen ") {
                info!("Detected screen command, using special handling");
                execute_screen_command(&mut state, command).await
            }
            // Check if command should run in background (contains &)
            else if command.contains(" & ")
                || command.ends_with(" &")
                || command.contains(" bg ")
                || command.contains(" &> ")
                || (command.contains(" > ") && command.contains(" < "))
            {
                info!("Command contains background operator, using background execution");
                execute_background_command(&mut state, command).await
            } else {
                // Normal command execution with WCGW-style timeout handling
                let timeout_seconds = bash_command
                    .wait_for_seconds
                    .or(Some(command_context.timeout.as_secs_f32()))
                    .unwrap_or(30.0); // Fallback to 30 seconds

                info!(
                    "Executing command '{}' with timeout: {:.1}s",
                    command, timeout_seconds
                );
                execute_interactive_command(&mut state, command, Some(timeout_seconds)).await
            }
        }
        BashCommandAction::StatusCheck { status_check: _ } => {
            debug!("Processing StatusCheck action");
            check_command_status(&mut state).await
        }
        BashCommandAction::SendText { send_text } => {
            debug!("Processing SendText action: {}", send_text);
            if send_text.is_empty() {
                return Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty text input".to_string()),
                });
            }

            send_text_to_interactive(&mut state, send_text).await
        }
        BashCommandAction::SendSpecials { send_specials } => {
            debug!("Processing SendSpecials action: {:?}", send_specials);
            if send_specials.is_empty() {
                return Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty special keys input".to_string()),
                });
            }

            send_special_keys_to_interactive(&mut state, send_specials).await
        }
        BashCommandAction::SendAscii { send_ascii } => {
            debug!("Processing SendAscii action: {:?}", send_ascii);
            if send_ascii.is_empty() {
                return Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty ASCII input".to_string()),
                });
            }

            send_ascii_to_interactive(&mut state, send_ascii).await
        }
    }
}
/// Generic helper function for sending input to interactive processes
///
/// This function handles the common pattern of validating input, acquiring locks,
/// checking command state, sending input, and formatting results.
///
/// # Arguments
///
/// * `bash_state` - The current bash state
/// * `validate_input` - Closure that validates the input and returns an error if invalid
/// * `send_input` - Async closure that sends the input to the bash process and returns (description, error_info)
/// * `format_result` - Closure that formats the final result string
///
/// # Returns
///
/// A Result containing the command output with status information
async fn send_input_to_interactive<F, G, H>(
    bash_state: &mut BashState,
    validate_input: F,
    send_input: G,
    format_result: H,
) -> Result<String>
where
    F: FnOnce() -> Result<()>,
    G: FnOnce(
        &mut crate::state::bash_state::InteractiveBash,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(String, String)>> + Send + '_>,
    >,
    H: FnOnce(String, String, String) -> String,
{
    // Validate input
    validate_input()?;

    // Acquire lock with timeout and better error handling
    let bash_guard = match tokio::time::timeout(
        std::time::Duration::from_secs(5), // 5 second timeout for lock acquisition
        async { bash_state.interactive_bash.lock().await },
    )
    .await
    {
        Ok(guard) => guard,
        Err(_) => {
            return Err(WinxError::BashStateLockError {
                message: Arc::new("Lock acquisition timed out".to_string()),
            });
        }
    };

    // Cannot clone a reference to InteractiveBash, we need to check command state here
    let command_state = match bash_guard.as_ref() {
        Some(bash) => bash.command_state.clone(),
        None => return Err(WinxError::BashStateNotInitialized),
    };

    // Check if a command is running
    if let CommandState::Idle = command_state {
        return Err(WinxError::CommandExecutionError {
            message: Arc::new("No command is currently running".to_string()),
        });
    }

    // Drop guard and acquire mutable reference to bash state
    drop(bash_guard);
    let mut bash_guard = bash_state.interactive_bash.lock().await;

    let bash = bash_guard
        .as_mut()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Send the input using the provided closure
    let (input_description, error_info) =
        send_input(bash)
            .await
            .map_err(|e| WinxError::CommandExecutionError {
                message: Arc::new(e.to_string()),
            })?;

    // Read output after sending with error handling
    let result = bash.read_output(0.5);
    let (output, complete) = match result {
        Ok((output, complete)) => (output, complete),
        Err(e) => {
            return Err(WinxError::CommandExecutionError {
                message: Arc::new(format!("Failed to read output: {}", e)),
            });
        }
    };

    // Process the output through terminal emulation
    let rendered_output = output.clone();

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

    // Format the final result
    let final_result = format_result(
        input_description,
        error_info,
        format!(
            "{}\n\n---\n\nstatus = {}{}\ncwd = {}\n",
            rendered_output,
            status,
            elapsed_info,
            bash_state.cwd.display()
        ),
    );

    Ok(final_result)
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
    send_input_to_interactive(
        bash_state,
        || {
            if text.trim().is_empty() {
                Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty text input".to_string()),
                })
            } else {
                Ok(())
            }
        },
        |bash| {
            let text = text.to_string(); // Clone the text to avoid lifetime issues
            Box::pin(async move {
                if let Some(mut stdin) = bash.process.stdin.take() {
                    let text_result = std::io::Write::write_all(&mut stdin, text.as_bytes());
                    let newline_result = std::io::Write::write_all(&mut stdin, b"\n");
                    let flush_result = stdin.flush();

                    bash.process.stdin = Some(stdin);

                    if let Err(e) = text_result {
                        return Err(WinxError::CommandExecutionError {
                            message: Arc::new(format!("Failed to write text: {}", e)),
                        });
                    }
                    if let Err(e) = newline_result {
                        return Err(WinxError::CommandExecutionError {
                            message: Arc::new(format!("Failed to write newline: {}", e)),
                        });
                    }
                    if let Err(e) = flush_result {
                        return Err(WinxError::CommandExecutionError {
                            message: Arc::new(format!("Failed to flush stdin: {}", e)),
                        });
                    }
                    Ok((format!("Text sent: {}", text), "".to_string()))
                } else {
                    Err(WinxError::CommandExecutionError {
                        message: Arc::new("No stdin available for the process".to_string()),
                    })
                }
            })
        },
        |input_desc, error_info, output| {
            if error_info.is_empty() {
                format!("{}\n\n{}", input_desc, output)
            } else {
                format!("{}\n\nWarning: {}\n\n{}", input_desc, error_info, output)
            }
        },
    )
    .await
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
    send_input_to_interactive(
        bash_state,
        || {
            if keys.is_empty() {
                Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty special keys input".to_string()),
                })
            } else {
                Ok(())
            }
        },
        |bash| {
            let keys = keys.to_vec(); // Clone the keys to avoid lifetime issues
            Box::pin(async move {
                // Process each key
                let mut key_descriptions = Vec::new();
                let mut key_errors = Vec::new();

                for key in keys {
                    // Handle special case for Ctrl+C to use the interrupt method
                    if key == SpecialKey::CtrlC {
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
                    return Err(WinxError::CommandExecutionError {
                        message: Arc::new("Failed to send any special keys".to_string()),
                    });
                }

                let input_desc = format!("Special keys sent: {}", key_descriptions.join(", "));
                let error_info = if !key_errors.is_empty() {
                    format!("Some keys could not be sent: {}", key_errors.join("; "))
                } else {
                    "".to_string()
                };

                Ok((input_desc, error_info))
            })
        },
        |input_desc, error_info, output| {
            if error_info.is_empty() {
                format!("{}\n\n{}", input_desc, output)
            } else {
                format!("{}\n\nWarning: {}\n\n{}", input_desc, error_info, output)
            }
        },
    )
    .await
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
    send_input_to_interactive(
        bash_state,
        || {
            if ascii_codes.is_empty() {
                Err(WinxError::CommandExecutionError {
                    message: Arc::new("Empty ASCII input".to_string()),
                })
            } else {
                Ok(())
            }
        },
        |bash| {
            let ascii_codes = ascii_codes.to_vec(); // Clone the codes to avoid lifetime issues
            Box::pin(async move {
                // Track codes that were successfully sent
                let mut sent_codes = Vec::new();
                let mut send_errors = Vec::new();

                // Handle special case for Ctrl+C (ASCII 3)
                let contains_ctrl_c = ascii_codes.contains(&3);
                if contains_ctrl_c {
                    match bash.send_interrupt() {
                        Ok(_) => sent_codes.push(3),
                        Err(e) => {
                            send_errors.push(format!("Failed to send Ctrl+C interrupt: {}", e))
                        }
                    }
                }

                // Send the ASCII codes
                if let Some(mut stdin) = bash.process.stdin.take() {
                    for &code in &ascii_codes {
                        if code != 3 {
                            // Skip Ctrl+C as it's handled by send_interrupt
                            match std::io::Write::write_all(&mut stdin, &[code]) {
                                Ok(_) => sent_codes.push(code),
                                Err(e) => send_errors
                                    .push(format!("Failed to send ASCII code {}: {}", code, e)),
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
                    return Err(WinxError::CommandExecutionError {
                        message: Arc::new("No stdin available for the process".to_string()),
                    });
                }

                // Check if we've sent anything successfully
                if sent_codes.is_empty() && !send_errors.is_empty() {
                    return Err(WinxError::CommandExecutionError {
                        message: Arc::new("Failed to send any ASCII codes".to_string()),
                    });
                }

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

                let input_desc = format!("ASCII codes sent: {}", ascii_display);
                let error_info = if !send_errors.is_empty() {
                    format!(
                        "Some ASCII codes could not be sent: {}",
                        send_errors.join("; ")
                    )
                } else {
                    "".to_string()
                };

                Ok((input_desc, error_info))
            })
        },
        |input_desc, error_info, output| {
            if error_info.is_empty() {
                format!("{}\n\n{}", input_desc, output)
            } else {
                format!("{}\n\nWarning: {}\n\n{}", input_desc, error_info, output)
            }
        },
    )
    .await
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

    // WCGW-style command safety validation
    if !command.trim().is_empty() {
        let command_safety = CommandSafety::new();

        // Check for command already running before validation
        {
            let bash_guard = bash_state.interactive_bash.lock().await;

            if let Some(ref bash) = *bash_guard
                && let CommandState::Running {
                    command: current_cmd,
                    start_time,
                } = &bash.command_state
            {
                let duration = start_time.elapsed().unwrap_or_default().as_secs_f64();
                return Err(WinxError::CommandAlreadyRunning {
                    current_command: Arc::new(current_cmd.clone()),
                    duration_seconds: duration,
                });
            }
        }

        // Validate command safety
        if command_safety.is_interactive(command) {
            warn!("Interactive command detected: {}", command);
            return Err(WinxError::InteractiveCommandDetected {
                command: Arc::new(format!(
                    "{} - Interactive commands may hang. Use non-interactive alternatives or flags",
                    command
                )),
            });
        }

        // Check for background commands and warn
        if command_safety.is_background_command(command) {
            info!("Background command detected: {}", command);
            // Continue execution but with modified timeout
        }

        // Get safety warnings and log them
        let warnings = command_safety.get_warnings(command);
        for warning in &warnings {
            debug!("Command safety warning: {}", warning);
        }
    }

    // Validate input
    if command.trim().is_empty() && timeout.is_none() {
        // This is effectively a status check, not a command execution
        return check_command_status(bash_state).await;
    }

    // Check for potential errors using the error predictor
    let mut potential_errors = Vec::new();
    match bash_state
        .error_predictor
        .predict_command_errors(command)
        .await
    {
        Ok(predictions) => {
            // Filter predictions with high confidence
            for prediction in predictions {
                if prediction.confidence > 0.8 {
                    debug!("High confidence error prediction: {:?}", prediction);
                    potential_errors.push(prediction);
                }
            }
        }
        Err(e) => {
            // Just log the error but continue execution
            warn!("Error prediction failed: {}", e);
        }
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

    // Add warnings for predicted errors
    if !potential_errors.is_empty() {
        // Format the warnings
        let mut warnings = String::new();
        warnings.push_str("\nPotential issues with this command:\n");

        for error in &potential_errors {
            warnings.push_str(&format!("- {}: {}\n", error.error_type, error.prevention));
        }

        // Add advice
        warnings.push_str("\nProceeding with execution, but be aware of these potential issues.\n");

        // Execute the command
        let result = bash_state
            .execute_interactive(command, timeout.unwrap_or(0.0))
            .await?;

        // Add the warnings to the output
        return Ok(result.replace("---\n\n", &format!("{}---\n\n", warnings)));
    }

    // WCGW-style intelligent timeout calculation
    let effective_timeout = match timeout {
        Some(t) => {
            if t > 0.0 {
                t
            } else {
                30.0
            }
        } // Default 30s if invalid
        None => {
            if !command.trim().is_empty() {
                // Use command safety analyzer for intelligent timeout
                let command_safety = CommandSafety::new();
                let recommended_timeout = command_safety.get_timeout(command);
                recommended_timeout.as_secs_f32()
            } else {
                // Status check should be quick
                5.0
            }
        }
    };

    debug!(
        "Using timeout of {:.1}s for command: {}",
        effective_timeout, command
    );

    // Record this command for pattern analysis
    if let Err(e) = bash_state
        .pattern_analyzer
        .record_command(command, bash_state.cwd.to_string_lossy().as_ref())
        .await
    {
        warn!("Failed to record command for pattern analysis: {}", e);
    }

    // Execute the command with WCGW-style timeout and error handling
    let start_execution_time = Instant::now();
    match bash_state
        .execute_interactive(command, effective_timeout)
        .await
    {
        Ok(output) => {
            let execution_duration = start_execution_time.elapsed();
            debug!("Command completed in {:.2?}", execution_duration);

            // Record successful command execution for pattern analysis
            if let Err(e) = bash_state
                .error_predictor
                .record_error(
                    "command_success",
                    "Command executed successfully",
                    Some(command),
                    None,
                    Some(&bash_state.cwd.to_string_lossy()),
                )
                .await
            {
                debug!("Failed to record successful command: {}", e);
            }

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
            let execution_duration = start_execution_time.elapsed();

            // Record command error for pattern analysis
            if let Err(record_err) = bash_state
                .error_predictor
                .record_error(
                    "command_execution_error",
                    &format!("{}", e),
                    Some(command),
                    None,
                    Some(&bash_state.cwd.to_string_lossy()),
                )
                .await
            {
                debug!("Failed to record command error: {}", record_err);
            }

            // Convert anyhow::Error to WinxError and enhance with WCGW-style context
            let mut err: WinxError = e.into();

            // Check if this might be a timeout
            if execution_duration.as_secs_f32() >= effective_timeout * 0.95 {
                err = WinxError::CommandTimeout {
                    command: Arc::new(command.to_string()),
                    timeout_seconds: effective_timeout as u64,
                };
            }

            // Enhance error messages with WCGW-style suggestions
            match &err {
                WinxError::CommandExecutionError { message } => {
                    if message.as_ref().contains("already running") {
                        // Already have a running command - provide more helpful info
                        Err(WinxError::CommandAlreadyRunning {
                            current_command: Arc::new(command.to_string()),
                            duration_seconds: execution_duration.as_secs_f64(),
                        })
                    } else {
                        Err(err)
                    }
                }
                WinxError::CommandTimeout { .. } => {
                    warn!(
                        "Command '{}' timed out after {:.1}s",
                        command, effective_timeout
                    );
                    Err(err)
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
        let bash_guard = bash_state.interactive_bash.lock().await;

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
    let bg_jobs = bash_state.check_background_jobs().await.unwrap_or(0);

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

// Define a local render_terminal_output function to handle missing functionality
fn render_terminal_output(text: &str) -> Vec<String> {
    text.lines().map(|line| line.to_string()).collect()
}
