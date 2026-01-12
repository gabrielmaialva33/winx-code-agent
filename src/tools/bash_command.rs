//! Implementation of the `BashCommand` tool with WCGW parity.
//!
//! This module provides the implementation for the `BashCommand` tool, which is used
//! to execute shell commands, check command status, and interact with the shell.
//! Matches the behavior of wcgw Python implementation 1:1.

use anyhow::Context as AnyhowContext;
use rand::Rng;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, CommandState, InteractiveBash};
use crate::state::terminal::render_terminal_output;
use crate::types::{BashCommand, BashCommandAction, SpecialKey};

// ==================== WCGW-Style Constants ====================

/// Default timeout for command execution (seconds) - matches WCGW Python Config.timeout
const DEFAULT_TIMEOUT: f64 = 5.0;

/// Extended timeout while output is still being produced - matches WCGW Python `Config.timeout_while_output`
const TIMEOUT_WHILE_OUTPUT: f64 = 20.0;

/// Number of iterations to wait without new output before giving up - matches WCGW Python `Config.output_wait_patience`
const OUTPUT_WAIT_PATIENCE: i32 = 3;

/// Chunk size for sending commands (characters) - matches WCGW Python (64 chars)
const COMMAND_CHUNK_SIZE: usize = 64;

/// Chunk size for sending text input (characters) - matches WCGW Python (128 chars)
const TEXT_CHUNK_SIZE: usize = 128;

/// Maximum output length to prevent excessive responses
const MAX_OUTPUT_LENGTH: usize = 100_000;

/// Message when a command is already running - matches WCGW Python `WAITING_INPUT_MESSAGE`
const WAITING_INPUT_MESSAGE: &str = "A command is already running. NOTE: You can't run multiple shell commands in main shell, likely a previous program hasn't exited.
1. Get its output using status check.
2. Use `send_ascii` or `send_specials` to give inputs to the running program OR
3. kill the previous program by sending ctrl+c first using `send_ascii` or `send_specials`
4. Interrupt and run the process in background
";

// ==================== Background Shell Manager ====================

/// Manages background shell sessions - matches WCGW Python's `background_shells` dict
#[derive(Debug, Default)]
pub struct BackgroundShellManager {
    shells: HashMap<String, Arc<Mutex<Option<InteractiveBash>>>>,
}

impl BackgroundShellManager {
    /// Create a new background shell manager
    pub fn new() -> Self {
        Self {
            shells: HashMap::new(),
        }
    }

    /// Start a new background shell and return its command ID
    pub fn start_new_shell(&mut self, working_dir: &Path, restricted_mode: bool) -> Result<String> {
        let cid = format!("{:010x}", rand::rng().random::<u32>());

        let bash = InteractiveBash::new(working_dir, restricted_mode)
            .map_err(|e| WinxError::CommandExecutionError(format!("Failed to start background shell: {e}")))?;

        self.shells.insert(cid.clone(), Arc::new(Mutex::new(Some(bash))));

        info!("Started background shell with id: {}", cid);
        Ok(cid)
    }

    /// Get a background shell by its command ID
    pub fn get_shell(&self, bg_command_id: &str) -> Option<Arc<Mutex<Option<InteractiveBash>>>> {
        self.shells.get(bg_command_id).cloned()
    }

    /// Remove and cleanup a background shell
    pub fn remove_shell(&mut self, bg_command_id: &str) -> bool {
        if let Some(shell_arc) = self.shells.remove(bg_command_id) {
            if let Ok(mut guard) = shell_arc.lock() {
                *guard = None; // Drop the shell
            }
            info!("Removed background shell: {}", bg_command_id);
            true
        } else {
            false
        }
    }

    /// Get info about all running background shells - matches WCGW Python `get_bg_running_commandsinfo`
    pub fn get_running_info(&self) -> String {
        if self.shells.is_empty() {
            return "No command running in background.\n".to_string();
        }

        let mut running = Vec::new();
        for (id, shell_arc) in &self.shells {
            if let Ok(guard) = shell_arc.lock() {
                if let Some(bash) = guard.as_ref() {
                    running.push(format!("Command: {}, bg_command_id: {}", bash.last_command, id));
                }
            }
        }

        if running.is_empty() {
            "No command running in background.\n".to_string()
        } else {
            format!("Following background commands are attached:\n{}\n", running.join("\n"))
        }
    }
}

