//! `BashCommand` facade and action dispatcher.
//!
//! Execution, output/status rendering, interactive input, and TUI driving live
//! in focused submodules under `tools/bash_command/`.

mod execution;
mod interaction;
mod output;
mod tui;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info};

pub use super::background_shell::{BackgroundShellManager, ExitedShellInfo};
use crate::errors::{Result, WinxError};
use crate::runtime::{
    lock_session_store, BashCommandRuntimeResult, EmbeddedShellRuntime, ShellActionOptions,
    ShellExecutionToken, ShellRuntime, ShellTarget,
};
use crate::state::bash_state::BashState;
use crate::state::live_terminal::ScreenUpdate;
use crate::state::pty::PtyShell;
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction};

use execution::execute_command;
use interaction::{execute_send_ascii, execute_send_specials, execute_send_text};
use output::{execute_status_check, finalize_tombstone};
use tui::{execute_screen, execute_wait_for_turn};

type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

/// Authoritative process state produced by the shell runtime. It is transported
/// separately from human-readable terminal output, so command text cannot spoof
/// orchestration state by printing Winx-looking markers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashProcessStatus {
    Running,
    Exited,
}

/// Machine-readable state of one `BashCommand` action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BashCommandState {
    pub process_status: BashProcessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_for_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_state: Option<crate::state::turn::TurnState>,
}

impl BashCommandState {
    pub const fn is_running(&self) -> bool {
        matches!(self.process_status, BashProcessStatus::Running)
    }
}

/// Human-readable output plus runtime-owned state. Only `state` drives MCP
/// orchestration; `output` is presentation and may contain arbitrary child text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BashCommandResult {
    pub output: String,
    pub state: BashCommandState,
}

pub(crate) fn runtime_rendered(
    mut body: String,
    legacy_status: &str,
    state: BashCommandState,
    compact_requested: bool,
    command_generation: Option<u64>,
    output_truncated: bool,
) -> BashCommandRuntimeResult {
    let compact_output = if compact_requested {
        Some(std::mem::take(&mut body))
    } else {
        body.push_str(legacy_status);
        None
    };
    BashCommandRuntimeResult {
        result: BashCommandResult { output: body, state },
        compact_output,
        command_generation,
        execution_token: None,
        generation_bound_actions: true,
        output_truncated,
    }
}

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

// Default block window when the client omits `wait_for_seconds`. Usage logs
// showed remote clients omit it and then burn one full LLM round-trip per 5 s
// re-check on long commands (runs of up to 18 consecutive polls); a larger
// window resolves most commands in a single call while staying far below the
// 120 s HTTP request timeout.
const DEFAULT_TIMEOUT: f64 = 15.0;
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

/// Backwards-compatible text-only API used by direct library callers.
pub async fn handle_tool_call_with_runtime(
    runtime: &dyn ShellRuntime,
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    Ok(handle_tool_call_with_runtime_detailed(runtime, bash_state, command).await?.output)
}

