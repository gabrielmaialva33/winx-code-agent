//! Implementation of the `BashCommand` tool with WCGW parity.
//!
//! This module provides the implementation for the `BashCommand` tool, which is used
//! to execute shell commands, check command status, and interact with the shell.
//! Matches the behavior of wcgw Python implementation 1:1.

use anyhow::Context as AnyhowContext;
use regex::Regex;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

pub use super::background_shell::{BackgroundShellManager, ExitedShellInfo};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, EmbeddedShellRuntime, ShellRuntime, ShellTarget};
use crate::state::bash_state::BashState;
use crate::state::live_terminal::ScreenUpdate;
use crate::state::pty::PtyShell;
use crate::state::terminal::{render_terminal_output, strip_ansi_codes};
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction, SpecialKey};

type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

/// Per-adapter delivery state used by daemon guardians. Embedded callers keep
/// using the cursor stored directly on `PtyShell`, preserving legacy behavior.
#[derive(Debug, Default)]
pub(crate) struct ShellDeliveryCursor {
    generation: Option<u64>,
    delivered_output: String,
    last_returned_hash: Option<u64>,
    screen_snapshot: Option<Vec<String>>,
}

impl ShellDeliveryCursor {
    fn sync_generation(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.delivered_output.clear();
            self.last_returned_hash = None;
            self.screen_snapshot = None;
        }
    }

    fn screen_update(&mut self, current: Vec<String>, threshold: usize) -> ScreenUpdate {
        let update = match self.screen_snapshot.as_ref() {
            Some(previous) => {
                let rows = previous.len().max(current.len());
                let changed = (0..rows)
                    .filter_map(|index| {
                        let before = previous.get(index).map_or("", String::as_str);
                        let after = current.get(index).map_or("", String::as_str);
                        (before != after).then(|| (index + 1, after.to_string()))
                    })
                    .collect::<Vec<_>>();
                if changed.is_empty() {
                    ScreenUpdate::Unchanged
                } else if changed.len() <= threshold {
                    ScreenUpdate::Diff(changed)
                } else {
                    ScreenUpdate::Full(current.clone())
                }
            }
            None => ScreenUpdate::Full(current.clone()),
        };
        self.screen_snapshot = Some(current);
        update
    }
}

fn main_shell(bash_state: &BashState) -> SharedPtyShell {
    let mut store = lock_session_store();
    store.bind_main(&bash_state.current_thread_id, &bash_state.pty_shell);
    store
        .resolve(&bash_state.current_thread_id, &ShellTarget::Main)
        .unwrap_or_else(|| bash_state.pty_shell.clone())
}

// ==================== WCGW-Style Constants ====================

/// Default timeout for command execution (seconds) - matches WCGW Python Config.timeout
const DEFAULT_TIMEOUT: f64 = 5.0;

/// Number of iterations to wait without new output before giving up - matches WCGW Python `Config.output_wait_patience`
const OUTPUT_WAIT_PATIENCE: i32 = 3;

fn effective_wait_for_seconds(wait_for_seconds: Option<f32>) -> f64 {
    wait_for_seconds.map_or(DEFAULT_TIMEOUT, |seconds| f64::from(seconds).max(0.0))
}

/// Async poll interval for adaptive output reads. We drain whatever the reader
/// thread has queued (non-blocking), release the shell lock, then `await` this
/// long — yielding the executor instead of pinning it on a blocking sleep.
const POLL_INTERVAL_MS: u64 = 20;

/// Grace period after the prompt is seen, to capture bytes that land just after
/// it (mirrors the old `read_output` post-prompt drain) — awaited, not blocked.
const POST_PROMPT_DRAIN_MS: u64 = 100;

/// Chunk size for sending commands (characters) - matches WCGW Python (64 chars)
const COMMAND_CHUNK_SIZE: usize = 64;

/// Chunk size for sending text input (characters) - matches WCGW Python (128 chars)
const TEXT_CHUNK_SIZE: usize = 128;

/// Byte cap used only when exact tokenization or decoding is unavailable.
const MAX_OUTPUT_LENGTH: usize = 100_000;

/// Token budget reserved for a single PTY response when token-aware truncation
/// kicks in. Picked to leave plenty of room for the surrounding context — most
/// frontier models have 128k+ windows, so 25k for one shell payload is generous
/// without monopolizing the conversation.
const MAX_OUTPUT_TOKENS: usize = 25_000;

/// Delay between typing text and the submitting Enter. Ink-based TUIs (Claude
/// Code) collapse a burst of bytes into a single render tick and drop a CR that
/// arrives glued to the text — the input box updates but never submits. Sending
/// the Enter as a separate write after this short pause lets the TUI commit the
/// typed text first, so `submit: true` reliably submits. ~40ms is imperceptible
/// yet enough for the event loop to flush; harmless for plain shells.
const SUBMIT_NUDGE_DELAY: Duration = Duration::from_millis(40);

/// `screen` diff mode emits a per-line delta only when at most this many lines
/// changed; beyond that the full frame is cheaper to read than a giant diff.
const SCREEN_DIFF_THRESHOLD: usize = 10;

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
    if crate::utils::encoder::definitely_fits_token_budget(text, max_tokens) {
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

/// Drain one background shell even when the client never polls it, then turn it
/// into a short-lived tombstone. Without this task, a completed `is_background`
/// command leaves a bash process, PTY and reader thread alive until some later
/// foreground status happens to prune the global manager.
fn spawn_background_reaper(owner_thread_id: String, bg_command_id: String) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(100)).await;
            let shell_arc = {
                let manager = lock_session_store();
                manager.get_shell(&owner_thread_id, &bg_command_id)
            };
            let Some(shell_arc) = shell_arc else {
                return;
            };

            let finished = {
                let mut guard = shell_arc.lock().await;
                match guard.as_mut() {
                    Some(shell) => shell.poll_output_nonblocking() || !shell.is_alive(),
                    None => true,
                }
            };
            if finished {
                // Drop the reaper's own Arc before pruning. `prune_finished_shells`
                // uses the strong count to detect an in-flight status/screen reader;
                // keeping this clone alive would make every finished shell look busy.
                drop(shell_arc);
                let removed = {
                    let mut manager = lock_session_store();
                    manager.prune_finished_shells();
                    manager.get_shell(&owner_thread_id, &bg_command_id).is_none()
                };
                if removed {
                    return;
                }
            }
        }
    });
}

// ==================== WCGW-Style Helper Functions ====================

