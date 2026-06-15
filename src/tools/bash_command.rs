//! Implementation of the `BashCommand` tool with WCGW parity.
//!
//! This module provides the implementation for the `BashCommand` tool, which is used
//! to execute shell commands, check command status, and interact with the shell.
//! Matches the behavior of wcgw Python implementation 1:1.

use anyhow::Context as AnyhowContext;
use rand::RngExt;
use regex::Regex;
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::state::pty::PtyShell;
use crate::state::terminal::{render_terminal_output, strip_ansi_codes};
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction, SpecialKey};

type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

// ==================== WCGW-Style Constants ====================

/// Default timeout for command execution (seconds) - matches WCGW Python Config.timeout
const DEFAULT_TIMEOUT: f64 = 5.0;

/// Extended timeout while output is still being produced - matches WCGW Python `Config.timeout_while_output`
const TIMEOUT_WHILE_OUTPUT: f64 = 20.0;

/// Number of iterations to wait without new output before giving up - matches WCGW Python `Config.output_wait_patience`
const OUTPUT_WAIT_PATIENCE: i32 = 3;

/// Polling slice for adaptive output reads. We read in chunks this long and
/// return as soon as the prompt returns, instead of sleeping the full budget.
const POLL_SLICE_SECS: f64 = 0.5;

/// Chunk size for sending commands (characters) - matches WCGW Python (64 chars)
const COMMAND_CHUNK_SIZE: usize = 64;

/// Chunk size for sending text input (characters) - matches WCGW Python (128 chars)
const TEXT_CHUNK_SIZE: usize = 128;

/// Cheap byte-level safety net. We never even consider token counting if the
/// raw payload is smaller than this — tokenizing is fast but not free, and
/// the vast majority of responses are tiny status updates.
const MAX_OUTPUT_LENGTH: usize = 100_000;

/// Token budget reserved for a single PTY response when token-aware truncation
/// kicks in. Picked to leave plenty of room for the surrounding context — most
/// frontier models have 128k+ windows, so 25k for one shell payload is generous
/// without monopolizing the conversation.
const MAX_OUTPUT_TOKENS: usize = 25_000;

/// Tail of `text` at most `max_len` bytes long, snapped up to a char boundary so
/// we never slice through a multibyte UTF-8 sequence (which would panic).
fn char_safe_tail(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }
    let mut start = text.len() - max_len;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Truncate `text` so its Claude token count stays under `max_tokens`.
///
/// We tokenize the tail of the string only when the raw byte length already
/// exceeds the byte cap; otherwise we trust the byte budget and return as-is.
/// When the tail still overshoots, we keep the last `max_tokens - reserve`
/// tokens and prepend a "(...truncated)" marker — exactly what wcgw does in
/// `_incremental_text`.
fn truncate_to_token_budget(text: &str, max_tokens: usize) -> std::borrow::Cow<'_, str> {
    if text.len() <= MAX_OUTPUT_LENGTH {
        return std::borrow::Cow::Borrowed(text);
    }

    let Some(tokens) = crate::utils::encoder::encode_ids(text) else {
        // Fallback to the byte-based truncation we used before the tokenizer.
        return std::borrow::Cow::Owned(format!(
            "(...truncated)\n{}",
            char_safe_tail(text, MAX_OUTPUT_LENGTH)
        ));
    };

    if tokens.len() <= max_tokens {
        return std::borrow::Cow::Borrowed(text);
    }

    // Reserve one token slot for the marker overhead.
    let keep = max_tokens.saturating_sub(1);
    let tail = &tokens[tokens.len() - keep..];
    let decoded = crate::utils::encoder::decode_ids(tail).unwrap_or_else(|| {
        // Tokenizer present but decode failed: fall back to a byte tail.
        char_safe_tail(text, MAX_OUTPUT_LENGTH).to_string()
    });
    std::borrow::Cow::Owned(format!("(...truncated)\n{decoded}"))
}

/// Message when a command is already running - matches WCGW Python `WAITING_INPUT_MESSAGE`
const WAITING_INPUT_MESSAGE: &str = "A command is already running. NOTE: You can't run multiple shell commands in main shell, likely a previous program hasn't exited.
1. Get its output using status check.
2. Use `send_ascii` or `send_specials` to give inputs to the running program OR
3. kill the previous program by sending ctrl+c first using `send_ascii` or `send_specials`
4. Interrupt and run the process in background
";

// ==================== Background Shell Manager ====================

/// Snapshot of a background shell that has exited but whose final output has not
/// yet been consumed by the caller. We keep it around so the next call (typically
/// a `status_check`) can return the trailing output before the entry is gone.
#[derive(Debug, Clone)]
pub struct ExitedShellInfo {
    pub last_command: String,
    pub final_output: String,
    pub exited_at: Instant,
}

/// Manages background shell sessions - matches WCGW Python's `background_shells` dict
#[derive(Debug, Default)]
pub struct BackgroundShellManager {
    shells: HashMap<String, SharedPtyShell>,
    /// Recently exited shells that still owe their final output to the caller.
    /// Entries are consumed the first time the caller queries the id, then dropped.
    tombstones: HashMap<String, ExitedShellInfo>,
}

impl BackgroundShellManager {
    /// Tombstones older than this are garbage-collected on the next prune pass.
    const TOMBSTONE_TTL: Duration = Duration::from_secs(300);