/// Execute a `BashCommand` while preserving the runtime-owned typed state.
pub async fn handle_tool_call_with_runtime_detailed(
    runtime: &dyn ShellRuntime,
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<BashCommandResult> {
    runtime.run_action(bash_state, command).await
}

pub(crate) async fn handle_embedded_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<BashCommandResult> {
    Ok(handle_embedded_tool_call_inner(bash_state, command, None, ShellActionOptions::default())
        .await?
        .result)
}

pub(crate) async fn handle_embedded_tool_call_detailed(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    handle_embedded_tool_call_inner(bash_state, command, None, options).await
}

pub(crate) async fn handle_embedded_tool_call_with_cursor_detailed(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: &Arc<Mutex<ShellDeliveryCursor>>,
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    let mut delivery_cursor = delivery_cursor.lock().await;
    handle_embedded_tool_call_inner(bash_state, command, Some(&mut delivery_cursor), options).await
}

#[tracing::instrument(level = "info", skip(bash_state, command, delivery_cursor))]
async fn handle_embedded_tool_call_inner(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
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

    let operation_barrier = lock_session_store().operation_barrier(&thread_id);
    let _operation = operation_barrier.read().await;

    if let Some(expected) = options.expected_execution.as_ref() {
        if expected.guardian_epoch == "embedded" {
            let current_epoch = local_state
                .pty_shell
                .lock()
                .await
                .as_ref()
                .map(|shell| format!("{:016x}", shell.incarnation()));
            if Some(expected.session_epoch.as_str()) != current_epoch.as_deref() {
                return Err(WinxError::InvalidInput(
                    "execution token belongs to a previous embedded shell incarnation".to_string(),
                ));
            }
        }
    }

    lock_session_store().bind_main(&local_state.current_thread_id, &local_state.pty_shell);
    let timeout_secs = effective_wait_for_seconds(command.wait_for_seconds);
    let result = execute_bash_action(
        &mut local_state,
        &command.action_json,
        timeout_secs,
        delivery_cursor,
        options,
    )
    .await;

    if let Some(state) = bash_state.lock().await.as_mut() {
        state.cwd.clone_from(&local_state.cwd);
    }

    match result {
        Ok(mut outcome) => {
            if let Some(generation) = outcome.command_generation {
                outcome.execution_token = Some(ShellExecutionToken {
                    guardian_epoch: "embedded".to_string(),
                    session_epoch: local_state.pty_shell.lock().await.as_ref().map_or_else(
                        || "uninitialized".to_string(),
                        |shell| format!("{:016x}", shell.incarnation()),
                    ),
                    generation,
                });
            }
            if let BashCommandAction::Command { ref command, .. } = command.action_json {
                let command = command.trim();
                if outcome.result.output.starts_with(command) {
                    outcome.result.output = outcome.result.output[command.len()..].to_string();
                }
                if outcome.compact_output.as_ref().is_some_and(|output| output.starts_with(command))
                {
                    let output = outcome.compact_output.take().unwrap_or_default();
                    outcome.compact_output = Some(output[command.len()..].to_string());
                }
            }
            Ok(outcome)
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
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
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
                    return finalize_tombstone(
                        bash_state,
                        id,
                        tombstone,
                        action,
                        options.compact_output,
                    );
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
                &options,
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
                options,
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
                options.compact_output,
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
                options.compact_output,
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
                options.compact_output,
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
                options.compact_output,
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
                options.compact_output,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_wait_for_seconds, runtime_rendered, BashCommandState, BashProcessStatus,
    };

    fn exited_state() -> BashCommandState {
        BashCommandState {
            process_status: BashProcessStatus::Exited,
            background_id: None,
            running_for_seconds: None,
            exit_code: Some(0),
            cwd: "/workspace".into(),
            turn_state: None,
        }
    }

    #[test]
    fn legacy_rendering_does_not_build_a_compact_payload_without_negotiation() {
        let result = runtime_rendered(
            "child output".to_string(),
            "\nstatus = process exited",
            exited_state(),
            false,
            Some(1),
            false,
        );
        assert_eq!(result.result.output, "child output\nstatus = process exited");
        assert_eq!(result.compact_output, None);
    }

    #[test]
    fn compact_rendering_is_built_directly_from_runtime_body() {
        let result = runtime_rendered(
            "child output".to_string(),
            "\nstatus = process exited",
            exited_state(),
            true,
            Some(1),
            false,
        );
        assert_eq!(result.compact_output.as_deref(), Some("child output"));
        assert!(result.result.output.is_empty());
    }

    #[test]
    fn requested_wait_is_not_silently_capped() {
        assert!((effective_wait_for_seconds(Some(120.0)) - 120.0).abs() < f64::EPSILON);
        assert!((effective_wait_for_seconds(Some(150.0)) - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn omitted_wait_uses_the_larger_default_window() {
        assert!((effective_wait_for_seconds(None) - 15.0).abs() < f64::EPSILON);
    }
}