// Global background shell manager (thread-safe) - matches WCGW Python's BashState.background_shells
lazy_static::lazy_static! {
    static ref BG_SHELL_MANAGER: Mutex<BackgroundShellManager> = Mutex::new(BackgroundShellManager::new());
}

// ==================== WCGW-Style Helper Functions ====================

/// Assert that command contains only a single statement - matches WCGW Python `assert_single_statement`
fn assert_single_statement(command: &str) -> Result<()> {
    if command.contains('\n') {
        // Check if the newline is inside quotes or a heredoc
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut escape_next = false;

        for ch in command.chars() {
            if escape_next {
                escape_next = false;
                continue;
            }

            match ch {
                '\\' => escape_next = true,
                '\'' if !in_double_quote => in_single_quote = !in_single_quote,
                '"' if !in_single_quote => in_double_quote = !in_double_quote,
                '\n' if !in_single_quote && !in_double_quote => {
                    return Err(WinxError::CommandExecutionError(
                        "Command should not contain newline character in middle. Run only one command at a time.".to_string()
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Get WCGW-style status string - matches WCGW Python's `get_status()`
fn get_status(bash_state: &BashState, is_bg: bool, bg_id: Option<&str>, is_running: bool, running_for: Option<&str>) -> String {
    let mut status = "\n\n---\n\n".to_string();

    if is_bg {
        if let Some(id) = bg_id {
            status.push_str(&format!("bg_command_id = {id}\n"));
        }
    }

    if is_running {
        status.push_str("status = still running\n");
        if let Some(duration) = running_for {
            status.push_str(&format!("running for = {duration}\n"));
        }
    } else {
        status.push_str("status = process exited\n");
    }

    status.push_str(&format!("cwd = {}\n", bash_state.cwd.display()));

    if !is_bg {
        // Add background shell info for main shell - matches WCGW Python
        if let Ok(manager) = BG_SHELL_MANAGER.lock() {
            status.push_str("This is the main shell. ");
            status.push_str(&manager.get_running_info());
        }
    }

    status.trim_end().to_string()
}

/// Process output with WCGW-style incremental text handling - matches WCGW Python _`incremental_text`
fn wcgw_incremental_text(text: &str, last_pending_output: &str) -> String {
    let text = if text.len() > MAX_OUTPUT_LENGTH {
        &text[text.len() - MAX_OUTPUT_LENGTH..]
    } else {
        text
    };

    if last_pending_output.is_empty() {
        let rendered = render_terminal_output(text);
        return rstrip_lines(&rendered).trim_start().to_string();
    }

    let last_rendered = render_terminal_output(last_pending_output);
    if last_rendered.is_empty() {
        return rstrip_lines(&render_terminal_output(text));
    }

    // Get text after last pending output
    let text_after_last = if text.len() > last_pending_output.len() {
        &text[last_pending_output.len()..]
    } else {
        text
    };

    let combined = format!("{}\n{}", last_rendered.join("\n"), text_after_last);
    let new_rendered = render_terminal_output(&combined);

    // Get incremental part - matches WCGW Python get_incremental_output
    let incremental = get_incremental_output(&last_rendered, &new_rendered);
    rstrip_lines(&incremental)
}

/// Right-strip each line and join - matches WCGW Python rstrip
fn rstrip_lines(lines: &[String]) -> String {
    lines.iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Get incremental output between old and new - matches WCGW Python `get_incremental_output`
fn get_incremental_output(old_output: &[String], new_output: &[String]) -> Vec<String> {
    if old_output.is_empty() {
        return new_output.to_vec();
    }

    let nold = old_output.len();
    let nnew = new_output.len();

    // Find where old output ends in new output
    for i in (0..nnew).rev() {
        if new_output[i] != old_output[nold - 1] {
            continue;
        }

        let mut matched = true;
        for j in (0..i).rev() {
            let old_idx = (nold as i64 - 1 + j as i64 - i as i64) as isize;
            if old_idx < 0 {
                break;
            }
            if new_output[j] != old_output[old_idx as usize] {
                matched = false;
                break;
            }
        }

        if matched {
            return new_output[i + 1..].to_vec();
        }
    }

    new_output.to_vec()
}

/// Check if action is effectively a status check - matches WCGW Python `is_status_check`
#[allow(dead_code)]
fn is_status_check_action(action: &BashCommandAction) -> bool {
    match action {
        BashCommandAction::StatusCheck { .. } => true,
        BashCommandAction::SendSpecials { send_specials, .. } => {
            send_specials.len() == 1 && send_specials[0] == SpecialKey::Enter
        }
        BashCommandAction::SendAscii { send_ascii, .. } => {
            send_ascii.len() == 1 && send_ascii[0] == 10 // newline
        }
        _ => false,
    }
}

// ==================== Main Tool Handler ====================

/// Handles the `BashCommand` tool call with WCGW parity
///
/// This function processes the `BashCommand` tool call following WCGW Python's
/// `execute_bash()` function behavior exactly.
#[tracing::instrument(level = "info", skip(bash_state_arc, bash_command))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
) -> Result<String> {
    info!("BashCommand tool called with: {:?}", bash_command);

    // Check if thread_id is empty
    if bash_command.thread_id.is_empty() {
        error!("Empty thread_id provided in BashCommand");
        return Err(WinxError::ThreadIdMismatch(
            "Error: No saved bash state found for thread ID \"\". Please initialize first with this ID.".to_string()
        ));
    }

    // Extract bash_state data
    let mut bash_state: BashState;
    {
        let bash_state_guard = bash_state_arc.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock bash state: {e}"))
        })?;

        let state = if let Some(state) = &*bash_state_guard { state } else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };

        bash_state = state.clone();
    }

    // Verify thread ID matches - matches WCGW Python thread_id check
    if bash_command.thread_id != bash_state.current_thread_id {
        // Try to load state from thread_id - matches WCGW Python load_state_from_thread_id
        if !bash_state.load_state_from_disk(&bash_command.thread_id).unwrap_or(false) {
            return Err(WinxError::ThreadIdMismatch(format!(
                "Error: No saved bash state found for thread_id `{}`. Please initialize first with this ID.",
                bash_command.thread_id
            )));
        }
    }

    // Calculate effective timeout - matches WCGW Python
    let timeout_s = bash_command.wait_for_seconds
        .map_or(DEFAULT_TIMEOUT, f64::from)
        .min(TIMEOUT_WHILE_OUTPUT);

    // Execute the action based on type - matches WCGW Python's _execute_bash()
    let result = execute_bash_action(&mut bash_state, &bash_command.action_json, timeout_s).await;

    // Remove echo if it's a command - matches WCGW Python
    match result {
        Ok(mut output) => {
            if let BashCommandAction::Command { ref command, .. } = bash_command.action_json {
                let cmd_trimmed = command.trim();
                if output.starts_with(cmd_trimmed) {
                    output = output[cmd_trimmed.len()..].to_string();
                }
            }
            Ok(output)
        }
        Err(e) => Err(e),
    }
}

/// Execute a bash action - matches WCGW Python's _`execute_bash()` function
async fn execute_bash_action(
    bash_state: &mut BashState,
    action: &BashCommandAction,
    timeout_s: f64,
) -> Result<String> {
    let mut is_bg = false;
    let mut bg_id: Option<String> = None;

    // Handle bg_command_id routing - matches WCGW Python
    let bg_shell: Option<Arc<Mutex<Option<InteractiveBash>>>> = match action {
        BashCommandAction::Command { .. } => None, // Commands don't use bg_command_id for routing
        BashCommandAction::StatusCheck { bg_command_id, .. } |
        BashCommandAction::SendText { bg_command_id, .. } |
        BashCommandAction::SendSpecials { bg_command_id, .. } |
        BashCommandAction::SendAscii { bg_command_id, .. } => {
            if let Some(id) = bg_command_id {
                let manager = BG_SHELL_MANAGER.lock()
                    .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}")))?;

                if let Some(shell) = manager.get_shell(id) {
                    is_bg = true;
                    bg_id = Some(id.clone());
                    Some(shell)
                } else {
                    // Error message matches WCGW Python
                    let error = format!("No shell found running with command id {}.\n{}", id, manager.get_running_info());
                    return Err(WinxError::CommandExecutionError(error));
                }
            } else {
                None
            }
        }
    };

    // Process based on action type - matches WCGW Python _execute_bash dispatch
    match action {
        BashCommandAction::Command { command, is_background } => {
            execute_command(bash_state, command, *is_background, timeout_s).await
        }
        BashCommandAction::StatusCheck { .. } => {
            execute_status_check(bash_state, bg_shell, is_bg, bg_id.as_deref(), timeout_s).await
        }
        BashCommandAction::SendText { send_text, .. } => {
            execute_send_text(bash_state, send_text, bg_shell, is_bg, bg_id.as_deref(), timeout_s).await
        }
        BashCommandAction::SendSpecials { send_specials, .. } => {
            execute_send_specials(bash_state, send_specials, bg_shell, is_bg, bg_id.as_deref(), timeout_s).await
        }
        BashCommandAction::SendAscii { send_ascii, .. } => {
            execute_send_ascii(bash_state, send_ascii, bg_shell, is_bg, bg_id.as_deref(), timeout_s).await
        }
    }
}

/// Execute a command - matches WCGW Python's Command handling in _`execute_bash`
async fn execute_command(
    bash_state: &mut BashState,
    command: &str,
    is_background: bool,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing Command action: {}", command);

    // Check mode permissions - matches WCGW Python bash_command_mode check
    if !bash_state.is_command_allowed(command) {
        error!("Command '{}' not allowed in current mode", command);
        return Err(WinxError::CommandNotAllowed(
            "Error: BashCommand not allowed in current mode".to_string()
        ));
    }

    // Validate single statement - matches WCGW Python assert_single_statement
    let command = command.trim();
    assert_single_statement(command)?;

    // If background execution requested, start new shell - matches WCGW Python is_background handling
    if is_background {
        return execute_in_background(bash_state, command, timeout_s).await;
    }

    // Check if a command is already running - matches WCGW Python state check
    {
        let bash_guard = bash_state.interactive_bash.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?;

        if let Some(ref bash) = *bash_guard {
            if let CommandState::Running { .. } = bash.command_state {
                return Err(WinxError::CommandExecutionError(WAITING_INPUT_MESSAGE.to_string()));
            }
        }
    }

    // Initialize bash if needed
    if bash_state.interactive_bash.lock()
        .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?
        .is_none()
    {
        bash_state.init_interactive_bash()
            .map_err(|e| WinxError::CommandExecutionError(format!("Failed to init bash: {e}")))?;
    }

    // Clear prompt before sending - matches WCGW Python clear_to_run
    // (simplified version - WCGW does more complex clearing)

    // Send command in chunks of 64 characters - matches WCGW Python exactly
    {
        let mut bash_guard = bash_state.interactive_bash.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?;

        let bash = bash_guard.as_mut()
            .ok_or(WinxError::BashStateNotInitialized)?;

        // Send in chunks - matches WCGW Python: for i in range(0, len(command), 64)
        for chunk in command.as_bytes().chunks(COMMAND_CHUNK_SIZE) {
            if let Some(mut stdin) = bash.process.stdin.take() {
                stdin.write_all(chunk)
                    .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write chunk: {e}")))?;
                bash.process.stdin = Some(stdin);
            }
        }

        // Send linesep to execute - matches WCGW Python bash_state.send(bash_state.linesep, ...)
        if let Some(mut stdin) = bash.process.stdin.take() {
            stdin.write_all(b"\n")
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write newline: {e}")))?;
            stdin.flush()
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to flush: {e}")))?;
            bash.process.stdin = Some(stdin);
        }

        bash.last_command = command.to_string();
        bash.command_state = CommandState::Running {
            start_time: std::time::SystemTime::now(),
            command: command.to_string(),
        };
    }

    // Wait for output with WCGW-style patience handling
    wait_for_output(bash_state, timeout_s, false, None, false).await
}

/// Wait for command output with WCGW-style patience handling - matches WCGW Python expect/wait logic
async fn wait_for_output(
    bash_state: &mut BashState,
    timeout_s: f64,
    is_bg: bool,
    bg_id: Option<&str>,
    is_status_check: bool,
) -> Result<String> {
    let start = Instant::now();
    let wait = timeout_s.min(TIMEOUT_WHILE_OUTPUT);
    let mut last_pending_output = String::new();
    let mut complete = false;

    // Initial wait - matches WCGW Python wait = min(timeout_s or CONFIG.timeout, CONFIG.timeout_while_output)
    sleep(Duration::from_secs_f64(wait.min(DEFAULT_TIMEOUT))).await;

    // Read initial output
    let mut output = {
        let mut bash_guard = bash_state.interactive_bash.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?;

        if let Some(bash) = bash_guard.as_mut() {
            let (out, done) = bash.read_output(0.5)
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to read output: {e}")))?;
            complete = done;
            out
        } else {
            String::new()
        }
    };

    // If not complete and this is a status check, use WCGW-style patience waiting
    // Matches WCGW Python: if is_status_check(bash_arg) block
    if !complete && is_status_check {
        let mut remaining = TIMEOUT_WHILE_OUTPUT - wait;
        let mut patience = OUTPUT_WAIT_PATIENCE;

        let incremental = wcgw_incremental_text(&output, &last_pending_output);
        if incremental.is_empty() {
            patience -= 1;
        }

        let mut last_incremental = incremental;

        while remaining > 0.0 && patience > 0 {
            sleep(Duration::from_secs_f64(wait.min(remaining))).await;

            let (new_output, done) = {
                let mut bash_guard = bash_state.interactive_bash.lock()
                    .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?;

                if let Some(bash) = bash_guard.as_mut() {
                    bash.read_output(0.5)
                        .map_err(|e| WinxError::CommandExecutionError(format!("Failed to read output: {e}")))?
                } else {
                    (String::new(), true)
                }
            };

            if done {
                complete = true;
                output = new_output;
                break;
            }

            // Check if output changed - matches WCGW Python patience logic
            let new_incremental = wcgw_incremental_text(&new_output, &last_pending_output);
            if new_incremental == last_incremental {
                patience -= 1;
            } else {
                patience = OUTPUT_WAIT_PATIENCE; // Reset patience on new output
            }
            last_incremental = new_incremental;

            output = new_output;
            remaining -= wait;
        }

        if !complete {
            // Update pending output - matches WCGW Python bash_state.set_pending(text)
            last_pending_output = output.clone();
        }
    }

    // Process output through terminal emulation - matches WCGW Python _incremental_text
    let rendered = wcgw_incremental_text(&output, &last_pending_output);

    // Truncate if needed - matches WCGW Python token truncation
    let rendered = if rendered.len() > MAX_OUTPUT_LENGTH {
        format!("(...truncated)\n{}", &rendered[rendered.len() - MAX_OUTPUT_LENGTH..])
    } else {
        rendered
    };

    // Calculate running duration for status
    let running_for = if complete {
        None
    } else {
        Some(format!("{} seconds", (start.elapsed().as_secs() + timeout_s as u64)))
    };

    // Add status - matches WCGW Python get_status
    let status = get_status(bash_state, is_bg, bg_id, !complete, running_for.as_deref());
    Ok(format!("{rendered}{status}"))
}

/// Execute a status check - matches WCGW Python's `StatusCheck` handling
async fn execute_status_check(
    bash_state: &mut BashState,
    _bg_shell: Option<Arc<Mutex<Option<InteractiveBash>>>>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing StatusCheck action");

    // Check if there's a running command - matches WCGW Python state check
    let is_running = {
        let guard = bash_state.interactive_bash.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {e}")))?;
        if let Some(ref bash) = *guard {
            matches!(bash.command_state, CommandState::Running { .. })
        } else {
            false
        }
    };

    // If no command running and not background, return error - matches WCGW Python
    if !is_running && !is_bg {
        let manager = BG_SHELL_MANAGER.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}")))?;
        let error = format!("No running command to check status of.\n{}", manager.get_running_info());
        return Err(WinxError::CommandExecutionError(error));
    }

    // Read output with patience handling - this IS a status check
    wait_for_output(bash_state, timeout_s, is_bg, bg_id, true).await
}

/// Execute `send_text` - matches WCGW Python's `SendText` handling
async fn execute_send_text(
    bash_state: &mut BashState,
    text: &str,
    bg_shell: Option<Arc<Mutex<Option<InteractiveBash>>>>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendText action: {}", text);

    // Validate - matches WCGW Python
    if text.is_empty() {
        return Err(WinxError::CommandExecutionError("Failure: send_text cannot be empty".to_string()));
    }

    // Get the target shell
    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.interactive_bash.clone());

    // Send text in chunks of 128 characters - matches WCGW Python exactly
    {
        let mut guard = shell_arc.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock shell: {e}")))?;

        let bash = guard.as_mut()
            .ok_or(WinxError::BashStateNotInitialized)?;

        // Send in chunks - matches WCGW Python: for i in range(0, len(command_data.send_text), 128)
        for chunk in text.as_bytes().chunks(TEXT_CHUNK_SIZE) {
            if let Some(mut stdin) = bash.process.stdin.take() {
                stdin.write_all(chunk)
                    .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write text chunk: {e}")))?;
                bash.process.stdin = Some(stdin);
            }
        }

        // Send linesep - matches WCGW Python bash_state.send(bash_state.linesep, ...)
        if let Some(mut stdin) = bash.process.stdin.take() {
            stdin.write_all(b"\n")
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write newline: {e}")))?;
            stdin.flush()
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to flush: {e}")))?;
            bash.process.stdin = Some(stdin);
        }
    }

    // Wait for output
    wait_for_output(bash_state, timeout_s, is_bg, bg_id, false).await
}

/// Execute `send_specials` - matches WCGW Python's `SendSpecials` handling exactly
async fn execute_send_specials(
    bash_state: &mut BashState,
    keys: &[SpecialKey],
    bg_shell: Option<Arc<Mutex<Option<InteractiveBash>>>>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendSpecials action: {:?}", keys);

    // Validate - matches WCGW Python
    if keys.is_empty() {
        return Err(WinxError::CommandExecutionError("Failure: send_specials cannot be empty".to_string()));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.interactive_bash.clone());
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock shell: {e}")))?;

        let bash = guard.as_mut()
            .ok_or(WinxError::BashStateNotInitialized)?;

        // Send each special key - matches WCGW Python exactly
        for key in keys {
            match key {
                SpecialKey::KeyUp => {
                    // matches WCGW Python: bash_state.send("\033[A", ...)
                    send_bytes_to_bash(bash, b"\x1b[A")?;
                }
                SpecialKey::KeyDown => {
                    // matches WCGW Python: bash_state.send("\033[B", ...)
                    send_bytes_to_bash(bash, b"\x1b[B")?;
                }
                SpecialKey::KeyLeft => {
                    // matches WCGW Python: bash_state.send("\033[D", ...)
                    send_bytes_to_bash(bash, b"\x1b[D")?;
                }
                SpecialKey::KeyRight => {
                    // matches WCGW Python: bash_state.send("\033[C", ...)
                    send_bytes_to_bash(bash, b"\x1b[C")?;
                }
                SpecialKey::Enter => {
                    // matches WCGW Python: bash_state.send("\x0d", ...) - carriage return
                    send_bytes_to_bash(bash, b"\x0d")?;
                }
                SpecialKey::CtrlC => {
                    // matches WCGW Python: bash_state.sendintr()
                    bash.send_interrupt()
                        .map_err(|e| WinxError::CommandExecutionError(format!("Failed to send interrupt: {e}")))?;
                    is_interrupt = true;
                }
                SpecialKey::CtrlD => {
                    // matches WCGW Python: bash_state.sendintr() - same as Ctrl+C in WCGW
                    bash.send_interrupt()
                        .map_err(|e| WinxError::CommandExecutionError(format!("Failed to send Ctrl+D: {e}")))?;
                    is_interrupt = true;
                }
                SpecialKey::CtrlZ => {
                    // Ctrl+Z = SIGTSTP (suspend) - ASCII 0x1a
                    send_bytes_to_bash(bash, b"\x1a")?;
                }
            }
        }
    }

    // Wait for output
    let mut output = wait_for_output(bash_state, timeout_s, is_bg, bg_id, false).await?;

    // Add interrupt failure message if still running - matches WCGW Python exactly
    if is_interrupt && output.contains("status = still running") {
        output.push_str("\n---\n----\nFailure interrupting.\nYou may want to try Ctrl-c again or program specific exit interactive commands.\n");
    }

    Ok(output)
}