    /// Create a new background shell manager
    pub fn new() -> Self {
        Self { shells: HashMap::new(), tombstones: HashMap::new() }
    }

    /// Start a new background shell and return its command ID
    pub fn start_new_shell(&mut self, working_dir: &Path, restricted_mode: bool) -> Result<String> {
        let cid = format!("{:010x}", rand::rng().random::<u32>());

        let shell = PtyShell::new(working_dir, restricted_mode).map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to start background shell: {e}"))
        })?;

        self.shells.insert(cid.clone(), Arc::new(Mutex::new(Some(shell))));

        info!("Started background shell with id: {}", cid);
        Ok(cid)
    }

    /// Get a background shell by its command ID
    pub fn get_shell(&self, bg_command_id: &str) -> Option<SharedPtyShell> {
        self.shells.get(bg_command_id).cloned()
    }

    /// Remove and cleanup a background shell
    pub fn remove_shell(&mut self, bg_command_id: &str) -> bool {
        if let Some(shell_arc) = self.shells.remove(bg_command_id) {
            if let Ok(mut guard) = shell_arc.try_lock() {
                *guard = None;
            }
            info!("Removed background shell: {}", bg_command_id);
            true
        } else {
            false
        }
    }

    fn prune_finished_shells(&mut self) {
        // GC old tombstones first.
        let now = Instant::now();
        self.tombstones.retain(|_, info| now.duration_since(info.exited_at) < Self::TOMBSTONE_TTL);

        let mut finished: Vec<(String, Option<ExitedShellInfo>)> = Vec::new();

        for (id, shell_arc) in &self.shells {
            let Ok(mut guard) = shell_arc.try_lock() else {
                continue;
            };

            let Some(shell) = guard.as_mut() else {
                finished.push((id.clone(), None));
                continue;
            };

            if !shell.is_alive() {
                let tombstone = ExitedShellInfo {
                    last_command: shell.last_command.clone(),
                    final_output: shell.output_buffer.clone(),
                    exited_at: now,
                };
                finished.push((id.clone(), Some(tombstone)));
                continue;
            }

            // Never prune shells that haven't received a command yet.
            // The global BG_SHELL_MANAGER is shared across parallel tests; a freshly
            // spawned shell would otherwise be evicted between start_new_shell and
            // the first send_command, leading to "Failed to get background shell".
            if shell.last_command.is_empty() {
                continue;
            }

            if shell.command_running {
                let _ = shell.read_output(0.1);
            }

            if !shell.command_running {
                let tombstone = ExitedShellInfo {
                    last_command: shell.last_command.clone(),
                    final_output: shell.output_buffer.clone(),
                    exited_at: now,
                };
                finished.push((id.clone(), Some(tombstone)));
            }
        }

        for (id, tombstone) in finished {
            self.remove_shell(&id);
            if let Some(info) = tombstone {
                self.tombstones.insert(id, info);
            }
        }
    }

    /// Look up the tombstone for a recently-exited shell, if any.
    ///
    /// The entry stays in the map until the TTL expires (see
    /// `prune_finished_shells`), so repeated `status_check` calls on the same
    /// `bg_command_id` keep returning the cached final output instead of
    /// flipping to "shell not found" after the first read.
    pub fn peek_tombstone(&self, bg_command_id: &str) -> Option<ExitedShellInfo> {
        self.tombstones.get(bg_command_id).cloned()
    }

    /// Get info about all running background shells - matches WCGW Python `get_bg_running_commandsinfo`
    pub fn get_running_info(&mut self) -> String {
        self.prune_finished_shells();

        if self.shells.is_empty() {
            return "No command running in background.\n".to_string();
        }

        let mut running = Vec::new();
        for (id, shell_arc) in &self.shells {
            if let Ok(guard) = shell_arc.try_lock() {
                if let Some(bash) = guard.as_ref() {
                    if bash.command_running {
                        running
                            .push(format!("Command: {}, bg_command_id: {}", bash.last_command, id));
                    }
                }
            } else {
                running.push(format!("Command: <busy>, bg_command_id: {id}"));
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
    static ref BG_SHELL_MANAGER: StdMutex<BackgroundShellManager> = StdMutex::new(BackgroundShellManager::new());
}

/// Lock the global background-shell manager, recovering from poisoning.
///
/// A panic while holding this lock (e.g. in the rendering path during a prune)
/// must NOT permanently brick all background-shell functionality for the rest of
/// the server's lifetime. The manager's data stays consistent across a panic, so
/// recovering the inner guard is safe.
fn lock_bg_manager() -> std::sync::MutexGuard<'static, BackgroundShellManager> {
    BG_SHELL_MANAGER.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ==================== WCGW-Style Helper Functions ====================

/// Get WCGW-style status string - matches WCGW Python's `get_status()`
fn get_status(
    bash_state: &BashState,
    is_bg: bool,
    bg_id: Option<&str>,
    is_running: bool,
    running_for: Option<&str>,
) -> String {
    let mut status = "\n\n---\n\n".to_string();

    if is_bg {
        if let Some(id) = bg_id {
            let _ = writeln!(status, "bg_command_id = {id}");
        }
    }

    if is_running {
        status.push_str("status = still running\n");
        if let Some(duration) = running_for {
            let _ = writeln!(status, "running for = {duration}");
        }
    } else {
        status.push_str("status = process exited\n");
    }

    let _ = writeln!(status, "cwd = {}", bash_state.cwd.display());

    if !is_bg {
        // Add background shell info for main shell - matches WCGW Python
        {
            let mut manager = lock_bg_manager();
            status.push_str("This is the main shell. ");
            status.push_str(&manager.get_running_info());
        }
    }

    status.trim_end().to_string()
}

/// Process output with WCGW-style incremental text handling - matches WCGW Python _`incremental_text`
fn wcgw_incremental_text(text: &str, last_pending_output: &str) -> String {
    let truncated = truncate_to_token_budget(text, MAX_OUTPUT_TOKENS);
    let text = truncated.as_ref();

    if last_pending_output.is_empty() {
        let rendered = render_terminal_output(text);
        return rstrip_lines(&rendered).trim_start().to_string();
    }

    let last_rendered = render_terminal_output(last_pending_output);
    if last_rendered.is_empty() {
        return rstrip_lines(&render_terminal_output(text));
    }

    // Get text after last pending output. Snap the offset down to a char
    // boundary: `last_pending_output.len()` is a byte count and may land inside
    // a multibyte code point of `text`, which would panic on the slice.
    let text_after_last = if text.len() > last_pending_output.len() {
        let cut = crate::utils::floor_char_boundary(text, last_pending_output.len());
        &text[cut..]
    } else {
        text
    };

    let combined = format!("{}\n{}", last_rendered.join("\n"), text_after_last);
    let new_rendered = render_terminal_output(&combined);

    // Get incremental part - matches WCGW Python get_incremental_output
    let incremental = get_incremental_output(&last_rendered, &new_rendered);
    rstrip_lines(&incremental)
}

fn extract_prompt_cwd(output: &str) -> Option<PathBuf> {
    static PROMPT_RE: std::sync::OnceLock<Option<Regex>> = std::sync::OnceLock::new();
    let prompt_regex =
        PROMPT_RE.get_or_init(|| Regex::new(r"◉ (?P<cwd>[^\r\n]*?)──➤").ok()).as_ref()?;
    let stripped = strip_ansi_codes(output);

    prompt_regex
        .captures_iter(&stripped)
        .filter_map(|captures| captures.name("cwd").map(|cwd| cwd.as_str().trim()))
        .filter(|cwd| !cwd.is_empty())
        .last()
        .map(PathBuf::from)
}

/// Right-strip each line and join - matches WCGW Python rstrip
fn rstrip_lines(lines: &[String]) -> String {
    lines.iter().map(|line| line.trim_end()).collect::<Vec<_>>().join("\n")
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

fn send_utf8_in_byte_chunks(shell: &mut PtyShell, text: &str, chunk_size: usize) -> Result<()> {
    let mut start = 0;

    while start < text.len() {
        let mut end = (start + chunk_size).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = text[start..].char_indices().nth(1).map_or(text.len(), |(idx, _)| start + idx);
        }

        shell.send_text(&text[start..end]).map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to write PTY input: {e}"))
        })?;
        start = end;
    }

    Ok(())
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

    let thread_id = normalize_thread_id(&bash_command.thread_id);

    // Check if thread_id is empty
    if thread_id.is_empty() {
        error!("Empty thread_id provided in BashCommand");
        return Err(WinxError::ThreadIdMismatch(
            "Error: No saved bash state found for thread ID \"\". Please initialize first with this ID.".to_string()
        ));
    }

    // Extract bash_state data
    let mut bash_state: BashState;
    {
        let bash_state_guard = bash_state_arc.lock().await;

        let Some(state) = &*bash_state_guard else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };

        bash_state = state.clone();
    }

    // Verify thread ID matches - matches WCGW Python thread_id check
    if thread_id != bash_state.current_thread_id {
        // Try to load state from thread_id - matches WCGW Python load_state_from_thread_id
        if !bash_state.load_state_from_disk(&thread_id).unwrap_or(false) {
            return Err(WinxError::ThreadIdMismatch(format!(
                "Error: No saved bash state found for thread_id `{thread_id}`. Please initialize first with this ID."
            )));
        }
    }

    // Calculate effective timeout - matches WCGW Python
    // SECURITY: Ensure timeout is not negative to prevent unexpected behavior
    let timeout_s = bash_command
        .wait_for_seconds
        .map_or(DEFAULT_TIMEOUT, |t| f64::from(t).max(0.0))
        .min(TIMEOUT_WHILE_OUTPUT);

    // Execute the action based on type - matches WCGW Python's _execute_bash()
    let result = execute_bash_action(&mut bash_state, &bash_command.action_json, timeout_s).await;

    {
        let mut bash_state_guard = bash_state_arc.lock().await;
        if let Some(state) = bash_state_guard.as_mut() {
            state.cwd.clone_from(&bash_state.cwd);
        }
    }

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
    let bg_shell: Option<SharedPtyShell> = match action {
        BashCommandAction::Command { .. } => None, // Commands don't use bg_command_id for routing
        BashCommandAction::StatusCheck { bg_command_id, .. }
        | BashCommandAction::SendText { bg_command_id, .. }
        | BashCommandAction::SendSpecials { bg_command_id, .. }
        | BashCommandAction::SendAscii { bg_command_id, .. } => {
            if let Some(id) = bg_command_id {
                let mut manager = BG_SHELL_MANAGER.lock().map_err(|e| {
                    WinxError::BashStateLockError(format!("Failed to lock bg manager: {e}"))
                })?;
                manager.prune_finished_shells();

                if let Some(shell) = manager.get_shell(id) {
                    is_bg = true;
                    bg_id = Some(id.clone());
                    Some(shell)
                } else if let Some(tombstone) = manager.peek_tombstone(id) {
                    // Shell already exited. For a status check we can hand back the
                    // final cached output exactly once. For anything else (send_text,
                    // send_specials, send_ascii) tell the caller the shell is gone
                    // and include the captured output so they can recover state.
                    drop(manager);
                    return finalize_tombstone(&bash_state.cwd, id, tombstone, action);
                } else {
                    // Error message matches WCGW Python
                    let error = format!(
                        "No shell found running with command id {}.\n{}",
                        id,
                        manager.get_running_info()
                    );
                    return Err(WinxError::CommandExecutionError(error));
                }
            } else {
                None
            }
        }
    };

    // Process based on action type - matches WCGW Python _execute_bash dispatch
    match action {
        BashCommandAction::Command { command, is_background, allow_multi } => {
            execute_command(bash_state, command, *is_background, *allow_multi, timeout_s).await
        }
        BashCommandAction::StatusCheck { scrollback_lines, verbose, .. } => {
            execute_status_check(
                bash_state,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                timeout_s,
                *scrollback_lines,
                *verbose,
            )
            .await
        }
        BashCommandAction::SendText { send_text, submit, .. } => {
            execute_send_text(
                bash_state,
                send_text,
                *submit,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                timeout_s,
            )
            .await
        }
        BashCommandAction::SendSpecials { send_specials, submit, .. } => {
            execute_send_specials(
                bash_state,
                send_specials,
                *submit,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                timeout_s,
            )
            .await
        }
        BashCommandAction::SendAscii { send_ascii, submit, .. } => {
            execute_send_ascii(
                bash_state,
                send_ascii,
                *submit,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                timeout_s,
            )
            .await
        }
    }
}

