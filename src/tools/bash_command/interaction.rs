use std::time::Duration;

use tracing::debug;

use super::output::{insert_note_before_status, wait_for_output};
use super::{main_shell, send_utf8_in_byte_chunks, SharedPtyShell, ShellDeliveryCursor};
use crate::errors::{Result, WinxError};
use crate::runtime::BashCommandRuntimeResult;
use crate::state::bash_state::BashState;
use crate::state::pty::PtyShell;
use crate::types::SpecialKey;

const TEXT_CHUNK_SIZE: usize = 128;
const SUBMIT_NUDGE_DELAY: Duration = Duration::from_millis(40);

async fn submit_enter(shell: &SharedPtyShell) -> Result<()> {
    tokio::time::sleep(SUBMIT_NUDGE_DELAY).await;
    let mut guard = shell.lock().await;
    let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
    ensure_interactive_target(shell)?;
    shell
        .send_special_key("Enter")
        .map_err(|error| WinxError::CommandExecutionError(format!("Failed to submit: {error}")))?;
    Ok(())
}

fn ensure_interactive_target(shell: &mut PtyShell) -> Result<()> {
    shell.poll_output_nonblocking();
    if shell.command_running {
        Ok(())
    } else {
        Err(WinxError::InvalidInput(
            "No interactive command is running. Start an allowed command first; send_text/send_ascii/send_specials cannot execute commands in an idle shell."
                .to_string(),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_send_text(
    bash_state: &mut BashState,
    text: &str,
    submit: bool,
    background_shell: Option<SharedPtyShell>,
    is_background: bool,
    background_id: Option<&str>,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    debug!(bytes = text.len(), submit, "Processing SendText action");
    if text.is_empty() {
        return Err(WinxError::InvalidInput(
            "send_text cannot be empty. To press Enter alone, use send_specials: [\"Enter\"]; \
             to submit typed text, set submit: true."
                .to_string(),
        ));
    }

    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    {
        let mut guard = shell.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(shell)?;
        send_utf8_in_byte_chunks(shell, text, TEXT_CHUNK_SIZE)?;
    }
    if submit {
        submit_enter(&shell).await?;
    }

    wait_for_output(
        bash_state,
        &shell,
        timeout_secs,
        is_background,
        background_id,
        false,
        delivery_cursor,
        compact_output,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_send_specials(
    bash_state: &mut BashState,
    keys: &[SpecialKey],
    submit: bool,
    background_shell: Option<SharedPtyShell>,
    is_background: bool,
    background_id: Option<&str>,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    debug!("Processing SendSpecials action: {keys:?} (submit={submit})");
    if keys.is_empty() {
        return Err(WinxError::InvalidInput("send_specials cannot be empty".to_string()));
    }

    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    let mut is_interrupt = false;
    {
        let mut guard = shell.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(shell)?;

        for key in keys {
            match key {
                SpecialKey::KeyUp => shell.send_special_key("KeyUp").map_err(|error| {
                    WinxError::CommandExecutionError(format!("Failed to send KeyUp: {error}"))
                })?,
                SpecialKey::KeyDown => {
                    shell.send_special_key("KeyDown").map_err(|error| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyDown: {error}"))
                    })?;
                }
                SpecialKey::KeyLeft => {
                    shell.send_special_key("KeyLeft").map_err(|error| {
                        WinxError::CommandExecutionError(format!("Failed to send KeyLeft: {error}"))
                    })?;
                }
                SpecialKey::KeyRight => {
                    shell.send_special_key("KeyRight").map_err(|error| {
                        WinxError::CommandExecutionError(format!(
                            "Failed to send KeyRight: {error}"
                        ))
                    })?;
                }
                SpecialKey::Enter => shell.send_special_key("Enter").map_err(|error| {
                    WinxError::CommandExecutionError(format!("Failed to send Enter: {error}"))
                })?,
                SpecialKey::CtrlC => {
                    shell.send_interrupt().map_err(|error| {
                        WinxError::CommandExecutionError(format!(
                            "Failed to send interrupt: {error}"
                        ))
                    })?;
                    is_interrupt = true;
                }
                SpecialKey::CtrlD => shell.send_eof().map_err(|error| {
                    WinxError::CommandExecutionError(format!("Failed to send Ctrl+D: {error}"))
                })?,
                SpecialKey::CtrlZ => shell.send_suspend().map_err(|error| {
                    WinxError::CommandExecutionError(format!("Failed to send Ctrl+Z: {error}"))
                })?,
            }
        }
    }

    if submit {
        submit_enter(&shell).await?;
    }
    let mut result = wait_for_output(
        bash_state,
        &shell,
        timeout_secs,
        is_background,
        background_id,
        false,
        delivery_cursor,
        compact_output,
        None,
    )
    .await?;
    if is_interrupt && result.result.state.is_running() {
        let note = "\n---\n----\nFailure interrupting.\nYou may want to try Ctrl-c again or program specific exit interactive commands.\n";
        insert_note_before_status(bash_state, &mut result, note);
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_send_ascii(
    bash_state: &mut BashState,
    ascii_codes: &[u8],
    submit: bool,
    background_shell: Option<SharedPtyShell>,
    is_background: bool,
    background_id: Option<&str>,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    debug!(bytes = ascii_codes.len(), submit, "Processing SendAscii action");
    if ascii_codes.is_empty() {
        return Err(WinxError::InvalidInput("send_ascii cannot be empty".to_string()));
    }

    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    let mut is_interrupt = false;
    {
        let mut guard = shell.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        ensure_interactive_target(shell)?;
        for &code in ascii_codes {
            shell.send_bytes(&[code]).map_err(|error| {
                WinxError::CommandExecutionError(format!("Failed to write ASCII code: {error}"))
            })?;
            if code == 3 {
                is_interrupt = true;
            }
        }
    }

    if submit {
        submit_enter(&shell).await?;
    }
    let mut result = wait_for_output(
        bash_state,
        &shell,
        timeout_secs,
        is_background,
        background_id,
        false,
        delivery_cursor,
        compact_output,
        None,
    )
    .await?;
    if is_interrupt && result.result.state.is_running() {
        let note = "\n---\n----\nFailure interrupting.\nYou may want to try Ctrl-c again or program specific exit interactive commands.\n";
        insert_note_before_status(bash_state, &mut result, note);
    }
    Ok(result)
}