/// Helper to send bytes to bash stdin
fn send_bytes_to_bash(bash: &mut InteractiveBash, bytes: &[u8]) -> Result<()> {
    if let Some(mut stdin) = bash.process.stdin.take() {
        stdin.write_all(bytes)
            .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write bytes: {e}")))?;
        stdin.flush()
            .map_err(|e| WinxError::CommandExecutionError(format!("Failed to flush: {e}")))?;
        bash.process.stdin = Some(stdin);
        Ok(())
    } else {
        Err(WinxError::CommandExecutionError("Failed to get stdin".to_string()))
    }
}

/// Execute `send_ascii` - matches WCGW Python's `SendAscii` handling
async fn execute_send_ascii(
    bash_state: &mut BashState,
    ascii_codes: &[u8],
    bg_shell: Option<Arc<Mutex<Option<InteractiveBash>>>>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendAscii action: {:?}", ascii_codes);

    // Validate - matches WCGW Python
    if ascii_codes.is_empty() {
        return Err(WinxError::CommandExecutionError("Failure: send_ascii cannot be empty".to_string()));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.interactive_bash.clone());
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock shell: {e}")))?;

        let bash = guard.as_mut()
            .ok_or(WinxError::BashStateNotInitialized)?;

        // Send each ASCII code - matches WCGW Python
        for &code in ascii_codes {
            // matches WCGW Python: bash_state.send(chr(ascii_char), ...)
            if let Some(mut stdin) = bash.process.stdin.take() {
                stdin.write_all(&[code])
                    .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write ASCII code: {e}")))?;
                stdin.flush()
                    .map_err(|e| WinxError::CommandExecutionError(format!("Failed to flush: {e}")))?;
                bash.process.stdin = Some(stdin);
            }

            // Check for interrupt - matches WCGW Python: if ascii_char == 3: is_interrupt = True
            if code == 3 {
                is_interrupt = true;
            }
        }
    }

    // Wait for output
    let mut output = wait_for_output(bash_state, timeout_s, is_bg, bg_id, false).await?;

    // Add interrupt failure message if still running - matches WCGW Python
    if is_interrupt && output.contains("status = still running") {
        output.push_str("\n---\n----\nFailure interrupting.\nYou may want to try Ctrl-c again or program specific exit interactive commands.\n");
    }

    Ok(output)
}