/// Get WCGW-style status string - matches WCGW Python's `get_status()`
fn get_status(
    bash_state: &BashState,
    is_bg: bool,
    bg_id: Option<&str>,
    is_running: bool,
    running_for: Option<&str>,
    exit_code: Option<i32>,
    reported_cwd: Option<&Path>,
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
        // Exit code of the just-finished command, parsed from the prompt marker
        // (`──➤<nonce>:<code>`). Lets the agent see failure without grepping stderr.
        if let Some(code) = exit_code {
            let _ = writeln!(status, "exit code = {code}");
        }
    }

    let cwd = reported_cwd.unwrap_or(&bash_state.cwd);
    let _ = writeln!(status, "cwd = {}", cwd.display());

    if !is_bg {
        // Add background shell info for main shell - matches WCGW Python
        {
            let mut manager = lock_session_store();
            status.push_str("This is the main shell. ");
            status.push_str(&manager.get_running_info(&bash_state.current_thread_id));
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
    static PROMPT_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // `.expect`, not `.ok()`: a compile-time-literal regex that fails to build is a
    // dev bug, not a runtime condition. The old `OnceLock<Option<Regex>>` froze
    // `None` forever on the first failure, silently disabling all CWD tracking for
    // the rest of the process. Fail loud instead.
    #[allow(clippy::expect_used)]
    let prompt_regex = PROMPT_RE
        .get_or_init(|| Regex::new(r"◉ (?P<cwd>[^\r\n]*?)──➤").expect("prompt regex must compile"));
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

/// Send the submitting Enter as a separate, slightly-delayed write so an
/// Ink-based TUI doesn't swallow a CR glued to the preceding input. Briefly
/// drops the shell lock during the pause so the reader thread keeps draining.
/// See [`SUBMIT_NUDGE_DELAY`].
async fn submit_enter(shell_arc: &SharedPtyShell) -> Result<()> {
    tokio::time::sleep(SUBMIT_NUDGE_DELAY).await;
    let mut guard = shell_arc.lock().await;
    let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
    ensure_interactive_target(bash)?;
    bash.send_special_key("Enter")
        .map_err(|e| WinxError::CommandExecutionError(format!("Failed to submit: {e}")))?;
    Ok(())
}

/// Interactive bytes are input for an already-running foreground program, not
/// an alternate command-execution API. Drain first so a prompt that arrived just
/// before this call marks the command complete; otherwise text submitted to an
/// idle shell would bypass the mode's command allowlist entirely.
fn ensure_interactive_target(shell: &mut PtyShell) -> Result<()> {
    shell.poll_output_nonblocking();
    if shell.command_running {
        Ok(())
    } else {
        Err(WinxError::CommandExecutionError(
            "No interactive command is running. Start an allowed command first; send_text/send_ascii/send_specials cannot execute commands in an idle shell."
                .to_string(),
        ))
    }
}

// ==================== Main Tool Handler ====================

/// Handles the `BashCommand` tool call with WCGW parity
///
/// This function processes the `BashCommand` tool call following WCGW Python's
/// `execute_bash()` function behavior exactly.
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
) -> Result<String> {
    handle_tool_call_with_runtime(&EmbeddedShellRuntime, bash_state_arc, bash_command).await
}

/// Execute a BashCommand through the selected shell runtime.
pub async fn handle_tool_call_with_runtime(
    runtime: &dyn ShellRuntime,
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
) -> Result<String> {
    runtime.run_action(bash_state_arc, bash_command).await
}

pub(crate) async fn handle_embedded_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
) -> Result<String> {
    handle_embedded_tool_call_inner(bash_state_arc, bash_command, None).await
}

pub(crate) async fn handle_embedded_tool_call_with_cursor(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
    delivery_cursor: &Arc<Mutex<ShellDeliveryCursor>>,
) -> Result<String> {
    let mut delivery_cursor = delivery_cursor.lock().await;
    handle_embedded_tool_call_inner(bash_state_arc, bash_command, Some(&mut delivery_cursor)).await
}