/// Strip a trailing `| tail ...` from a command (wcgw parity, `strip_tail_pipe`).
///
/// LLMs habitually pipe output through `tail`, but we already truncate output
/// server-side — stripping the pipe avoids hiding the earlier output the model
/// usually wants. Only a `tail` at the very end of the command is removed.
///
/// This matches wcgw by default. Set `WINX_KEEP_TAIL_PIPE=1` to preserve the
/// pipe instead (winx's original behavior), e.g. when you deliberately want only
/// the tail of a huge log rather than the server-side truncation.
fn strip_tail_pipe(command: &str) -> String {
    strip_tail_pipe_impl(command, keep_tail_pipe())
}

/// Pure core of [`strip_tail_pipe`], split out so both modes are unit-testable
/// without touching process-wide env vars (tests run concurrently).
fn strip_tail_pipe_impl(command: &str, keep: bool) -> String {
    static RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    if keep {
        return command.to_string();
    }
    let re = RE.get_or_init(|| regex::Regex::new(r"\|\s*tail(?:\s+(?:-n\s*|-)?(\d+))?\s*$").ok());
    match re.as_ref().and_then(|re| re.find(command)) {
        Some(matched) => command[..matched.start()].trim_end().to_string(),
        None => command.to_string(),
    }
}

/// Whether the user opted out of `| tail` stripping via `WINX_KEEP_TAIL_PIPE`.
fn keep_tail_pipe() -> bool {
    std::env::var("WINX_KEEP_TAIL_PIPE").is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// Execute a command - matches WCGW Python's Command handling in _`execute_bash`
async fn execute_command(
    bash_state: &mut BashState,
    command: &str,
    is_background: bool,
    allow_multi: bool,
    timeout_s: f64,
) -> Result<String> {
    // wcgw strips a trailing `| tail` before anything else (model_validator).
    let stripped_command = strip_tail_pipe(command);
    let command = stripped_command.as_str();
    debug!("Processing Command action: {command:?} (allow_multi={allow_multi})");

    // Check mode permissions - matches WCGW Python bash_command_mode check
    if !bash_state.is_command_allowed(command) {
        error!("Command '{}' not allowed in current mode", command);
        return Err(WinxError::CommandNotAllowed(
            "Error: BashCommand not allowed in current mode".to_string(),
        ));
    }

    // Single-statement guard (wcgw parity). Callers can opt out via
    // `allow_multi: true` when they knowingly want to chain commands
    // without wrapping in `bash -lc '...'`.
    let command = command.trim();
    if !allow_multi {
        crate::utils::bash_parser::assert_single_statement(command)?;
    }

    // If background execution requested, start new shell - matches WCGW Python is_background handling
    if is_background {
        return execute_in_background(bash_state, command, timeout_s).await;
    }

    // Check if a command is already running - matches WCGW Python state check
    {
        let bash_guard = bash_state.pty_shell.lock().await;

        if let Some(ref bash) = *bash_guard {
            if bash.command_running {
                return Err(WinxError::CommandExecutionError(WAITING_INPUT_MESSAGE.to_string()));
            }
        }
    }

    // Initialize bash if needed
    if bash_state.pty_shell.lock().await.is_none() {
        bash_state
            .init_pty_shell()
            .await
            .map_err(|e| WinxError::CommandExecutionError(format!("Failed to init bash: {e}")))?;
    }

    // Clear prompt before sending - matches WCGW Python clear_to_run.
    // Drain any leftover output and, if the shell still looks busy, send
    // Ctrl-C so the new command lands on a fresh prompt instead of being
    // appended to whatever was hanging on stdin.
    {
        let needs_reset = {
            let mut bash_guard = bash_state.pty_shell.lock().await;
            match bash_guard.as_mut() {
                Some(bash) => match bash.clear_to_run(DEFAULT_TIMEOUT as f32) {
                    Ok(true) => false,
                    Ok(false) => {
                        warn!("clear_to_run: shell still busy after Ctrl-C, resetting it");
                        true
                    }
                    Err(e) => {
                        warn!("clear_to_run failed ({e}), resetting shell");
                        true
                    }
                },
                None => false,
            }
        };
        // wcgw parity: a shell that won't return to a prompt even after Ctrl-C is
        // recreated, so the new command lands on a fresh prompt instead of being
        // appended to a hung shell. init_pty_shell rebuilds at the same cwd/mode.
        if needs_reset {
            if let Err(e) = bash_state.init_pty_shell().await {
                warn!("Failed to reset shell after clear_to_run: {e}");
            }
        }
    }

    // Send command in chunks of 64 characters - matches WCGW Python exactly
    {
        let mut bash_guard = bash_state.pty_shell.lock().await;

        let bash = bash_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

        bash.output_buffer.clear();
        bash.output_truncated = false;
        // Send in chunks - matches WCGW Python: for i in range(0, len(command), 64)
        send_utf8_in_byte_chunks(bash, command, COMMAND_CHUNK_SIZE)?;

        // Send linesep to execute - matches WCGW Python bash_state.send(bash_state.linesep, ...)
        bash.send_special_key("Enter").map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to send newline: {e}"))
        })?;

        bash.last_command = command.to_string();
        bash.command_running = true;
    }

    // Wait for output with WCGW-style patience handling
    let shell_arc = bash_state.pty_shell.clone();
    wait_for_output(bash_state, &shell_arc, timeout_s, false, None, false).await
}