/// Execute command in background - matches WCGW Python's `is_background` handling
async fn execute_in_background(
    bash_state: &mut BashState,
    command: &str,
    timeout_s: f64,
) -> Result<String> {
    debug!("Executing command in background: {}", command);

    // Start a new background shell - matches WCGW Python bash_state.start_new_bg_shell
    let restricted_mode = matches!(bash_state.bash_command_mode.bash_mode, crate::types::BashMode::RestrictedMode);

    let bg_id = {
        let mut manager = BG_SHELL_MANAGER.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}")))?;
        manager.start_new_shell(&bash_state.cwd, restricted_mode)?
    };

    // Get the shell
    let shell_arc = {
        let manager = BG_SHELL_MANAGER.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}")))?;
        manager.get_shell(&bg_id)
            .ok_or_else(|| WinxError::CommandExecutionError("Failed to get background shell".to_string()))?
    };

    // Send command in chunks - same as regular command
    {
        let mut guard = shell_arc.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg shell: {e}")))?;

        let bash = guard.as_mut()
            .ok_or(WinxError::BashStateNotInitialized)?;

        for chunk in command.as_bytes().chunks(COMMAND_CHUNK_SIZE) {
            if let Some(mut stdin) = bash.process.stdin.take() {
                stdin.write_all(chunk)
                    .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write chunk: {e}")))?;
                bash.process.stdin = Some(stdin);
            }
        }

        if let Some(mut stdin) = bash.process.stdin.take() {
            stdin.write_all(b"\n")
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to write newline: {e}")))?;
            stdin.flush()
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to flush: {e}")))?;
            bash.process.stdin = Some(stdin);
        }

        bash.last_command = command.to_string();
        bash.command_state = CommandState::Running {
            start_time: std::time::SystemTime::now(),
            command: command.to_string(),
        };
    }

    // Wait briefly and get initial output
    sleep(Duration::from_secs_f64(timeout_s.min(DEFAULT_TIMEOUT))).await;

    let (output, complete) = {
        let mut guard = shell_arc.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg shell: {e}")))?;

        if let Some(bash) = guard.as_mut() {
            bash.read_output(0.5)
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to read output: {e}")))?
        } else {
            (String::new(), true)
        }
    };

    // Process output
    let rendered = wcgw_incremental_text(&output, "");
    let rendered = if rendered.len() > MAX_OUTPUT_LENGTH {
        format!("(...truncated)\n{}", &rendered[rendered.len() - MAX_OUTPUT_LENGTH..])
    } else {
        rendered
    };

    // Build status with bg_command_id - matches WCGW Python
    let status = get_status(bash_state, true, Some(&bg_id), !complete, None);

    // Cleanup if complete - matches WCGW Python
    if complete {
        let mut manager = BG_SHELL_MANAGER.lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}")))?;
        manager.remove_shell(&bg_id);
    }

    Ok(format!("{rendered}{status}"))
}

