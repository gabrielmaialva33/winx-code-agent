//! `BashCommand` facade and action dispatcher.
//!
//! Execution, output/status rendering, interactive input, and TUI driving live
//! in focused submodules under `tools/bash_command/`.

mod execution;
mod interaction;
mod output;
mod tui;

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{error, info};

pub use super::background_shell::{BackgroundShellManager, ExitedShellInfo};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, EmbeddedShellRuntime, ShellRuntime, ShellTarget};
use crate::state::bash_state::BashState;
use crate::state::live_terminal::ScreenUpdate;
use crate::state::pty::PtyShell;
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction};

use execution::execute_command;
use interaction::{execute_send_ascii, execute_send_specials, execute_send_text};
use output::{execute_status_check, finalize_tombstone};
use tui::{execute_screen, execute_wait_for_turn};

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

const DEFAULT_TIMEOUT: f64 = 5.0;

fn effective_wait_for_seconds(wait_for_seconds: Option<f32>) -> f64 {
    wait_for_seconds.map_or(DEFAULT_TIMEOUT, |seconds| f64::from(seconds).max(0.0))
}

fn send_utf8_in_byte_chunks(shell: &mut PtyShell, text: &str, chunk_size: usize) -> Result<()> {
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + chunk_size).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end =
                text[start..].char_indices().nth(1).map_or(text.len(), |(index, _)| start + index);
        }
        shell.send_text(&text[start..end]).map_err(|error| {
            WinxError::CommandExecutionError(format!("Failed to write PTY input: {error}"))
        })?;
        start = end;
    }
    Ok(())
}

/// Handle a `BashCommand` using the embedded runtime.
pub async fn handle_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    handle_tool_call_with_runtime(&EmbeddedShellRuntime, bash_state, command).await
}

/// Execute a `BashCommand` through the selected shell runtime.
pub async fn handle_tool_call_with_runtime(
    runtime: &dyn ShellRuntime,
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    runtime.run_action(bash_state, command).await
}

pub(crate) async fn handle_embedded_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    handle_embedded_tool_call_inner(bash_state, command, None).await
}

pub(crate) async fn handle_embedded_tool_call_with_cursor(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: &Arc<Mutex<ShellDeliveryCursor>>,
) -> Result<String> {
    let mut delivery_cursor = delivery_cursor.lock().await;
    handle_embedded_tool_call_inner(bash_state, command, Some(&mut delivery_cursor)).await
}

#[tracing::instrument(level = "info", skip(bash_state, command, delivery_cursor))]
async fn handle_embedded_tool_call_inner(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let action_kind = match &command.action_json {
        BashCommandAction::Command { .. } => "command",
        BashCommandAction::StatusCheck { .. } => "status_check",
        BashCommandAction::SendText { .. } => "send_text",
        BashCommandAction::SendSpecials { .. } => "send_specials",
        BashCommandAction::SendAscii { .. } => "send_ascii",
        BashCommandAction::Screen { .. } => "screen",
        BashCommandAction::WaitForTurn { .. } => "wait_for_turn",
    };
    info!(thread_id = %command.thread_id, action = action_kind, "BashCommand tool called");

    let thread_id = normalize_thread_id(&command.thread_id);
    if thread_id.is_empty() {
        error!("Empty thread_id provided in BashCommand");
        return Err(WinxError::ThreadIdMismatch(
            "Error: No saved bash state found for thread ID \"\". Please initialize first with this ID."
                .to_string(),
        ));
    }

    let mut local_state = {
        let guard = bash_state.lock().await;
        let Some(state) = &*guard else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };
        state.clone()
    };

    if thread_id != local_state.current_thread_id {
        let loaded = local_state.load_state_from_disk(&thread_id).map_err(|error| {
            WinxError::CommandExecutionError(format!(
                "Failed to load saved bash state for thread_id `{thread_id}`: {error}"
            ))
        })?;
        if !loaded {
            return Err(WinxError::ThreadIdMismatch(format!(
                "Error: No saved bash state found for thread_id `{thread_id}`. Please initialize first with this ID."
            )));
        }
        if let Some(state) = bash_state.lock().await.as_mut() {
            *state = local_state.clone();
        }
    }

    lock_session_store().bind_main(&local_state.current_thread_id, &local_state.pty_shell);
    let timeout_secs = effective_wait_for_seconds(command.wait_for_seconds);
    let result =
        execute_bash_action(&mut local_state, &command.action_json, timeout_secs, delivery_cursor)
            .await;

    if let Some(state) = bash_state.lock().await.as_mut() {
        state.cwd.clone_from(&local_state.cwd);
    }

    match result {
        Ok(mut output) => {
            if let BashCommandAction::Command { ref command, .. } = command.action_json {
                let command = command.trim();
                if output.starts_with(command) {
                    output = output[command.len()..].to_string();
                }
            }
            Ok(output)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_lines)]
async fn execute_bash_action(
    bash_state: &mut BashState,
    action: &BashCommandAction,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
) -> Result<String> {
    let mut is_background = false;
    let mut background_id: Option<String> = None;

    let background_shell = match action {
        BashCommandAction::Command { .. } => None,
        BashCommandAction::StatusCheck { bg_command_id, .. }
        | BashCommandAction::SendText { bg_command_id, .. }
        | BashCommandAction::SendSpecials { bg_command_id, .. }
        | BashCommandAction::SendAscii { bg_command_id, .. }
        | BashCommandAction::Screen { bg_command_id, .. }
        | BashCommandAction::WaitForTurn { bg_command_id, .. } => {
            if let Some(id) = bg_command_id {
                let mut manager = lock_session_store();
                manager.prune_finished_shells();
                if let Some(shell) = manager.get_shell(&bash_state.current_thread_id, id) {
                    is_background = true;
                    background_id = Some(id.clone());
                    Some(shell)
                } else if let Some(tombstone) =
                    manager.peek_tombstone(&bash_state.current_thread_id, id)
                {
                    drop(manager);
                    return finalize_tombstone(id, tombstone, action);
                } else {
                    let error = format!(
                        "No shell found running with command id {}.\n{}",
                        id,
                        manager.get_running_info(&bash_state.current_thread_id)
                    );
                    return Err(WinxError::InvalidInput(error));
                }
            } else {
                None
            }
        }
    };

    match action {
        BashCommandAction::Command { command, is_background, allow_multi } => {
            execute_command(
                bash_state,
                command,
                *is_background,
                *allow_multi,
                timeout_secs,
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::StatusCheck { scrollback_lines, verbose, .. } => {
            execute_status_check(
                bash_state,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
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
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::SendSpecials { send_specials, submit, .. } => {
            execute_send_specials(
                bash_state,
                send_specials,
                *submit,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::SendAscii { send_ascii, submit, .. } => {
            execute_send_ascii(
                bash_state,
                send_ascii,
                *submit,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
            )
            .await
        }
        BashCommandAction::Screen { lines, diff, .. } => {
            execute_screen(
                bash_state,
                background_shell,
                is_background,
                background_id.as_deref(),
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
                background_shell,
                is_background,
                background_id.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::effective_wait_for_seconds;

    #[test]
    fn requested_wait_is_not_silently_capped() {
        assert!((effective_wait_for_seconds(Some(120.0)) - 120.0).abs() < f64::EPSILON);
        assert!((effective_wait_for_seconds(Some(150.0)) - 150.0).abs() < f64::EPSILON);
    }
}