/// Wait for command output with WCGW-style patience handling - matches WCGW Python expect/wait logic.
///
/// `shell_arc` selects which shell to read from (main shell or a bg shell handle).
async fn wait_for_output(
    bash_state: &mut BashState,
    shell_arc: &SharedPtyShell,
    timeout_s: f64,
    is_bg: bool,
    bg_id: Option<&str>,
    is_status_check: bool,
) -> Result<String> {
    let start = Instant::now();
    let wait = timeout_s.min(TIMEOUT_WHILE_OUTPUT);
    let mut last_pending_output = String::new();
    let mut complete = false;

    // Adaptive polling instead of a blind sleep. wcgw sleeps the full `wait`
    // budget before reading even once, so a `pwd` that finishes in 10ms still
    // costs ~5s. Instead we read in short slices and return the moment the
    // prompt comes back (`read_output` already exits early on prompt + drain),
    // dropping fast-command latency from seconds to ~100ms. Long-running
    // commands still consume the whole budget, since we loop until `complete`
    // or `wait` elapses — identical upper-bound behavior, far snappier floor.
    let mut output = String::new();
    loop {
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed >= wait {
            break;
        }
        let slice = (wait - elapsed).clamp(0.1, POLL_SLICE_SECS);
        let (out, done) = {
            let mut bash_guard = shell_arc.lock().await;
            match bash_guard.as_mut() {
                Some(bash) => bash.read_output(slice as f32).map_err(|e| {
                    WinxError::CommandExecutionError(format!("Failed to read output: {e}"))
                })?,
                None => (String::new(), true),
            }
        };
        output = out;
        complete = done;
        if complete {
            break;
        }
    }

    // If not complete and this is a status check, use WCGW-style patience waiting.
    //
    // Treat `timeout_s` (== caller's `wait_for_seconds`, capped at
    // `TIMEOUT_WHILE_OUTPUT`) as the hard upper bound on the TOTAL wall-clock
    // spent inside this call. wcgw computes `remaining = TIMEOUT_WHILE_OUTPUT
    // - wait`, which makes a 2-second `wait_for_seconds` block for almost 20s
    // on a TUI that keeps emitting spinner frames. We diverge from wcgw here
    // because driving agents expect their wait budget to be respected.
    if !complete && is_status_check {
        let budget_secs = timeout_s.min(TIMEOUT_WHILE_OUTPUT);
        let iter_wait_secs = 0.5_f64;
        let mut patience = OUTPUT_WAIT_PATIENCE;

        let incremental = wcgw_incremental_text(&output, &last_pending_output);
        if incremental.is_empty() {
            patience -= 1;
        }

        let mut last_incremental = incremental;

        while start.elapsed().as_secs_f64() < budget_secs && patience > 0 {
            let remaining = (budget_secs - start.elapsed().as_secs_f64()).max(0.0);
            if remaining < 0.1 {
                break;
            }
            sleep(Duration::from_secs_f64(iter_wait_secs.min(remaining))).await;

            let (new_output, done) = {
                let mut bash_guard = shell_arc.lock().await;

                if let Some(bash) = bash_guard.as_mut() {
                    bash.read_output(0.5).map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to read output: {e}"))
                    })?
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
        }

        if !complete {
            // Update pending output - matches WCGW Python bash_state.set_pending(text)
            last_pending_output = output.clone();
        }
    }

    if complete {
        if let Some(cwd) = extract_prompt_cwd(&output) {
            bash_state.cwd = cwd;
        }
    }

    // Process output through terminal emulation - matches WCGW Python _incremental_text
    let rendered = wcgw_incremental_text(&output, &last_pending_output);

    // Conscious compression: collapse mechanical repetition (identical line runs,
    // blank-line blocks) before truncating, to save tokens without dropping any
    // unique context. Falls back to the raw text when nothing is safe to collapse.
    let rendered = crate::utils::output_compress::compress_output(&rendered).unwrap_or(rendered);

    // Truncate if needed - matches WCGW Python token truncation
    let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();

    // Calculate running duration for status
    let running_for =
        if complete { None } else { Some(format!("{} seconds", start.elapsed().as_secs())) };

    // Add status - matches WCGW Python get_status
    let status = get_status(bash_state, is_bg, bg_id, !complete, running_for.as_deref());
    Ok(format!("{rendered}{status}"))
}

