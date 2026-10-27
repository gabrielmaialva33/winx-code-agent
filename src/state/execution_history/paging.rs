use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::storage::{digest, HistoryTransaction};
use super::{HistoryChanges, HistoryError, HistoryResult, OutputPage, OutputSource};

#[derive(Serialize, Deserialize)]
struct PageCursor {
    version: u8,
    kind: String,
    scope: String,
    epoch: String,
    execution: String,
    source: OutputSource,
    output_epoch: u64,
    snapshot: Option<String>,
    offset: u64,
}

#[derive(Serialize, Deserialize)]
struct EventCursor {
    version: u8,
    kind: String,
    scope: String,
    epoch: String,
    sequence: u64,
}

fn encode(value: &impl Serialize) -> HistoryResult<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?))
}

fn decode<T: serde::de::DeserializeOwned>(cursor: &str) -> HistoryResult<T> {
    if cursor.is_empty() || cursor.len() > 4096 {
        return Err(HistoryError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| HistoryError::InvalidCursor)?;
    serde_json::from_slice(&bytes).map_err(|_| HistoryError::InvalidCursor)
}

pub(super) fn event_cursor(transaction: &HistoryTransaction) -> HistoryResult<String> {
    encode(&EventCursor {
        version: 1,
        kind: "changes".into(),
        scope: transaction.index.scope_key.clone(),
        epoch: transaction.index.epoch.clone(),
        sequence: transaction.index.sequence,
    })
}

pub(super) fn changes(
    transaction: &HistoryTransaction,
    cursor: &str,
    ids: &[String],
) -> HistoryResult<HistoryChanges> {
    let cursor: EventCursor = decode(cursor)?;
    if cursor.version != 1
        || cursor.kind != "changes"
        || cursor.scope != transaction.index.scope_key
    {
        return Err(HistoryError::InvalidCursor);
    }
    let same_epoch = cursor.epoch == transaction.index.epoch;
    if same_epoch && cursor.sequence > transaction.index.sequence {
        return Err(HistoryError::InvalidCursor);
    }
    let missing_ids =
        ids.iter().filter(|id| transaction.find(id).is_none()).cloned().collect::<Vec<_>>();
    let resync_required = !same_epoch
        || cursor.sequence < transaction.index.floor_sequence
        || !missing_ids.is_empty();
    let executions = transaction
        .index
        .executions
        .iter()
        .filter(|record| ids.is_empty() || ids.contains(&record.execution_id))
        .filter(|record| resync_required || record.sequence > cursor.sequence)
        .cloned()
        .collect();
    Ok(HistoryChanges {
        sequence: transaction.index.sequence,
        after_sequence: if same_epoch { cursor.sequence } else { 0 },
        cursor: event_cursor(transaction)?,
        executions,
        missing_ids,
        resync_required,
    })
}

pub(super) fn read(
    transaction: &HistoryTransaction,
    id: &str,
    source: OutputSource,
    cursor: Option<&str>,
    limit: usize,
) -> HistoryResult<OutputPage> {
    let record = transaction.find(id).ok_or(HistoryError::NotFound)?;
    let payload = transaction.payload(record)?;
    let (text, base, output_epoch, snapshot, truncated, coverage) = match source {
        OutputSource::Output => (
            payload.output.as_str(),
            payload.output_start,
            payload.output_epoch,
            None,
            record.output_truncated,
            record.output_coverage.clone(),
        ),
        OutputSource::Screen | OutputSource::Detection => (
            payload.screen.as_str(),
            0,
            0,
            Some(digest(payload.screen.as_bytes())),
            record.screen_truncated,
            if source == OutputSource::Screen { "rendered_screen" } else { "detection_snapshot" }
                .into(),
        ),
    };
    let end = base.saturating_add(text.len() as u64);
    let mut next = PageCursor {
        version: 1,
        kind: "output".into(),
        scope: transaction.index.scope_key.clone(),
        epoch: transaction.index.epoch.clone(),
        execution: digest(id.as_bytes()),
        source,
        output_epoch,
        snapshot,
        offset: base,
    };
    let mut gap = false;
    let mut resync_required = false;
    if let Some(cursor) = cursor {
        let previous: PageCursor = decode(cursor)?;
        validate_page_binding(&previous, &next)?;
        gap = source == OutputSource::Output
            && (previous.offset < base
                || previous.output_epoch != output_epoch
                || previous.epoch != next.epoch);
        resync_required = gap || previous.epoch != next.epoch || previous.snapshot != next.snapshot;
        if !resync_required {
            if previous.offset > end {
                return Err(HistoryError::InvalidCursor);
            }
            next.offset = previous.offset;
        }
    }
    let start = usize::try_from(next.offset.saturating_sub(base))
        .map_err(|_| HistoryError::InvalidCursor)?;
    if !text.is_char_boundary(start) {
        return Err(HistoryError::InvalidCursor);
    }
    let delivered = if resync_required {
        String::new()
    } else {
        let mut finish = start.saturating_add(limit).min(text.len());
        while !text.is_char_boundary(finish) {
            finish -= 1;
        }
        next.offset = base.saturating_add(finish as u64);
        text[start..finish].to_string()
    };
    Ok(OutputPage {
        execution_id: id.to_string(),
        source,
        text: delivered,
        next_cursor: encode(&next)?,
        has_more: next.offset < end,
        gap,
        resync_required,
        retained_start: base,
        retained_end: end,
        output_truncated: truncated,
        output_coverage: coverage,
    })
}

fn validate_page_binding(previous: &PageCursor, current: &PageCursor) -> HistoryResult<()> {
    if previous.version != 1
        || previous.kind != "output"
        || previous.scope != current.scope
        || previous.execution != current.execution
        || previous.source != current.source
    {
        return Err(HistoryError::InvalidCursor);
    }
    Ok(())
}
