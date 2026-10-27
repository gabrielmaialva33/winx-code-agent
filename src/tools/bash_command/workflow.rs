use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::output::{render_status, status_state};
use super::{main_shell, runtime_rendered, send_utf8_in_byte_chunks, SharedPtyShell};
use crate::errors::{Result, WinxError};
use crate::runtime::workflow::{
    PromptDelivery, PromptOutcome, PromptReceipt, PromptRecord, WorkflowObservation,
};
use crate::runtime::{BashCommandRuntimeResult, ShellActionOptions};
use crate::state::bash_state::BashState;
use crate::state::pty::PtyShell;
use crate::state::turn::{recognizer_for, TurnInput, TurnState};
use crate::types::Modes;

const MAX_PROMPT_BYTES: usize = 16 * 1024;
const MAX_PROMPT_RECEIPTS: usize = 64;
const POLL: Duration = Duration::from_millis(20);
const QUIET: Duration = Duration::from_millis(300);

fn invalid(message: &str) -> WinxError {
    WinxError::InvalidInput(message.to_string())
}

fn validate_generation(shell: &PtyShell, options: &ShellActionOptions) -> Result<()> {
    let expected = options
        .expected_execution
        .as_ref()
        .map(|token| token.generation)
        .or(options.expected_generation);
    if expected.is_some_and(|expected| expected != shell.command_generation()) {
        return Err(invalid("workflow execution belongs to a replaced command generation"));
    }
    Ok(())
}

fn observe(shell: &mut PtyShell, hint: &str, include_output: bool) -> WorkflowObservation {
    let activity_seq = shell.poll_workflow_output();
    let screen = shell.live_snapshot(0);
    let title = shell.live_osc_title();
    let progress = shell.live_osc_progress();
    let explanation = recognizer_for(hint).explain_input(&TurnInput {
        screen: &screen,
        osc_title: &title,
        osc_progress: &progress,
    });
    WorkflowObservation {
        output: if include_output { shell.output_snapshot() } else { String::new() },
        output_included: include_output,
        screen,
        activity_seq,
        explanation,
        prompt_receipt: None,
    }
}

fn result(
    state: &BashState,
    shell: &PtyShell,
    background_id: Option<&str>,
    observation: WorkflowObservation,
    compact: bool,
) -> BashCommandRuntimeResult {
    let running = shell.command_running;
    let status = status_state(
        background_id,
        running,
        running.then(|| shell.command_elapsed()).flatten(),
        shell.last_exit_code,
        shell.current_cwd(),
        Some(observation.explanation.state),
    );
    let mut result = runtime_rendered(
        String::new(),
        &render_status(state, &status),
        status,
        compact,
        Some(shell.command_generation()),
        shell.output_truncated,
    );
    result.dropped_output_file = shell.scratch_path().map(std::path::Path::to_path_buf);
    result.workflow = Some(observation);
    result
}

pub(super) async fn execute_inspect(
    state: &BashState,
    background_shell: Option<SharedPtyShell>,
    background_id: Option<&str>,
    hint: Option<&str>,
    include_output: bool,
    options: &ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    let shell = background_shell.unwrap_or_else(|| main_shell(state));
    let mut guard = shell.lock().await;
    let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
    validate_generation(shell, options)?;
    let observation = observe(shell, hint.unwrap_or("auto"), include_output);
    Ok(result(state, shell, background_id, observation, options.compact_output))
}