/// Render the final cached output of an exited background shell.
///
/// `status_check` is allowed to "consume" the tombstone and return the trailing
/// output exactly once. Send-style actions (`send_text`, `send_specials`,
/// `send_ascii`) cannot interact with a dead shell, so we return an explicit
/// error that still includes the captured output so the agent can recover state.
fn finalize_tombstone(
    cwd: &Path,
    id: &str,
    tombstone: ExitedShellInfo,
    action: &BashCommandAction,
) -> Result<String> {
    let ExitedShellInfo { last_command, final_output, .. } = tombstone;
    match action {
        BashCommandAction::StatusCheck { .. } => {
            let rendered = wcgw_incremental_text(&final_output, "");
            let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();
            // Build a compact status block matching `get_status` for a finished bg shell.
            let mut status = "\n\n---\n\n".to_string();
            let _ = writeln!(status, "bg_command_id = {id}");
            status.push_str("status = process exited\n");
            let _ = writeln!(status, "cwd = {}", cwd.display());
            Ok(format!("{rendered}{}", status.trim_end()))
        }
        BashCommandAction::SendText { .. }
        | BashCommandAction::SendSpecials { .. }
        | BashCommandAction::SendAscii { .. } => Err(WinxError::CommandExecutionError(format!(
            "Background shell {id} already exited (last command: {last_command}).\nFinal captured output:\n{final_output}"
        ))),
        BashCommandAction::Command { .. } => {
            // We only enter `finalize_tombstone` from the bg routing path, which
            // never matches Command. Treat this as a programmer error.
            unreachable!("finalize_tombstone called for non-bg action")
        }
    }
}