#[tracing::instrument(level = "info", skip(bash_state_arc, bash_command, delivery_cursor))]
async fn handle_embedded_tool_call_inner(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    bash_command: BashCommand,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let action_kind = match &bash_command.action_json {
        BashCommandAction::Command { .. } => "command",
        BashCommandAction::StatusCheck { .. } => "status_check",
        BashCommandAction::SendText { .. } => "send_text",
        BashCommandAction::SendSpecials { .. } => "send_specials",
        BashCommandAction::SendAscii { .. } => "send_ascii",
        BashCommandAction::Screen { .. } => "screen",
        BashCommandAction::WaitForTurn { .. } => "wait_for_turn",
    };
    info!(thread_id = %bash_command.thread_id, action = action_kind, "BashCommand tool called");

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
        // Distinguish "no state for this id" (Ok(false)) from a real load failure
        // (Err: permission denied, corrupt JSON). The old `.unwrap_or(false)`
        // collapsed both into the misleading "initialize first" message.
        let loaded = bash_state.load_state_from_disk(&thread_id).map_err(|e| {
            WinxError::CommandExecutionError(format!(
                "Failed to load saved bash state for thread_id `{thread_id}`: {e}"
            ))
        })?;
        if !loaded {
            return Err(WinxError::ThreadIdMismatch(format!(
                "Error: No saved bash state found for thread_id `{thread_id}`. Please initialize first with this ID."
            )));
        }
        // Promote the loaded state back to the shared slot. The old code only wrote
        // `cwd` back at the end, so switching thread_id silently dropped the loaded
        // whitelist/mode/thread_id — the next command re-cloned the stale state.
        // load_state_from_disk doesn't touch pty_shell (shells aren't serialized),
        // so the live shell Arc is preserved through the clone.
        if let Some(state) = bash_state_arc.lock().await.as_mut() {
            *state = bash_state.clone();
        }
    }

    lock_session_store().bind_main(&bash_state.current_thread_id, &bash_state.pty_shell);

    // Honor the caller's wait budget; only negative values are normalized.
    let timeout_s = effective_wait_for_seconds(bash_command.wait_for_seconds);

    // Execute the action based on type - matches WCGW Python's _execute_bash()
    let result =
        execute_bash_action(&mut bash_state, &bash_command.action_json, timeout_s, delivery_cursor)
            .await;

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
#[allow(clippy::too_many_lines)]
async fn execute_bash_action(
    bash_state: &mut BashState,
    action: &BashCommandAction,
    timeout_s: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let mut is_bg = false;
    let mut bg_id: Option<String> = None;

    // Handle bg_command_id routing - matches WCGW Python
    let bg_shell: Option<SharedPtyShell> = match action {
        BashCommandAction::Command { .. } => None, // Commands don't use bg_command_id for routing
        BashCommandAction::StatusCheck { bg_command_id, .. }
        | BashCommandAction::SendText { bg_command_id, .. }
        | BashCommandAction::SendSpecials { bg_command_id, .. }
        | BashCommandAction::SendAscii { bg_command_id, .. }
        | BashCommandAction::Screen { bg_command_id, .. }
        | BashCommandAction::WaitForTurn { bg_command_id, .. } => {
            if let Some(id) = bg_command_id {
                // Use the recovery helper (poison -> into_inner) like every other
                // call site, rather than hard-failing this one path on poison.
                let mut manager = lock_session_store();
                manager.prune_finished_shells();

                if let Some(shell) = manager.get_shell(&bash_state.current_thread_id, id) {
                    is_bg = true;
                    bg_id = Some(id.clone());
                    Some(shell)
                } else if let Some(tombstone) =
                    manager.peek_tombstone(&bash_state.current_thread_id, id)
                {
                    // Shell already exited. For a status check we can hand back the
                    // final cached output exactly once. For anything else (send_text,
                    // send_specials, send_ascii) tell the caller the shell is gone
                    // and include the captured output so they can recover state.
                    drop(manager);
                    return finalize_tombstone(id, tombstone, action);
                } else {
                    // Error message matches WCGW Python
                    let error = format!(
                        "No shell found running with command id {}.\n{}",
                        id,
                        manager.get_running_info(&bash_state.current_thread_id)
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
            execute_command(
                bash_state,
                command,
                *is_background,
                *allow_multi,
                timeout_s,
                delivery_cursor,
            )
            .await
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
                delivery_cursor,
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
                delivery_cursor,
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
                delivery_cursor,
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
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::Screen { lines, diff, .. } => {
            execute_screen(
                bash_state,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                *lines,
                *diff,
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::WaitForTurn {
            recognizer,
            quiet_ms,
            timeout_seconds,
            lines,
            wait_through_busy,
            ..
        } => {
            execute_wait_for_turn(
                bash_state,
                bg_shell,
                is_bg,
                bg_id.as_deref(),
                recognizer.as_deref(),
                *quiet_ms,
                *timeout_seconds,
                *lines,
                *wait_through_busy,
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
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    if keep {
        return command.to_string();
    }
    // `.expect`, not `.ok()`: a compile-time-literal regex that fails to build is
    // a dev bug. The old `OnceLock<Option<Regex>>` + `.ok()` would freeze `None`
    // and silently stop stripping `| tail` for the whole process.
    #[allow(clippy::expect_used)]
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\|\s*tail(?:\s+(?:-n\s*|-)?(\d+))?\s*$")
            .expect("tail-pipe regex must compile")
    });
    match re.find(command) {
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
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    // wcgw strips a trailing `| tail` before anything else (model_validator).
    let stripped_command = strip_tail_pipe(command);
    let command = stripped_command.as_str();
    debug!(bytes = command.len(), allow_multi, "Processing Command action");

    // Check mode permissions - matches WCGW Python bash_command_mode check
    if !bash_state.is_command_allowed(command) {
        error!(bytes = command.len(), "Command not allowed in current mode");
        return Err(WinxError::CommandNotAllowed(
            "Error: BashCommand not allowed in current mode".to_string(),
        ));
    }

    // Single-statement guard (wcgw parity). Callers can opt out via
    // `allow_multi: true` when they knowingly want to chain commands
    // without wrapping in `bash -lc '...'`.
    let command = command.trim();
    if !allow_multi {
        // The `bash -n -c` fallback inside the parser spawns a shell; only allow
        // that probe in trusted (wcgw) mode. In restricted modes tree-sitter is
        // the sole arbiter so we never shell out to vet a command.
        let allow_shell_probe = matches!(bash_state.mode, crate::types::Modes::Wcgw);
        crate::utils::bash_parser::assert_single_statement(command, allow_shell_probe)?;
    }

    // If background execution requested, start new shell - matches WCGW Python is_background handling
    if is_background {
        return execute_in_background(bash_state, command, timeout_s, delivery_cursor).await;
    }

    // `BashState` is cloned per request, so the shell mutex alone did not make
    // the check below atomic with command submission: two simultaneous requests
    // could both observe idle, then both write into the same foreground PTY.
    // Serialize the complete foreground start/wait path across those clones.
    let foreground_gate = bash_state.foreground_command_gate.clone();
    let _foreground_guard = foreground_gate.lock_owned().await;
    let shell_arc = main_shell(bash_state);

    // Check if a command is already running - matches WCGW Python state check
    {
        let bash_guard = shell_arc.lock().await;

        if let Some(ref bash) = *bash_guard {
            if bash.command_running {
                return Err(WinxError::CommandExecutionError(WAITING_INPUT_MESSAGE.to_string()));
            }
        }
    }

    // Initialize bash if needed
    if shell_arc.lock().await.is_none() {
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
            // Only when a shell exists. clear_to_run_async drains/interrupts WITHOUT
            // holding the lock across a blocking sleep (was bash.clear_to_run, which
            // slept up to DEFAULT_TIMEOUT seconds inside the tokio mutex).
            if shell_arc.lock().await.is_some() {
                if clear_to_run_async(&shell_arc, DEFAULT_TIMEOUT).await {
                    false
                } else {
                    warn!("clear_to_run: shell still busy after Ctrl-C, resetting it");
                    true
                }
            } else {
                false
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
    let scratch_root = bash_state.workspace_root.clone();
    {
        let mut bash_guard = shell_arc.lock().await;

        let bash = bash_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

        bash.output_buffer.clear();
        bash.output_truncated = false;
        // Offload an over-long output's dropped head to a scratch file under the
        // current workspace so the agent can recover it via ReadFiles.
        bash.reset_scratch();
        bash.set_scratch_root(&scratch_root);
        // Mirror send_command's full reset. This foreground path hand-rolls the
        // field mutation (it chunks the send for WCGW parity) and used to drop
        // these two: a stale exit code or dedup hash would then leak into the
        // next status_check — wrong `exit code`, or a false "no new output".
        bash.last_exit_code = None;
        bash.last_returned_hash = None;
        bash.mark_output_delivered("");
        // Send in chunks - matches WCGW Python: for i in range(0, len(command), 64)
        send_utf8_in_byte_chunks(bash, command, COMMAND_CHUNK_SIZE)?;

        // Send linesep to execute - matches WCGW Python bash_state.send(bash_state.linesep, ...)
        bash.send_special_key("Enter").map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to send newline: {e}"))
        })?;

        bash.last_command = command.to_string();
        bash.command_running = true;
        bash.mark_command_started();
    }

    // Wait for output with WCGW-style patience handling
    wait_for_output(bash_state, &shell_arc, timeout_s, false, None, false, delivery_cursor).await
}

/// Wait for command output with WCGW-style patience handling - matches WCGW Python expect/wait logic.
///
/// Non-blocking drain of queued shell output under the lock; returns whether the
/// command has completed. The lock is released before returning so the caller can
/// `await` a poll interval without pinning the executor — unlike the old
/// `read_output`, which slept (blocking) inside this lock.
async fn poll_shell(shell_arc: &SharedPtyShell) -> bool {
    let mut guard = shell_arc.lock().await;
    match guard.as_mut() {
        Some(bash) => bash.poll_output_nonblocking(),
        None => true,
    }
}

/// Snapshot the shell's accumulated output buffer in one lock acquisition.
async fn snapshot_shell(shell_arc: &SharedPtyShell) -> String {
    let mut guard = shell_arc.lock().await;
    guard.as_mut().map_or_else(String::new, |bash| bash.output_snapshot())
}

/// Poll-drain the shell until its prompt returns or `budget_secs` elapses; returns
/// whether the prompt was seen (i.e. the shell is idle). The lock is released
/// between polls and the wait is awaited, so the executor is never blocked.
async fn drain_until_prompt(shell_arc: &SharedPtyShell, budget_secs: f64) -> bool {
    let start = Instant::now();
    loop {
        if poll_shell(shell_arc).await {
            return true;
        }
        if start.elapsed().as_secs_f64() >= budget_secs {
            return false;
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Async replacement for `PtyShell::clear_to_run`: drain leftover output, and if
/// the shell still looks busy, send Ctrl-C and re-drain — returning whether the
/// shell reached an idle prompt. The old sync method ran `read_output` (a blocking
/// `thread::sleep` loop, up to `DEFAULT_TIMEOUT` seconds) WHILE the caller held the
/// tokio mutex, pinning the worker on every foreground command. This holds the
/// lock only for the instantaneous poll/interrupt, never across a wait.
async fn clear_to_run_async(shell_arc: &SharedPtyShell, max_wait_secs: f64) -> bool {
    // Phase 1: a quick drain — return the moment the prompt is already back.
    if drain_until_prompt(shell_arc, max_wait_secs.min(0.5)).await {
        return true;
    }
    // Still busy: interrupt (lock held only for the send), then re-drain.
    {
        let mut guard = shell_arc.lock().await;
        if let Some(bash) = guard.as_mut() {
            if let Err(e) = bash.send_interrupt() {
                warn!("clear_to_run: failed to send Ctrl-C: {e}");
            }
        }
    }
    drain_until_prompt(shell_arc, max_wait_secs).await
}

/// `shell_arc` selects which shell to read from (main shell or a bg shell handle).
async fn wait_for_output(
    bash_state: &mut BashState,
    shell_arc: &SharedPtyShell,
    timeout_s: f64,
    is_bg: bool,
    bg_id: Option<&str>,
    is_status_check: bool,
    mut delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let start = Instant::now();
    let wait = timeout_s;
    let (generation, legacy_delivered) = {
        let guard = shell_arc.lock().await;
        guard.as_ref().map_or((0, String::new()), |shell| {
            (shell.command_generation(), shell.delivered_output())
        })
    };
    let mut previously_delivered = match delivery_cursor.as_deref_mut() {
        Some(cursor) => {
            cursor.sync_generation(generation);
            cursor.delivered_output.clone()
        }
        None => legacy_delivered,
    };
    let mut complete = false;

    // Adaptive polling instead of a blind sleep. wcgw sleeps the full `wait`
    // budget before reading even once, so a `pwd` that finishes in 10ms still
    // costs ~5s. Instead we read in short slices and return the moment the
    // prompt comes back (`read_output` already exits early on prompt + drain),
    // dropping fast-command latency from seconds to ~100ms. Long-running
    // commands still consume the whole budget, since we loop until `complete`
    // or `wait` elapses — identical upper-bound behavior, far snappier floor.
    // Drain non-blocking, release the lock, then `await` the poll interval. The
    // old `read_output(slice)` slept up to ~0.5s of blocking `thread::sleep` WHILE
    // holding this tokio mutex — pinning the worker thread (starving every other
    // task on it) and the shell lock the whole time. poll + `await` frees both.
    let mut output = String::new();
    loop {
        if start.elapsed().as_secs_f64() >= wait {
            break;
        }
        complete = poll_shell(shell_arc).await;
        if complete {
            break;
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    // Post-prompt grace drain (awaited, no lock held), then one buffer snapshot —
    // the old read_output cloned the whole buffer on every slice (performance-2).
    if complete {
        sleep(Duration::from_millis(POST_PROMPT_DRAIN_MS)).await;
        poll_shell(shell_arc).await;
    }
    output = snapshot_shell(shell_arc).await;

    if let Some(cursor) = delivery_cursor.as_deref_mut() {
        let generation = {
            let guard = shell_arc.lock().await;
            guard.as_ref().map_or(0, PtyShell::command_generation)
        };
        if cursor.generation != Some(generation) {
            cursor.sync_generation(generation);
            previously_delivered.clear();
        }
    }

    // If not complete and this is a status check, use WCGW-style patience waiting.
    //
    // Treat `timeout_s` (the caller's `wait_for_seconds`) as the hard upper
    // bound on the TOTAL wall-clock spent inside this call. Driving agents rely
    // on that contract when they deliberately choose a long poll.
    if !complete && is_status_check {
        let budget_secs = timeout_s;
        let iter_wait_secs = 0.5_f64;
        let mut patience = OUTPUT_WAIT_PATIENCE;

        let incremental = wcgw_incremental_text(&output, &previously_delivered);
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

            // The patience `sleep(iter_wait_secs)` above already elapsed, so the
            // last half-second of output is queued — drain it non-blocking (was
            // read_output(0.5) holding the lock across its own internal sleep).
            let done = poll_shell(shell_arc).await;
            let new_output = snapshot_shell(shell_arc).await;

            if done {
                complete = true;
                output = new_output;
                break;
            }

            // Check if output changed - matches WCGW Python patience logic
            let new_incremental = wcgw_incremental_text(&new_output, &previously_delivered);
            if new_incremental == last_incremental {
                patience -= 1;
            } else {
                patience = OUTPUT_WAIT_PATIENCE; // Reset patience on new output
            }
            last_incremental = new_incremental;

            output = new_output;
        }
    }

    if complete && !is_bg {
        if let Some(cwd) = extract_prompt_cwd(&output) {
            bash_state.cwd = cwd;
        }
    }

    // Process output through terminal emulation - matches WCGW Python _incremental_text
    let rendered = wcgw_incremental_text(&output, &previously_delivered);
    // Advance the delivery cursor only AFTER rendering. The old code assigned the
    // current snapshot first and then diffed it against itself, hiding every new
    // byte emitted while a process was still running (even with verbose=true).
    if let Some(cursor) = delivery_cursor.as_deref_mut() {
        cursor.delivered_output.clone_from(&output);
    } else {
        let mut guard = shell_arc.lock().await;
        if let Some(shell) = guard.as_mut() {
            shell.mark_output_delivered(&output);
        }
    }

    // Conscious compression: collapse mechanical repetition (identical line runs,
    // blank-line blocks) before truncating, to save tokens without dropping any
    // unique context. Falls back to the raw text when nothing is safe to collapse.
    let rendered = {
        let before_lines = rendered.lines().count();
        match crate::utils::output_compress::compress_output(&rendered) {
            Some(compressed) => {
                debug!(
                    from_lines = before_lines,
                    to_lines = compressed.lines().count(),
                    "winx collapsed mechanical repetition in shell output"
                );
                compressed
            }
            None => rendered,
        }
    };

    // Truncate if needed - matches WCGW Python token truncation
    let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();

    // Read command age, exit code, and scratch pointer in one lock acquisition.
    let (running_for, exit_code, shell_cwd, scratch_pointer) =
        read_status_extras(shell_arc, complete).await;
    let running_for = running_for.map(|elapsed| format!("{} seconds", elapsed.as_secs()));

    // Add status - matches WCGW Python get_status
    let status = get_status(
        bash_state,
        is_bg,
        bg_id,
        !complete,
        running_for.as_deref(),
        exit_code,
        shell_cwd.as_deref(),
    );
    Ok(format!("{rendered}{status}{scratch_pointer}"))
}

/// Pull command age, exit code, and any output-offload pointer in one lock.
async fn read_status_extras(
    shell_arc: &SharedPtyShell,
    complete: bool,
) -> (Option<Duration>, Option<i32>, Option<PathBuf>, String) {
    let guard = shell_arc.lock().await;
    let Some(shell) = guard.as_ref() else {
        return (None, None, None, String::new());
    };
    let running_for = if complete { None } else { shell.command_elapsed() };
    let exit_code = if complete { shell.last_exit_code } else { None };
    let cwd = Some(shell.current_cwd().to_path_buf());
    let pointer = scratch_pointer(shell.output_truncated, shell.scratch_path());
    (running_for, exit_code, cwd, pointer)
}

fn scratch_pointer(output_truncated: bool, scratch_path: Option<&Path>) -> String {
    match (output_truncated, scratch_path) {
        (true, Some(path)) => format!(
            "\n\n---\n[Output was truncated to fit context. The earlier (dropped) output was \
             saved to:\n{}\nRead it with ReadFiles or grep it via BashCommand.]\n---",
            path.display()
        ),
        _ => String::new(),
    }
}

/// Render the final cached output of an exited background shell.
///
/// `status_check` is allowed to "consume" the tombstone and return the trailing
/// output exactly once. Send-style actions (`send_text`, `send_specials`,
/// `send_ascii`) cannot interact with a dead shell, so we return an explicit
/// error that still includes the captured output so the agent can recover state.
fn finalize_tombstone(
    id: &str,
    tombstone: ExitedShellInfo,
    action: &BashCommandAction,
) -> Result<String> {
    let ExitedShellInfo {
        last_command,
        final_output,
        exit_code,
        cwd,
        output_truncated,
        scratch_path,
        ..
    } = tombstone;
    match action {
        // A dead shell's turn is over: Screen/WaitForTurn hand back the final
        // captured output as the snapshot, exactly like a status check.
        BashCommandAction::StatusCheck { .. }
        | BashCommandAction::Screen { .. }
        | BashCommandAction::WaitForTurn { .. } => {
            let rendered = wcgw_incremental_text(final_output.as_ref(), "");
            let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();
            // Build a compact status block matching `get_status` for a finished bg shell.
            let mut status = "\n\n---\n\n".to_string();
            let _ = writeln!(status, "bg_command_id = {id}");
            status.push_str("status = process exited\n");
            if let Some(code) = exit_code {
                let _ = writeln!(status, "exit code = {code}");
            }
            let _ = writeln!(status, "cwd = {}", cwd.display());
            let pointer = scratch_pointer(output_truncated, scratch_path.as_deref());
            Ok(format!("{rendered}{}{pointer}", status.trim_end()))
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
    mut delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    debug!("Processing StatusCheck action (verbose={verbose}, scrollback={scrollback_lines:?})");

    // Pick the shell we're going to inspect: bg shell when bg_command_id was provided,
    // otherwise fall back to the main interactive shell.
    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));

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
        let mut manager = lock_session_store();
        let error = format!(
            "No command is currently running, so there's nothing to check. The previous \
             command already finished and its output was returned when it completed. Start a \
             new command, or pass a bg_command_id if you launched one in the background.\n{}",
            manager.get_running_info(&bash_state.current_thread_id)
        );
        return Err(WinxError::CommandExecutionError(error));
    }

    // Read output with patience handling - this IS a status check
    let response = wait_for_output(
        bash_state,
        &shell_arc,
        timeout_s,
        is_bg,
        bg_id,
        true,
        delivery_cursor.as_deref_mut(),
    )
    .await?;

    // Inter-call dedup hashes the cumulative PTY buffer, while the response body
    // itself stays incremental. Hashing the incremental body made the first empty
    // poll after real output look "different" again and emit a blank response.
    if !verbose && scrollback_lines.is_none() {
        let (fingerprint, running_for, running, exit_code, cwd) = {
            let guard = shell_arc.lock().await;
            let Some(bash) = guard.as_ref() else {
                return Err(WinxError::BashStateNotInitialized);
            };
            (
                PtyShell::fingerprint(&bash.output_snapshot()),
                bash.command_elapsed().map(|elapsed| format!("{} seconds", elapsed.as_secs())),
                bash.command_running,
                (!bash.command_running).then_some(bash.last_exit_code).flatten(),
                bash.current_cwd().to_path_buf(),
            )
        };
        let previous_hash = match delivery_cursor.as_deref_mut() {
            Some(cursor) => cursor.last_returned_hash.replace(fingerprint),
            None => {
                let mut guard = shell_arc.lock().await;
                guard.as_mut().and_then(|bash| bash.last_returned_hash.replace(fingerprint))
            }
        };
        if previous_hash == Some(fingerprint) {
            let status = get_status(
                bash_state,
                is_bg,
                bg_id,
                running,
                running_for.as_deref(),
                exit_code,
                Some(&cwd),
            );
            return Ok(format!("no new output since last check{status}"));
        }
    } else if !verbose {
        // Still record the hash so subsequent non-scrollback calls can dedup.
        let fingerprint = {
            let guard = shell_arc.lock().await;
            guard.as_ref().map(|bash| PtyShell::fingerprint(&bash.output_snapshot()))
        };
        if let Some(fingerprint) = fingerprint {
            if let Some(cursor) = delivery_cursor.as_deref_mut() {
                cursor.last_returned_hash = Some(fingerprint);
            } else {
                let mut guard = shell_arc.lock().await;
                if let Some(bash) = guard.as_mut() {
                    bash.last_returned_hash = Some(fingerprint);
                }
            }
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

/// Execute `Screen` — a stable, point-in-time snapshot of a shell's live
/// terminal screen (consolidated grid, ANSI stripped). No waiting, no dedup;
/// the foundation for reading an interactive TUI's current frame.
async fn execute_screen(
    bash_state: &BashState,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    lines: Option<usize>,
    diff: bool,
    mut delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));
    let max_lines = lines.unwrap_or(0);

    if diff {
        let (update, is_running, in_alt, cursor, running_for, exit_code, cwd) = {
            let mut guard = shell_arc.lock().await;
            match guard.as_mut() {
                Some(bash) => {
                    bash.poll_output_nonblocking();
                    let running = bash.command_running;
                    let update = if let Some(cursor) = delivery_cursor.as_deref_mut() {
                        cursor.sync_generation(bash.command_generation());
                        cursor.screen_update(bash.live_snapshot(max_lines), SCREEN_DIFF_THRESHOLD)
                    } else {
                        bash.live_snapshot_diff(max_lines, SCREEN_DIFF_THRESHOLD)
                    };
                    (
                        update,
                        running,
                        bash.live_in_alt_screen(),
                        bash.live_cursor_position(),
                        running.then(|| bash.command_elapsed()).flatten(),
                        (!running).then_some(bash.last_exit_code).flatten(),
                        Some(bash.current_cwd().to_path_buf()),
                    )
                }
                None => (ScreenUpdate::Full(Vec::new()), false, false, (0, 0), None, None, None),
            }
        };
        let (crow, ccol) = cursor;
        let alt = if in_alt { " [alt-screen]" } else { "" };
        let running_for = running_for.map(|elapsed| format!("{} seconds", elapsed.as_secs()));
        let status = get_status(
            bash_state,
            is_bg,
            bg_id,
            is_running,
            running_for.as_deref(),
            exit_code,
            cwd.as_deref(),
        );
        let body = match update {
            ScreenUpdate::Unchanged => "(no change since last screen)".to_string(),
            ScreenUpdate::Diff(changed) => {
                let mut out = String::from("(changed lines only)\n");
                for (row, content) in changed {
                    let _ = writeln!(out, "{row:>4}: {content}");
                }
                out
            }
            ScreenUpdate::Full(snap) => {
                let joined = snap.join("\n");
                if joined.trim().is_empty() {
                    "(screen is empty)".to_string()
                } else {
                    truncate_to_token_budget(&joined, MAX_OUTPUT_TOKENS).into_owned()
                }
            }
        };
        return Ok(format!(
            "--- live screen{alt} [cursor row={crow} col={ccol}] (diff) ---\n{body}{status}"
        ));
    }

    let (snapshot, is_running, in_alt, cursor, running_for, exit_code, cwd) = {
        let mut guard = shell_arc.lock().await;
        match guard.as_mut() {
            Some(bash) => {
                bash.poll_output_nonblocking();
                let running = bash.command_running;
                (
                    bash.live_snapshot(max_lines),
                    running,
                    bash.live_in_alt_screen(),
                    bash.live_cursor_position(),
                    running.then(|| bash.command_elapsed()).flatten(),
                    (!running).then_some(bash.last_exit_code).flatten(),
                    Some(bash.current_cwd().to_path_buf()),
                )
            }
            None => (Vec::new(), false, false, (0, 0), None, None, None),
        }
    };

    let joined = snapshot.join("\n");
    let body = if joined.trim().is_empty() {
        "(screen is empty)".to_string()
    } else {
        truncate_to_token_budget(&joined, MAX_OUTPUT_TOKENS).into_owned()
    };
    let alt = if in_alt { " [alt-screen]" } else { "" };
    let (crow, ccol) = cursor;
    let running_for = running_for.map(|elapsed| format!("{} seconds", elapsed.as_secs()));
    let status = get_status(
        bash_state,
        is_bg,
        bg_id,
        is_running,
        running_for.as_deref(),
        exit_code,
        cwd.as_deref(),
    );
    Ok(format!("--- live screen{alt} [cursor row={crow} col={ccol}] ---\n{body}{status}"))
}

/// Why [`execute_wait_for_turn`] should stop polling, or `None` to keep waiting.
///
/// Extracted as a pure function so the (subtle) exit logic is unit-testable
/// without driving a real PTY. `busy_for` is how long we've read `Busy`
/// *continuously*: a TUI that is actively working never holds a stable screen
/// (its spinner repaints every frame), so we confirm `Busy` by elapsed duration,
/// not by `stable_for` — that was exactly why a busy child used to pin the caller
/// for the whole `hard_cap` (up to 600s).
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn wait_turn_outcome(
    state: crate::state::turn::TurnState,
    alive: bool,
    activity: bool,
    seen_busy: bool,
    stable_for: Duration,
    busy_for: Duration,
    settle: Duration,
    quiet: Duration,
    timed_out: bool,
    wait_through_busy: bool,
) -> Option<&'static str> {
    use crate::state::turn::TurnState;
    if !alive {
        return Some("exited");
    }
    let ready = match state {
        TurnState::Busy => false,
        // A short settle is enough once we saw it go Busy; otherwise wait the
        // full quiet window so a slow first token isn't read as "done".
        TurnState::AwaitingInput | TurnState::AwaitingApproval => {
            activity && stable_for >= if seen_busy { settle } else { quiet }
        }
        TurnState::Unknown => activity && stable_for >= quiet,
    };
    if ready {
        return Some("ready");
    }
    // Early-out on a confirmed-busy turn instead of blocking until the hard cap.
    // A settled `busy` reading is a valid answer on its own (the tool documents
    // `busy` as a return state); the caller can poll again to keep watching. This
    // is the fix for a parent that "waits forever" on a long-running child.
    if !wait_through_busy && state == TurnState::Busy && busy_for >= settle {
        return Some("busy");
    }
    if timed_out {
        return Some("timeout");
    }
    None
}

/// Execute `WaitForTurn` — wait for an interactive TUI's turn.
///
/// Polls the live screen, combining a per-app recognizer (claude/codex/auto)
/// with a generic quiescence window: the turn is "ready" when the recognizer
/// reports awaiting-input/approval (after a short settle) or, for an unknown
/// TUI, when the screen simply stops changing for `quiet`. By default it also
/// returns as soon as `Busy` is confirmed (see [`wait_turn_outcome`]); pass
/// `wait_through_busy` to block through busy until ready. Returns the stable
/// snapshot plus the detected state.
#[allow(clippy::too_many_arguments)]
async fn execute_wait_for_turn(
    bash_state: &BashState,
    bg_shell: Option<SharedPtyShell>,
    is_bg: bool,
    bg_id: Option<&str>,
    recognizer_hint: Option<&str>,
    quiet_ms: Option<u64>,
    timeout_seconds: Option<f32>,
    lines: Option<usize>,
    wait_through_busy: bool,
) -> Result<String> {
    use crate::state::turn::{recognizer_for, TurnState};

    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));
    let recognizer = recognizer_for(recognizer_hint.unwrap_or("auto"));
    let quiet = Duration::from_millis(quiet_ms.unwrap_or(600).clamp(50, 10_000));
    let settle = quiet.min(Duration::from_millis(300));
    let hard_cap =
        Duration::from_secs_f64(f64::from(timeout_seconds.unwrap_or(30.0)).clamp(0.5, 600.0));
    let max_lines = lines.unwrap_or(0);
    let poll = Duration::from_millis(120);
    // Grace period so a freshly-prompted app has time to react before we'd
    // otherwise mistake the *pre-input* idle screen for a finished turn.
    let warmup = Duration::from_millis(2500);

    let start = Instant::now();
    let mut last_hash: Option<u64> = None;
    let mut initial_hash: Option<u64> = None;
    let mut stable_since = Instant::now();
    let mut seen_busy = false;
    let mut busy_since: Option<Instant> = None;

    loop {
        let (snapshot, in_alt, alive, running, running_for, exit_code, cwd) = {
            let mut guard = shell_arc.lock().await;
            match guard.as_mut() {
                Some(bash) => {
                    bash.poll_output_nonblocking();
                    let running = bash.command_running;
                    (
                        bash.live_snapshot(max_lines),
                        bash.live_in_alt_screen(),
                        bash.is_alive(),
                        running,
                        running.then(|| bash.command_elapsed()).flatten(),
                        (!running).then_some(bash.last_exit_code).flatten(),
                        Some(bash.current_cwd().to_path_buf()),
                    )
                }
                None => (Vec::new(), false, false, false, None, None, None),
            }
        };

        let hash = PtyShell::fingerprint(&snapshot.join("\n"));
        if initial_hash.is_none() {
            initial_hash = Some(hash);
        }
        if Some(hash) != last_hash {
            last_hash = Some(hash);
            stable_since = Instant::now();
        }

        let state = recognizer.detect(&snapshot);
        if state == TurnState::Busy {
            seen_busy = true;
            if busy_since.is_none() {
                busy_since = Some(Instant::now());
            }
        } else {
            busy_since = None;
        }
        let busy_for = busy_since.map_or(Duration::ZERO, |since| since.elapsed());
        let stable_for = stable_since.elapsed();
        // Don't call it "done" until something actually happened since we began
        // waiting: the app went Busy, the screen changed from the first frame we
        // saw, or the warmup elapsed (instant reply / nothing to do).
        let activity = seen_busy || Some(hash) != initial_hash || start.elapsed() >= warmup;
        let timed_out = start.elapsed() >= hard_cap;
        if let Some(reason) = wait_turn_outcome(
            state,
            alive,
            activity,
            seen_busy,
            stable_for,
            busy_for,
            settle,
            quiet,
            timed_out,
            wait_through_busy,
        ) {
            let joined = snapshot.join("\n");
            let body = if joined.trim().is_empty() {
                "(screen is empty)".to_string()
            } else {
                truncate_to_token_budget(&joined, MAX_OUTPUT_TOKENS).into_owned()
            };
            let alt = if in_alt { " [alt-screen]" } else { "" };
            let header = format!(
                "--- turn: {} ({}, recognizer={}, waited {:.1}s){} ---",
                state.as_str(),
                reason,
                recognizer.name(),
                start.elapsed().as_secs_f64(),
                alt
            );
            let running_for = running_for.map(|elapsed| format!("{} seconds", elapsed.as_secs()));
            let status = get_status(
                bash_state,
                is_bg,
                bg_id,
                running,
                running_for.as_deref(),
                exit_code,
                cwd.as_deref(),
            );
            return Ok(format!("{header}\n{body}{status}"));
        }

        tokio::time::sleep(poll).await;
    }
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
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    debug!(bytes = text.len(), submit, "Processing SendText action");

    // Validate - matches WCGW Python
    if text.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_text cannot be empty".to_string(),
        ));
    }

    // Get the target shell
    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));

    // Send text in chunks of 128 characters - matches WCGW Python exactly
    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(bash)?;

        // Send in chunks - matches WCGW Python: for i in range(0, len(command_data.send_text), 128)
        send_utf8_in_byte_chunks(bash, text, TEXT_CHUNK_SIZE)?;
    }

    // Only submit when explicitly asked. The Enter goes as a separate, delayed
    // write so Ink TUIs (Claude Code) don't swallow a CR glued to the text — a
    // bare CR there is otherwise treated as a soft newline and never submits.
    if submit {
        submit_enter(&shell_arc).await?;
    }

    // Wait for output
    wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false, delivery_cursor).await
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
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    debug!("Processing SendSpecials action: {keys:?} (submit={submit})");

    // Validate - matches WCGW Python
    if keys.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_specials cannot be empty".to_string(),
        ));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(bash)?;

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
                    // Ctrl-D is EOF, not an interrupt. A process may legitimately
                    // stay alive after EOF, so do not emit "Failure interrupting".
                    bash.send_eof().map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send Ctrl+D: {e}"))
                    })?;
                }
                SpecialKey::CtrlZ => {
                    // Ctrl+Z = SIGTSTP (suspend) - ASCII 0x1a
                    bash.send_suspend().map_err(|e| {
                        WinxError::CommandExecutionError(format!("Failed to send Ctrl+Z: {e}"))
                    })?;
                }
            }
        }
    }

    // Submit as a separate, slightly-delayed write (see `submit_enter`) so an
    // Ink TUI doesn't drop the CR.
    if submit {
        submit_enter(&shell_arc).await?;
    }

    // NOTE: wcgw treats a bare Enter as a status check and applies its
    // patience loop. We deliberately diverge: for a driving agent (e.g.,
    // pushing Enter to submit text in a TUI) the patience loop swallows the
    // immediate response. Callers that want patience semantics should use the
    // explicit `status_check` action instead.

    // Wait for output
    let mut output =
        wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false, delivery_cursor)
            .await?;

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
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    debug!(bytes = ascii_codes.len(), submit, "Processing SendAscii action");

    // Validate - matches WCGW Python
    if ascii_codes.is_empty() {
        return Err(WinxError::CommandExecutionError(
            "Failure: send_ascii cannot be empty".to_string(),
        ));
    }

    let shell_arc = bg_shell.unwrap_or_else(|| main_shell(bash_state));
    let mut is_interrupt = false;

    {
        let mut guard = shell_arc.lock().await;

        let bash = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(bash)?;

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
    }

    // Submit as a separate, slightly-delayed write (see `submit_enter`) so an
    // Ink TUI doesn't drop the CR.
    if submit {
        submit_enter(&shell_arc).await?;
    }

    // Same divergence from wcgw as in `execute_send_specials`: send_ascii [10]
    // or [13] is treated as a direct write, not a status check. Callers that
    // need patience-aware reads should use `status_check`.

    // Wait for output
    let mut output =
        wait_for_output(bash_state, &shell_arc, timeout_s, is_bg, bg_id, false, delivery_cursor)
            .await?;

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
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    debug!(bytes = command.len(), "Executing command in background");

    // Start a new background shell - matches WCGW Python bash_state.start_new_bg_shell
    let restricted_mode =
        matches!(bash_state.bash_command_mode.bash_mode, crate::types::BashMode::RestrictedMode);

    // Build the shell OFF the tokio worker AND outside the manager lock:
    // PtyShell::new forks+execs and blocks ~300ms on prompt init. The old
    // start_new_shell did all of that under the std::Mutex on the executor thread.
    let bg_id = {
        let cwd = bash_state.cwd.clone();
        let shell = tokio::task::spawn_blocking(move || PtyShell::new(&cwd, restricted_mode))
            .await
            .map_err(|e| {
                WinxError::CommandExecutionError(format!("bg shell init task failed: {e}"))
            })?
            .map_err(|e| {
                WinxError::CommandExecutionError(format!("Failed to start background shell: {e}"))
            })?;
        lock_session_store().register_shell(&bash_state.current_thread_id, shell)?
    };

    // Get the shell
    let shell_arc = {
        let manager = lock_session_store();
        manager.get_shell(&bash_state.current_thread_id, &bg_id).ok_or_else(|| {
            WinxError::CommandExecutionError("Failed to get background shell".to_string())
        })?
    };

    // Send command via the same PTY path used by foreground execute_command.
    let scratch_root = bash_state.workspace_root.clone();
    let send_result = {
        let mut guard = shell_arc.lock().await;
        guard.as_mut().map(|bash| {
            // Offload an over-long bg output's dropped head too (send_command resets
            // the per-command scratch state; this points it at the workspace).
            bash.set_scratch_root(&scratch_root);
            bash.send_command(command)
        })
    };
    let Some(send_result) = send_result else {
        lock_session_store().remove_shell(&bg_id);
        return Err(WinxError::BashStateNotInitialized);
    };
    if let Err(error) = send_result {
        // Registration consumes a capacity slot before the PTY write. Roll it
        // back on failure; no reaper has been spawned yet to clean it for us.
        lock_session_store().remove_shell(&bg_id);
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to send bg command: {error}"
        )));
    }
    debug!("bg[{}]: send_command returned, replying with bg_command_id", bg_id);

    spawn_background_reaper(bash_state.current_thread_id.clone(), bg_id.clone());

    let _ = (timeout_s, delivery_cursor);
    let _ = shell_arc;
    Ok(get_status(bash_state, true, Some(&bg_id), true, None, None, Some(&bash_state.cwd)))
}