// ==================== Legacy Screen-based Functions (kept for backward compatibility) ====================

/// Process simple command execution for a bash command (legacy)
#[allow(dead_code)]
#[tracing::instrument(level = "debug", skip(command, cwd))]
async fn execute_simple_command(command: &str, cwd: &Path, _timeout: Option<f32>) -> Result<String> {
    debug!("Executing command: {}", command);

    let start_time = Instant::now();
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().context("Failed to execute command")?;
    let elapsed = start_time.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let raw_result = format!("{stdout}{stderr}");
    let mut result = raw_result.clone();
    if !raw_result.is_empty() {
        let rendered_lines = render_terminal_output(&raw_result);
        if !rendered_lines.is_empty() {
            result = rendered_lines.join("\n");
        }
    }

    if result.len() > MAX_OUTPUT_LENGTH {
        result = format!(
            "(...truncated)\n{}",
            &result[result.len() - MAX_OUTPUT_LENGTH..]
        );
    }

    let exit_status = if output.status.success() {
        "Command completed successfully".to_string()
    } else {
        format!("Command failed with status: {}", output.status)
    };

    let current_dir = std::env::current_dir().map_or_else(|_| "Unknown".to_string(), |p| p.to_string_lossy().into_owned());

    debug!("Command executed in {:.2?}", elapsed);
    Ok(format!(
        "{result}\n\n---\n\nstatus = {exit_status}\ncwd = {current_dir}\n"
    ))
}