/// Execute a status check - matches WCGW Python's `StatusCheck` handling.
///
/// New behavior (v0.2.308):
/// - Deduplicates against the last response by fingerprint; when nothing
///   changed and `verbose=false`, returns a compact "no new output" payload
///   instead of resending the same screen.
/// - Optional `scrollback_lines` pulls bounded history from the `PtyShell`
///   ringbuffer so agents can reorient after a long pause.
async fn execute_status_check(
    bash_state: &mut BashState,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
    scrollback_lines: Option<usize>,
    verbose: bool,
) -> Result<String> {
    debug!("Processing StatusCheck action (verbose={verbose}, scrollback={scrollback_lines:?})");

    // Pick the shell we're going to inspect: bg shell when bg_command_id was provided,
    // otherwise fall back to the main interactive shell.
    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.pty_shell.clone());

    // Check if there's a running command - matches WCGW Python state check
    let is_running = {
        let guard = shell_arc.lock().await;
        if let Some(ref bash) = *guard {
            bash.command_running
        } else {
            false
        }
    };

    // If no command running and not background, return error - matches WCGW Python
    if !is_running && !is_bg {
        let mut manager = lock_bg_manager();
        let error = format!(
            "No command is currently running, so there's nothing to check. The previous \
             command already finished and its output was returned when it completed. Start a \
             new command, or pass a bg_command_id if you launched one in the background.\n{}",
            manager.get_running_info()
        );
        return Err(WinxError::CommandExecutionError(error));
    }

    // Read output with patience handling - this IS a status check
    let response = wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, true).await?;

    // Inter-call dedup: hash only the response *body* (the chunk before the
    // `\n\n---\n` status footer). The footer contains a live "running for"
    // counter that would otherwise defeat the comparison.
    let body = response.split("\n\n---\n").next().unwrap_or(&response);
    if !verbose && scrollback_lines.is_none() {
        let mut guard = shell_arc.lock().await;
        if let Some(bash) = guard.as_mut() {
            let fingerprint = PtyShell::fingerprint(body);
            if Some(fingerprint) == bash.last_returned_hash {
                let status = get_status(bash_state, is_bg, bg_id, is_running, None);
                return Ok(format!("no new output since last check{status}"));
            }
            bash.last_returned_hash = Some(fingerprint);
        }
    } else if !verbose {
        // Still record the hash so subsequent non-scrollback calls can dedup.
        let mut guard = shell_arc.lock().await;
        if let Some(bash) = guard.as_mut() {
            bash.last_returned_hash = Some(PtyShell::fingerprint(body));
        }
    }

    // Optional scrollback prefix — only ever pulled when the caller asks for it.
    if let Some(lines) = scrollback_lines {
        if lines > 0 {
            let scrollback = {
                let guard = shell_arc.lock().await;
                guard.as_ref().map(|s| s.collect_scrollback(lines)).unwrap_or_default()
            };
            if !scrollback.is_empty() {
                let count = scrollback.lines().count();
                return Ok(format!(
                    "--- scrollback ({count} lines) ---\n{scrollback}\n--- latest ---\n{response}"
                ));
            }
        }
    }

    Ok(response)
}