#[cfg(test)]
mod wait_turn_tests {
    use super::wait_turn_outcome;
    use crate::state::turn::TurnState;
    use std::time::Duration;

    const SETTLE: Duration = Duration::from_millis(300);
    const QUIET: Duration = Duration::from_millis(600);

    fn call(
        state: TurnState,
        busy_for: Duration,
        stable_for: Duration,
        seen_busy: bool,
        timed_out: bool,
        wait_through_busy: bool,
    ) -> Option<&'static str> {
        // `activity` is true in these cases (something happened) unless noted.
        wait_turn_outcome(
            state,
            true,
            true,
            seen_busy,
            stable_for,
            busy_for,
            SETTLE,
            QUIET,
            timed_out,
            wait_through_busy,
        )
    }

    #[test]
    fn confirmed_busy_returns_early_instead_of_blocking_to_timeout() {
        // The bug: a busy child used to pin the caller until hard_cap. Now a
        // busy reading held for >= settle returns "busy" with NO timeout.
        let out = call(TurnState::Busy, SETTLE, Duration::ZERO, true, false, false);
        assert_eq!(out, Some("busy"));
    }

    #[test]
    fn busy_not_yet_confirmed_keeps_waiting() {
        // Seen busy for less than settle (one-frame flicker): keep polling.
        let out =
            call(TurnState::Busy, Duration::from_millis(100), Duration::ZERO, true, false, false);
        assert_eq!(out, None);
    }

    #[test]
    fn wait_through_busy_blocks_through_busy_until_timeout() {
        // Opt-out preserves the old contract: busy never returns early...
        let out = call(TurnState::Busy, SETTLE * 10, Duration::ZERO, true, false, true);
        assert_eq!(out, None);
        // ...only the hard cap ends it.
        let out = call(TurnState::Busy, SETTLE * 10, Duration::ZERO, true, true, true);
        assert_eq!(out, Some("timeout"));
    }

    #[test]
    fn awaiting_input_after_busy_is_ready_on_short_settle() {
        let out = call(TurnState::AwaitingInput, Duration::ZERO, SETTLE, true, false, false);
        assert_eq!(out, Some("ready"));
    }

    #[test]
    fn awaiting_input_without_prior_busy_needs_full_quiet() {
        // No prior busy: a short settle is not enough, must wait the quiet window.
        let short = call(TurnState::AwaitingInput, Duration::ZERO, SETTLE, false, false, false);
        assert_eq!(short, None);
        let full = call(TurnState::AwaitingInput, Duration::ZERO, QUIET, false, false, false);
        assert_eq!(full, Some("ready"));
    }

    #[test]
    fn dead_shell_reports_exited_even_if_busy() {
        let out = wait_turn_outcome(
            TurnState::Busy,
            false,
            true,
            true,
            Duration::ZERO,
            SETTLE,
            SETTLE,
            QUIET,
            false,
            false,
        );
        assert_eq!(out, Some("exited"));
    }

    #[test]
    fn nothing_happening_keeps_waiting() {
        let out = call(TurnState::Unknown, Duration::ZERO, Duration::ZERO, false, false, false);
        assert_eq!(out, None);
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_wait_for_seconds, strip_tail_pipe_impl, truncate_to_token_budget};

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

    #[test]
    fn requested_wait_is_not_silently_capped() {
        assert!((effective_wait_for_seconds(Some(120.0)) - 120.0).abs() < f64::EPSILON);
        assert!((effective_wait_for_seconds(Some(150.0)) - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn token_budget_applies_below_the_byte_fallback_cap() {
        let input = "alpha beta gamma delta ".repeat(100);
        let rendered = truncate_to_token_budget(&input, 20);

        assert!(rendered.starts_with("(...truncated)\n"));
        assert_ne!(rendered.as_ref(), input);
        let tail = rendered.strip_prefix("(...truncated)\n").unwrap_or_default();
        let tail_tokens = crate::utils::encoder::encode_ids(tail);
        assert!(tail_tokens.as_ref().is_some_and(|tokens| tokens.len() <= 19));
    }
}
