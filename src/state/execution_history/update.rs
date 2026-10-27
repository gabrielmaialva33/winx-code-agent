use super::storage::{retain_tail, truncate, HistoryTransaction, StoredPayload};
use super::{
    CapturedOutput, ExecutionCapture, ExecutionProcessStatus, ExecutionRecord, HistoryError,
    HistoryResult,
};
use crate::tools::bash_command::BashProcessStatus;

pub(super) fn record(
    transaction: &mut HistoryTransaction,
    capture: &ExecutionCapture,
) -> HistoryResult<ExecutionRecord> {
    super::validate_id(&capture.execution_id)?;
    let previous = transaction.find(&capture.execution_id).cloned();
    if let Some(previous) = previous.as_ref() {
        validate_identity(previous, capture)?;
        if stale_observation(previous, capture) {
            return Ok(previous.clone());
        }
    } else {
        transaction.make_room()?;
    }
    let old_payload = previous
        .as_ref()
        .map_or_else(|| Ok(StoredPayload::default()), |record| transaction.payload(record))?;
    let mut payload = old_payload.clone();
    let mut record = previous.clone().unwrap_or_else(|| new_record(capture));
    let older_output = previous.as_ref().is_some_and(|previous| {
        previous.activity_seq.zip(capture.activity_seq).is_some_and(|(old, new)| new < old)
    });
    apply_state(&mut record, capture, transaction.limits.retention_ms);
    if older_output {
        // The first exit is authoritative even if its output observation is older.
        // Keep the newer bytes and retry a complete capture against the same token.
        record.output_pending = true;
    } else {
        apply_output(&mut record, &mut payload, capture, transaction.limits.max_output_bytes);
    }
    let state_changed = previous.as_ref().is_none_or(|old| {
        old.process_status != record.process_status
            || old.exit_code != record.exit_code
            || old.turn_state != record.turn_state
            || old.explanation != record.explanation
            || old.prompt_receipt != record.prompt_receipt
            || old.execution_token != record.execution_token
            || old.cwd != record.cwd
            || old.unknown_reason != record.unknown_reason
    });
    let output_changed = old_payload != payload
        || previous.as_ref().is_none_or(|old| {
            old.activity_seq != record.activity_seq
                || old.output_activity_seq != record.output_activity_seq
                || old.output_pending != record.output_pending
                || old.runtime_output_truncated != record.runtime_output_truncated
                || old.dropped_output_file != record.dropped_output_file
        });
    if !state_changed && !output_changed {
        if let Some(previous) = previous {
            return Ok(previous);
        }
    }
    if state_changed || output_changed {
        record.sequence = transaction.next_sequence();
        if state_changed {
            record.state_sequence = record.sequence;
        }
        if output_changed {
            record.output_sequence = record.sequence;
        }
    }
    if old_payload != payload || record.output_revision == 0 {
        transaction.store_payload(&mut record, &payload)?;
    }
    let id = record.execution_id.clone();
    transaction.put(record);
    transaction.enforce_output_budget()?;
    transaction.find(&id).cloned().ok_or(HistoryError::NotFound)
}

fn validate_identity(previous: &ExecutionRecord, capture: &ExecutionCapture) -> HistoryResult<()> {
    let token_changed = previous
        .execution_token
        .as_ref()
        .zip(capture.execution_token.as_ref())
        .is_some_and(|(left, right)| left != right);
    let generation_changed = previous
        .command_generation
        .zip(capture.command_generation)
        .is_some_and(|(left, right)| left != right);
    let exit_changed = previous.process_status == ExecutionProcessStatus::Exited
        && capture.state.process_status == BashProcessStatus::Exited
        && previous
            .exit_code
            .zip(capture.state.exit_code)
            .is_some_and(|(left, right)| left != right);
    if token_changed
        || generation_changed
        || exit_changed
        || previous.background_id != capture.state.background_id
    {
        return Err(HistoryError::IdentityConflict);
    }
    Ok(())
}

fn stale_observation(previous: &ExecutionRecord, capture: &ExecutionCapture) -> bool {
    if previous.process_status == ExecutionProcessStatus::Exited
        && capture.state.process_status == BashProcessStatus::Running
    {
        return true;
    }
    (previous.process_status == ExecutionProcessStatus::Exited
        || capture.state.process_status != BashProcessStatus::Exited)
        && previous.activity_seq.zip(capture.activity_seq).is_some_and(|(old, new)| new < old)
}

fn new_record(capture: &ExecutionCapture) -> ExecutionRecord {
    ExecutionRecord {
        execution_id: capture.execution_id.clone(),
        execution_token: capture.execution_token.clone(),
        command_generation: capture.command_generation,
        background_id: capture.state.background_id.clone(),
        cwd: capture.state.cwd.clone(),
        label: capture.label.as_ref().map(|label| {
            let mut label = crate::utils::redact::redact(label).into_owned();
            truncate(&mut label, 256);
            label
        }),
        process_status: ExecutionProcessStatus::Running,
        exit_code: None,
        turn_state: None,
        explanation: None,
        prompt_receipt: None,
        first_observed_at_unix_ms: capture.observed_at_unix_ms,
        observed_at_unix_ms: capture.observed_at_unix_ms,
        finished_at_unix_ms: None,
        expires_at_unix_ms: None,
        activity_seq: None,
        output_activity_seq: None,
        output_pending: true,
        sequence: 0,
        state_sequence: 0,
        output_sequence: 0,
        output_revision: 0,
        retained_start: 0,
        retained_end: 0,
        retained_bytes: 0,
        output_truncated: false,
        runtime_output_truncated: false,
        screen_truncated: false,
        screen_retained_bytes: 0,
        output_coverage: "unavailable".into(),
        dropped_output_file: None,
        unknown_reason: None,
    }
}