/// Execute `send_text` - matches WCGW Python's `SendText` handling
async fn execute_send_text(
    bash_state: &mut BashState,
    text: &str,
    submit: bool,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendText action: {text:?} (submit={submit})");

    // Validate - matches WCGW Python
    if text.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_text cannot be empty".to_string(),
        ));
    }

    // Get the target shell
    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.pty_shell.clone());

    // Send text in chunks of 128 characters - matches WCGW Python exactly
    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

        // Send in chunks - matches WCGW Python: for i in range(0, len(command_data.send_text), 128)
        send_utf8_in_byte_chunks(bash, text, TEXT_CHUNK_SIZE)?;

        // Only append Enter when the caller explicitly asks to submit. Many TUIs
        // (e.g., Claude Code) treat a bare CR as a soft newline inside the input
        // box, so blindly auto-Entering interferes with multi-step interaction.
        if submit {
            bash.send_special_key("Enter").map_err(|e| {
                WinxError::CommandExecutionError(format!("Failed to send newline: {e}"))
            })?;
        }
    }

    // Wait for output
    wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false).await
}

/// Execute `send_specials` - matches WCGW Python's `SendSpecials` handling exactly
async fn execute_send_specials(
    bash_state: &mut BashState,
    keys: &[SpecialKey],
    submit: bool,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendSpecials action: {keys:?} (submit={submit})");

    // Validate - matches WCGW Python
    if keys.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_specials cannot be empty".to_string(),
        ));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.pty_shell.clone());
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

        // Send each special key - matches WCGW Python exactly
        for key in keys {
            match key {
                SpecialKey::KeyUp => {
                    // matches WCGW Python: bash_state.send("\033[A", ...)
                    bash.send_special_key("KeyUp").map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyUp: {e}"))
                    })?;
                }
                SpecialKey::KeyDown => {
                    // matches WCGW Python: bash_state.send("\033[B", ...)
                    bash.send_special_key("KeyDown").map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyDown: {e}"))
                    })?;
                }
                SpecialKey::KeyLeft => {
                    // matches WCGW Python: bash_state.send("\033[D", ...)
                    bash.send_special_key("KeyLeft").map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyLeft: {e}"))
                    })?;
                }
                SpecialKey::KeyRight => {
                    // matches WCGW Python: bash_state.send("\033[C", ...)
                    bash.send_special_key("KeyRight").map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyRight: {e}"))
                    })?;
                }
                SpecialKey::Enter => {
                    // matches WCGW Python: bash_state.send("\x0d", ...) - carriage return
                    bash.send_special_key("Enter").map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send Enter: {e}"))
                    })?;
                }
                SpecialKey::CtrlC => {
                    // matches WCGW Python: bash_state.sendintr()
                    bash.send_interrupt().map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send interrupt: {e}"))
                    })?;
                    is_interrupt = true;
                }
                SpecialKey::CtrlD => {
                    // matches WCGW Python: bash_state.sendintr() - same as Ctrl+C in WCGW
                    bash.send_eof().map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send Ctrl+D: {e}"))
                    })?;
                    is_interrupt = true;
                }
                SpecialKey::CtrlZ => {
                    // Ctrl+Z = SIGTSTP (suspend) - ASCII 0x1a
                    bash.send_suspend().map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send Ctrl+Z: {e}"))
                    })?;
                }
            }
        }
        // Submit (append Enter) only when explicitly requested by the caller.
        if submit {
            bash.send_special_key("Enter")
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to submit: {e}")))?;
        }
    }

    // NOTE: wcgw treats a bare Enter as a status check and applies its
    // patience loop. We deliberately diverge: for a driving agent (e.g.,
    // pushing Enter to submit text in a TUI) the patience loop swallows the
    // immediate response. Callers that want patience semantics should use the
    // explicit `status_check` action instead.

    // Wait for output
    let mut output =
        wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false).await?;

    // Add interrupt failure message if still running - matches WCGW Python exactly
    if is_interrupt && output.contains("status = still running") {
        output.push_str("\n---\n----\nFailure interrupting.\nYou may want to try Ctrl-c again or program specific exit interactive commands.\n");
    }

    Ok(output)
}

