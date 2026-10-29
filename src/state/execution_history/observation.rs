//! Bounded, independently resumable observations from one catalog transaction.

use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    paging, storage, validate_id, ExecutionCatalog, ExecutionRecord, ExecutionScope, HistoryError,
    HistoryResult, OutputPage, OutputSource, MAX_PAGE_BYTES, MAX_WATCHED_EXECUTIONS,
};

pub const DEFAULT_OBSERVATION_PAGE_BYTES: usize = 2 * 1024;
pub const MAX_OBSERVATION_ITEMS: usize = MAX_WATCHED_EXECUTIONS;
pub const MAX_OBSERVATION_OUTPUT_BYTES: usize = MAX_PAGE_BYTES;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ObservationRequest {
    /// Execution retained for the initialized principal, thread and workspace.
    #[schemars(length(min = 1, max = 512))]
    pub execution_id: String,
    #[serde(default)]
    pub source: OutputSource,
    /// An opaque Read/Observe cursor for this same execution and source.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum UTF-8 text bytes for this page; all items together allow 64 KiB.
    #[serde(default = "default_page_bytes")]
    #[schemars(range(min = 4, max = 65536))]
    pub page_bytes: usize,
}

const fn default_page_bytes() -> usize {
    DEFAULT_OBSERVATION_PAGE_BYTES
}

/// Metadata remains available when its output page cannot be read.
#[derive(Debug)]
pub struct ObservationItem {
    pub execution_id: String,
    pub source: OutputSource,
    pub record: Option<ExecutionRecord>,
    pub page: HistoryResult<OutputPage>,
}

/// The cursor is a catalog change baseline, independent of every page cursor.
#[derive(Debug)]
pub struct CatalogObservation {
    pub sequence: u64,
    pub cursor: String,
    pub items: Vec<ObservationItem>,
}

/// Validate resource admission before session, catalog or runtime I/O.
///
/// A well-formed missing execution or invalid page cursor is an item error;
/// malformed identities, duplicate streams and invalid budgets reject the call.
pub fn validate_observation_requests(items: &[ObservationRequest]) -> HistoryResult<()> {
    if items.is_empty() || items.len() > MAX_OBSERVATION_ITEMS {
        return Err(HistoryError::InvalidInput(format!(
            "observe requires between 1 and {MAX_OBSERVATION_ITEMS} items"
        )));
    }
    let mut total_page_bytes = 0;
    for (index, item) in items.iter().enumerate() {
        validate_id(&item.execution_id)?;
        if !(4..=MAX_PAGE_BYTES).contains(&item.page_bytes) {
            return Err(HistoryError::InvalidInput(format!(
                "observe page_bytes must be between 4 and {MAX_PAGE_BYTES}"
            )));
        }
        // Both the item count and each summand are bounded before addition.
        total_page_bytes += item.page_bytes;
        if total_page_bytes > MAX_OBSERVATION_OUTPUT_BYTES {
            return Err(HistoryError::InvalidInput(format!(
                "observe total page_bytes must not exceed {MAX_OBSERVATION_OUTPUT_BYTES}"
            )));
        }
        if items[..index].iter().any(|previous| {
            previous.execution_id == item.execution_id && previous.source == item.source
        }) {
            return Err(HistoryError::InvalidInput(
                "observe requires unique execution_id and source pairs".into(),
            ));
        }
    }
    Ok(())
}

impl ExecutionCatalog {
    /// Read retained metadata and bounded pages under one scoped catalog lock.
    ///
    /// This performs no runtime refresh and consumes no delivery cursor. Item
    /// failures preserve their metadata and do not discard successful siblings.
    /// The outer error reports a shared catalog or request-admission failure.
    pub fn observe(
        &self,
        scope: &ExecutionScope,
        items: &[ObservationRequest],
        now: u64,
    ) -> HistoryResult<CatalogObservation> {
        validate_observation_requests(items)?;
        storage::transaction(self, scope, now, |transaction| {
            let items = items
                .iter()
                .map(|item| ObservationItem {
                    execution_id: item.execution_id.clone(),
                    source: item.source,
                    record: transaction.find(&item.execution_id).cloned(),
                    page: paging::read(
                        transaction,
                        &item.execution_id,
                        item.source,
                        item.cursor.as_deref(),
                        item.page_bytes,
                    ),
                })
                .collect();
            Ok(CatalogObservation {
                sequence: transaction.index.sequence,
                cursor: paging::event_cursor(transaction)?,
                items,
            })
        })
    }
}