fn apply_state(record: &mut ExecutionRecord, capture: &ExecutionCapture, retention_ms: u64) {
    record.observed_at_unix_ms = record.observed_at_unix_ms.max(capture.observed_at_unix_ms);
    record.execution_token =
        capture.execution_token.clone().or_else(|| record.execution_token.clone());
    record.command_generation = capture.command_generation.or(record.command_generation);
    record.cwd.clone_from(&capture.state.cwd);
    record.activity_seq = match (record.activity_seq, capture.activity_seq) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (old, new) => new.or(old),
    };
    record.unknown_reason = None;
    record.process_status = match capture.state.process_status {
        BashProcessStatus::Running => ExecutionProcessStatus::Running,
        BashProcessStatus::Exited => ExecutionProcessStatus::Exited,
    };
    if record.process_status == ExecutionProcessStatus::Exited {
        record.exit_code = capture.state.exit_code.or(record.exit_code);
        let finished = *record.finished_at_unix_ms.get_or_insert(capture.observed_at_unix_ms);
        record.expires_at_unix_ms = Some(finished.saturating_add(retention_ms));
        record.turn_state = None;
    } else {
        record.exit_code = None;
        record.finished_at_unix_ms = None;
        record.expires_at_unix_ms = None;
        record.turn_state = capture
            .state
            .turn_state
            .or_else(|| capture.explanation.as_ref().map(|explanation| explanation.state));
    }
    record.explanation.clone_from(&capture.explanation);
    if let Some(explanation) = record.explanation.as_mut() {
        truncate(&mut explanation.recognizer, 128);
        truncate(&mut explanation.source, 64);
        if let Some(rule) = explanation.matched_rule.as_mut() {
            *rule = crate::utils::redact::redact(rule).into_owned();
            truncate(rule, 256);
        }
        if let Some(reason) = explanation.reason.as_mut() {
            *reason = crate::utils::redact::redact(reason).into_owned();
            truncate(reason, 512);
        }
    }
    if capture.prompt_receipt.is_some() {
        record.prompt_receipt.clone_from(&capture.prompt_receipt);
    }
    if capture.dropped_output_file.is_some() {
        record.dropped_output_file.clone_from(&capture.dropped_output_file);
    }
}

fn apply_output(
    record: &mut ExecutionRecord,
    payload: &mut StoredPayload,
    capture: &ExecutionCapture,
    limit: usize,
) {
    match &capture.output {
        CapturedOutput::Snapshot(output) => {
            let output = crate::utils::redact::redact(output).into_owned();
            let unchanged_rolling = capture.output_truncated
                && payload.runtime_snapshot
                && output == payload.output
                && capture.activity_seq.is_some()
                && capture.activity_seq == payload.output_activity_seq;
            let continued = payload.runtime_snapshot
                && !capture.output_truncated
                && !record.runtime_output_truncated
                && usize::try_from(payload.output_start)
                    .ok()
                    .and_then(|start| output.get(start..))
                    .is_some_and(|tail| tail.starts_with(&payload.output));
            if !continued && !unchanged_rolling {
                payload.output_epoch = payload.output_epoch.saturating_add(1);
            }
            if !unchanged_rolling {
                payload.output = output;
                payload.output_start = 0;
            }
            payload.runtime_snapshot = true;
            payload.output_activity_seq = capture.activity_seq;
            record.runtime_output_truncated = capture.output_truncated;
            record.output_coverage = "retained_runtime_output".into();
            record.output_activity_seq = capture.activity_seq;
            record.output_pending = false;
        }
        CapturedOutput::Fragment(output) if !payload.runtime_snapshot => {
            payload.output.push_str(&crate::utils::redact::redact(output));
            payload.output_activity_seq = capture.activity_seq;
            record.runtime_output_truncated |= capture.output_truncated;
            record.output_coverage = "delivered_fragments".into();
            record.output_pending = true;
        }
        CapturedOutput::Fragment(_) | CapturedOutput::None => {
            record.output_pending |= record.output_activity_seq != capture.activity_seq
                || capture.state.process_status == BashProcessStatus::Exited;
        }
    }
    let removed = retain_tail(&mut payload.output, limit);
    payload.output_start = payload.output_start.saturating_add(removed as u64);
    record.output_truncated = record.runtime_output_truncated || payload.output_start > 0;
    if let Some(screen) = capture.screen.as_ref() {
        payload.screen = crate::utils::redact::redact(screen).into_owned();
        record.screen_truncated = payload.screen.len() > 64 * 1024;
        retain_tail(&mut payload.screen, 64 * 1024);
    }
}

pub(super) fn mark_unknown(
    transaction: &mut HistoryTransaction,
    id: &str,
    reason: &str,
) -> Option<ExecutionRecord> {
    let mut record = transaction.find(id).cloned()?;
    if record.process_status == ExecutionProcessStatus::Exited {
        return Some(record);
    }
    let mut reason = crate::utils::redact::redact(reason).into_owned();
    truncate(&mut reason, 256);
    if record.process_status == ExecutionProcessStatus::Unknown
        && record.unknown_reason.as_deref() == Some(reason.as_str())
    {
        return Some(record);
    }
    record.process_status = ExecutionProcessStatus::Unknown;
    record.exit_code = None;
    record.turn_state = None;
    record.explanation = None;
    record.unknown_reason = Some(reason);
    record.observed_at_unix_ms = transaction.now;
    let finished = *record.finished_at_unix_ms.get_or_insert(transaction.now);
    record.expires_at_unix_ms = Some(finished.saturating_add(transaction.limits.retention_ms));
    record.sequence = transaction.next_sequence();
    record.state_sequence = record.sequence;
    transaction.put(record.clone());
    Some(record)
}