/// Execute `send_ascii` - matches WCGW Python's `SendAscii` handling
async fn execute_send_ascii(
    bash_state: &mut BashState,
    ascii_codes: &[u8],
    submit: bool,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    timeout_s: f64,
) -> Result<String> {
    debug!("Processing SendAscii action: {ascii_codes:?} (submit={submit})");

    // Validate - matches WCGW Python
    if ascii_codes.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_ascii cannot be empty".to_string(),
        ));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| bash_state.pty_shell.clone());
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

        // Send each ASCII code - matches WCGW Python
        for &code in ascii_codes {
            // matches WCGW Python: bash_state.send(chr(ascii_char), ...)
            bash.send_bytes(&[code]).map_err(|e| {
                WinxError::CommandExecutionError(format!("Failed to write ASCII code: {e}"))
            })?;

            // Check for interrupt - matches WCGW Python: if ascii_char == 3: is_interrupt = True
            if code == 3 {
                is_interrupt = true;
            }
        }
        // Submit (append Enter) only when explicitly requested by the caller.
        if submit {
            bash.send_special_key("Enter")
                .map_err(|e| WinxError::CommandExecutionError(format!("Failed to submit: {e}")))?;
        }
    }

    // Same divergence from wcgw as in `execute_send_specials`: send_ascii [10]
    // or [13] is treated as a direct write, not a status check. Callers that
    // need patience-aware reads should use `status_check`.

    // Wait for output
    let mut output =
        wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false).await?;

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
    let restricted_mode =
        matches!(bash_state.bash_command_mode.bash_mode, crate::types::BashMode::RestrictedMode);

    let bg_id = {
        let mut manager = lock_bg_manager();
        manager.start_new_shell(&bash_state.cwd, restricted_mode)?
    };

    // Get the shell
    let shell_arc = {
        let manager = lock_bg_manager();
        manager.get_shell(&bg_id).ok_or_else(|| {
            WinxError::CommandExecutionError("Failed to get background shell".to_string())
        })?
    };

    // Send command via the same PTY path used by foreground execute_command.
    {
        let mut guard = shell_arc.lock().await;
        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        bash.send_command(command).map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to send bg command: {e}"))
        })?;
    }
    debug!("bg[{}]: send_command returned, replying with bg_command_id", bg_id);

    let _ = timeout_s;
    let _ = shell_arc;
    Ok(get_status(bash_state, true, Some(&bg_id), true, None))
}

// ==================== Legacy Screen-based Functions (kept for backward compatibility) ====================

/// Process simple command execution for a bash command (legacy)
#[allow(dead_code)]
#[tracing::instrument(level = "debug", skip(command, cwd))]
async fn execute_simple_command(command: &str, cwd: &Path) -> Result<String> {
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
        if rendered_lines.is_empty() {
            // Fallback: just strip ANSI codes if rendering failed or wasn't needed
            result = strip_ansi_codes(&raw_result);
        } else {
            result = rendered_lines.join("\n");
        }
    }

    result = truncate_to_token_budget(&result, MAX_OUTPUT_TOKENS).into_owned();

    let exit_status = if output.status.success() {
        "Command completed successfully".to_string()
    } else {
        format!("Command failed with status: {}", output.status)
    };

    let current_dir = std::env::current_dir()
        .map_or_else(|_| "Unknown".to_string(), |p| p.to_string_lossy().into_owned());

    debug!("Command executed in {:.2?}", elapsed);
    Ok(format!("{result}\n\n---\n\nstatus = {exit_status}\ncwd = {current_dir}\n"))
}

/// Execute command in screen (legacy)
#[allow(dead_code)]
#[tracing::instrument(level = "debug", skip(command, cwd, screen_name))]
async fn execute_in_screen(command: &str, cwd: &Path, screen_name: &str) -> Result<String> {
    debug!("Executing command in screen session '{}': {}", screen_name, command);

    let screen_check = Command::new("which")
        .arg("screen")
        .output()
        .context("Failed to check for screen command")?;

    if !screen_check.status.success() {
        warn!("Screen command not found, falling back to direct execution");
        return execute_simple_command(command, cwd).await;
    }

    let _cleanup = Command::new("screen").args(["-X", "-S", screen_name, "quit"]).output();

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

    let screen_check =
        Command::new("screen").args(["-ls"]).output().context("Failed to list screen sessions")?;

    let screen_list = String::from_utf8_lossy(&screen_check.stdout).to_string();

    let current_dir = std::env::current_dir()
        .map_or_else(|_| "Unknown".to_string(), |p| p.to_string_lossy().into_owned());

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
fn special_key_to_screen_input(key: SpecialKey) -> String {
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

#[cfg(test)]
mod tests {
    use super::strip_tail_pipe_impl;

    #[test]
    fn strips_trailing_tail_by_default() {
        assert_eq!(strip_tail_pipe_impl("seq 1 5 | tail -2", false), "seq 1 5");
        assert_eq!(strip_tail_pipe_impl("cat log | tail -n 20", false), "cat log");
        assert_eq!(strip_tail_pipe_impl("cat log | tail", false), "cat log");
        assert_eq!(strip_tail_pipe_impl("ls -la|tail -5", false), "ls -la");
    }

    #[test]
    fn keeps_command_without_trailing_tail() {
        // tail not at the end, or piped further, must be left alone.
        assert_eq!(strip_tail_pipe_impl("tail -f log | grep err", false), "tail -f log | grep err");
        assert_eq!(strip_tail_pipe_impl("echo hi", false), "echo hi");
        assert_eq!(
            strip_tail_pipe_impl("cat a | tail -5 | wc -l", false),
            "cat a | tail -5 | wc -l"
        );
    }

    #[test]
    fn keep_mode_preserves_tail_pipe() {
        // WINX_KEEP_TAIL_PIPE behavior: command passes through untouched.
        assert_eq!(strip_tail_pipe_impl("seq 1 5 | tail -2", true), "seq 1 5 | tail -2");
        assert_eq!(strip_tail_pipe_impl("cat log | tail -n 20", true), "cat log | tail -n 20");
    }
}