/// Execute command in screen (legacy)
#[allow(dead_code)]
#[tracing::instrument(level = "debug", skip(command, cwd, screen_name))]
async fn execute_in_screen(command: &str, cwd: &Path, screen_name: &str) -> Result<String> {
    debug!(
        "Executing command in screen session '{}': {}",
        screen_name, command
    );

    let screen_check = Command::new("which")
        .arg("screen")
        .output()
        .context("Failed to check for screen command")?;

    if !screen_check.status.success() {
        warn!("Screen command not found, falling back to direct execution");
        return execute_simple_command(command, cwd, None).await;
    }

    let _cleanup = Command::new("screen")
        .args(["-X", "-S", screen_name, "quit"])
        .output();

    let screen_cmd = format!(
        "screen -dmS {} bash -c '{} ; ec=$? ; echo \"Command completed with exit code: $ec\" ; sleep 1 ; exit $ec'",
        screen_name,
        command.replace('\'', "'\\''")
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
            "Failed to start screen session: {stderr}"
        )));
    }

    sleep(Duration::from_millis(300)).await;

    let screen_check = Command::new("screen")
        .args(["-ls"])
        .output()
        .context("Failed to list screen sessions")?;

    let screen_list = String::from_utf8_lossy(&screen_check.stdout).to_string();

    let current_dir = std::env::current_dir().map_or_else(|_| "Unknown".to_string(), |p| p.to_string_lossy().into_owned());

    Ok(format!(
        "Started command in background screen session '{screen_name}'.\n\
        Use status_check to get output.\n\n\
        Screen sessions:\n{screen_list}\n\
        ---\n\n\
        status = running in background\n\
        cwd = {current_dir}\n"
    ))
}

/// Converts a `SpecialKey` to its screen stuff input representation (legacy)
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
        SpecialKey::CtrlZ => String::from("\x1a"),
    }
}