fn validate_prompt(prompt_id: &str, text: Option<&str>, timeout: Option<f32>) -> Result<Duration> {
    if prompt_id.is_empty() || prompt_id.len() > 128 || prompt_id.contains('\0') {
        return Err(invalid("prompt_id must contain 1–128 UTF-8 bytes and no NUL"));
    }
    if text
        .is_some_and(|text| text.is_empty() || text.len() > MAX_PROMPT_BYTES || text.contains('\0'))
    {
        return Err(invalid("prompt text must contain 1–16384 UTF-8 bytes and no NUL"));
    }
    let timeout = timeout.unwrap_or(30.0);
    if !timeout.is_finite() || !(0.0..=30.0).contains(&timeout) {
        return Err(invalid("prompt timeout_seconds must be finite and between 0 and 30"));
    }
    Ok(Duration::from_secs_f32(timeout))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_prompt(
    state: &BashState,
    background_shell: Option<SharedPtyShell>,
    background_id: Option<&str>,
    prompt_id: &str,
    text: Option<&str>,
    hint: Option<&str>,
    timeout: Option<f32>,
    options: &ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    let timeout = validate_prompt(prompt_id, text, timeout)?;
    let shell = background_shell.unwrap_or_else(|| main_shell(state));
    let (incarnation, generation, hint) = {
        let mut guard = shell.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        validate_generation(shell, options)?;
        let hint = prepare_prompt(shell, state, prompt_id, text, hint).await?;
        (shell.incarnation(), shell.command_generation(), hint)
    };
    wait_prompt(
        state,
        &shell,
        background_id,
        prompt_id,
        &hint,
        (incarnation, generation),
        timeout,
        options.compact_output,
    )
    .await
}

async fn prepare_prompt(
    shell: &mut PtyShell,
    state: &BashState,
    prompt_id: &str,
    text: Option<&str>,
    hint: Option<&str>,
) -> Result<String> {
    if let Some(record) = shell.prompt_receipts.get(prompt_id) {
        if text
            .is_some_and(|text| <[u8; 32]>::from(Sha256::digest(text.as_bytes())) != record.digest)
        {
            return Err(invalid(
                "prompt_id was already used with different text; it will not be resent",
            ));
        }
        if hint.is_some_and(|hint| hint != record.recognizer) {
            return Err(invalid("an existing prompt receipt must use its original recognizer"));
        }
        return Ok(record.recognizer.clone());
    }
    let text = text.ok_or_else(|| {
        invalid("no receipt exists for prompt_id; text was absent, so no input was sent")
    })?;
    if !matches!(state.mode, Modes::Wcgw) {
        return Err(invalid("native agent prompts require wcgw mode"));
    }
    if shell.prompt_receipts.len() >= MAX_PROMPT_RECEIPTS {
        return Err(invalid(
            "this generation already retains 64 prompt receipts; no input was sent",
        ));
    }
    let hint = hint.unwrap_or("auto");
    shell.poll_output_nonblocking();
    if !shell.command_running {
        return Err(invalid(
            "native prompt requires a running command at a recognized input-ready prompt",
        ));
    }
    let (baseline_activity_seq, activity) = shell.arm_prompt_activity(hint, text)?;
    let receipt = PromptReceipt {
        prompt_id: prompt_id.to_string(),
        delivery: PromptDelivery::Uncertain,
        activity_observed: false,
        outcome: PromptOutcome::Waiting,
        generation: shell.command_generation(),
        baseline_activity_seq,
    };
    shell.prompt_receipts.insert(
        prompt_id.to_string(),
        PromptRecord {
            digest: Sha256::digest(text.as_bytes()).into(),
            recognizer: hint.to_string(),
            receipt,
            activity,
        },
    );
    // The caller retains the PTY lock across text, the Enter gap, and Enter.
    // Cancellation leaves an uncertain receipt, preventing automatic replay.
    if send_utf8_in_byte_chunks(shell, text, 128).is_ok() {
        tokio::time::sleep(Duration::from_millis(40)).await;
        shell.poll_output_nonblocking();
        if shell.command_running && shell.submit_prompt_enter().is_ok() {
            if let Some(record) = shell.prompt_receipts.get_mut(prompt_id) {
                record.receipt.delivery = PromptDelivery::Delivered;
            }
        }
    }
    Ok(hint.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn wait_prompt(
    state: &BashState,
    shared: &SharedPtyShell,
    background_id: Option<&str>,
    prompt_id: &str,
    hint: &str,
    identity: (u64, u64),
    timeout: Duration,
    compact: bool,
) -> Result<BashCommandRuntimeResult> {
    let start = Instant::now();
    let mut stable_since = start;
    let mut last_hash = None;
    loop {
        let mut guard = shared.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        if (shell.incarnation(), shell.command_generation()) != identity {
            return Err(invalid(
                "prompt execution generation changed; replacement output was not observed",
            ));
        }
        let mut observation = observe(shell, hint, true);
        let hash = PtyShell::fingerprint(&observation.screen.join("\n"));
        if last_hash != Some(hash) {
            last_hash = Some(hash);
            stable_since = Instant::now();
        }
        let running = shell.command_running;
        let record = shell
            .prompt_receipts
            .get_mut(prompt_id)
            .ok_or_else(|| invalid("prompt receipt is unavailable for this generation"))?;
        let receipt = &mut record.receipt;
        receipt.activity_observed |= record.activity.load(std::sync::atomic::Ordering::Acquire);
        let outcome = if matches!(
            receipt.outcome,
            PromptOutcome::AwaitingInput | PromptOutcome::AwaitingApproval | PromptOutcome::Exited
        ) {
            Some(receipt.outcome)
        } else if !running {
            Some(PromptOutcome::Exited)
        } else if receipt.delivery == PromptDelivery::Uncertain {
            Some(PromptOutcome::Waiting)
        } else if receipt.activity_observed
            && observation.explanation.state == TurnState::AwaitingApproval
        {
            Some(PromptOutcome::AwaitingApproval)
        } else if receipt.activity_observed
            && observation.explanation.state == TurnState::AwaitingInput
            && stable_since.elapsed() >= QUIET
        {
            Some(PromptOutcome::AwaitingInput)
        } else if start.elapsed() >= timeout {
            Some(PromptOutcome::TimedOut)
        } else {
            None
        };
        if let Some(outcome) = outcome {
            receipt.outcome = outcome;
            observation.prompt_receipt = Some(receipt.clone());
            return Ok(result(state, shell, background_id, observation, compact));
        }
        drop(guard);
        tokio::time::sleep(POLL.min(timeout.saturating_sub(start.elapsed()))).await;
    }
}

pub(super) fn inspect_tombstone(
    state: &BashState,
    id: &str,
    tombstone: &crate::tools::background_shell::ExitedShellInfo,
    action: &crate::types::BashCommandAction,
    compact: bool,
) -> Result<BashCommandRuntimeResult> {
    use crate::types::BashCommandAction;
    let retained = tombstone.workflow.as_ref().ok_or_else(|| {
        invalid("retained terminal detection evidence is unavailable for this background execution")
    })?;
    let (hint, include_output, receipt) = match action {
        BashCommandAction::Inspect { recognizer, include_output, .. } => {
            (recognizer.as_deref().unwrap_or("auto"), include_output.unwrap_or(true), None)
        }
        BashCommandAction::Prompt { prompt_id, text, recognizer, timeout_seconds, .. } => {
            validate_prompt(prompt_id, text.as_deref(), *timeout_seconds)?;
            let record = retained.receipts.get(prompt_id).ok_or_else(|| invalid(
                "background execution exited and this prompt_id has no retained receipt; no input was sent",
            ))?;
            if text.as_ref().is_some_and(|text| {
                <[u8; 32]>::from(Sha256::digest(text.as_bytes())) != record.digest
            }) {
                return Err(invalid(
                    "prompt_id was already used with different text; it will not be resent",
                ));
            }
            if recognizer.as_deref().is_some_and(|hint| hint != record.recognizer) {
                return Err(invalid("an existing prompt receipt must use its original recognizer"));
            }
            let mut receipt = record.receipt.clone();
            receipt.activity_observed |= record.activity.load(std::sync::atomic::Ordering::Acquire);
            receipt.outcome = PromptOutcome::Exited;
            (record.recognizer.as_str(), true, Some(receipt))
        }
        _ => unreachable!("only native observations use retained workflow evidence"),
    };
    let explanation = recognizer_for(hint).explain_input(&TurnInput {
        screen: &retained.screen,
        osc_title: &retained.osc_title,
        osc_progress: &retained.osc_progress,
    });
    let observation = WorkflowObservation {
        output: if include_output { tombstone.final_output.to_string() } else { String::new() },
        output_included: include_output,
        screen: retained.screen.clone(),
        activity_seq: retained.activity_seq,
        explanation,
        prompt_receipt: receipt,
    };
    let status = status_state(
        Some(id),
        false,
        None,
        tombstone.exit_code,
        &tombstone.cwd,
        Some(observation.explanation.state),
    );
    let mut result = runtime_rendered(
        String::new(),
        &render_status(state, &status),
        status,
        compact,
        Some(retained.generation),
        tombstone.output_truncated,
    );
    result.workflow = Some(observation);
    result.dropped_output_file.clone_from(&tombstone.scratch_path);
    Ok(result)
}

pub(super) fn validate_tombstone(
    tombstone: &crate::tools::background_shell::ExitedShellInfo,
    options: &ShellActionOptions,
) -> Result<()> {
    let expected = options
        .expected_execution
        .as_ref()
        .map(|token| token.generation)
        .or(options.expected_generation);
    if let Some(expected) = expected {
        if tombstone.workflow.as_ref().map(|retained| retained.generation) != Some(expected) {
            return Err(invalid(
                "retained background execution belongs to a different or unavailable generation",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
