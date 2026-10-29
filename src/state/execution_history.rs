//! Bounded, session-scoped execution history independent of MCP Task retention.
//!
//! Runtime identities and process states are authoritative. Stored output is a
//! projection for reading; it never determines completion, ownership or routing.

mod observation;
mod paging;
mod storage;
mod update;

pub use observation::{
    validate_observation_requests, CatalogObservation, ObservationItem, ObservationRequest,
    DEFAULT_OBSERVATION_PAGE_BYTES, MAX_OBSERVATION_ITEMS, MAX_OBSERVATION_OUTPUT_BYTES,
};

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::runtime::workflow::PromptReceipt;
use crate::runtime::ShellExecutionToken;
use crate::state::turn::{TurnExplanation, TurnState};
use crate::tools::bash_command::BashCommandState;

pub const DEFAULT_PAGE_BYTES: usize = 16 * 1024;
pub const MAX_PAGE_BYTES: usize = 64 * 1024;
pub const MAX_WATCHED_EXECUTIONS: usize = 32;

pub type HistoryResult<T> = Result<T, HistoryError>;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("execution history I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("execution history data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid execution history request: {0}")]
    InvalidInput(String),
    #[error("invalid or foreign execution cursor")]
    InvalidCursor,
    #[error("execution was not found in this session or its retention expired")]
    NotFound,
    #[error("execution history capacity is occupied by active executions")]
    Capacity,
    #[error("execution identity conflicts with its retained record")]
    IdentityConflict,
}

#[derive(Clone, Debug)]
pub struct HistoryLimits {
    pub max_records: usize,
    pub retention_ms: u64,
    pub max_output_bytes: usize,
    pub max_session_output_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_records: 128,
            retention_ms: 24 * 60 * 60 * 1000,
            max_output_bytes: 1024 * 1024,
            max_session_output_bytes: 16 * 1024 * 1024,
        }
    }
}

/// The thread ID is already authenticated and internally scoped by the server.
#[derive(Clone, Debug)]
pub struct ExecutionScope {
    pub thread_id: String,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSource {
    #[default]
    Output,
    Screen,
    Detection,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProcessStatus {
    Running,
    Exited,
    Unknown,
}

impl ExecutionProcessStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }
}

/// Output observed without changing the legacy delivery cursor.
#[derive(Clone, Debug)]
pub enum CapturedOutput {
    /// A retained runtime snapshot; truncation is supplied separately.
    Snapshot(String),
    /// A delivered fragment from a runtime without workflow observation.
    Fragment(String),
    /// Metadata-only inspection must never replace stored output with empty text.
    None,
}

#[derive(Clone, Debug)]
pub struct ExecutionCapture {
    pub execution_id: String,
    pub execution_token: Option<ShellExecutionToken>,
    pub command_generation: Option<u64>,
    pub state: BashCommandState,
    pub output: CapturedOutput,
    pub screen: Option<String>,
    pub explanation: Option<TurnExplanation>,
    pub prompt_receipt: Option<PromptReceipt>,
    pub activity_seq: Option<u64>,
    pub observed_at_unix_ms: u64,
    pub label: Option<String>,
    pub output_truncated: bool,
    pub dropped_output_file: Option<PathBuf>,
}

// These independent flags are part of the persisted and public metadata contract.
#[expect(clippy::struct_excessive_bools, reason = "independent wire metadata flags")]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecutionRecord {
    pub execution_id: String,
    pub execution_token: Option<ShellExecutionToken>,
    pub command_generation: Option<u64>,
    pub background_id: Option<String>,
    pub cwd: PathBuf,
    pub label: Option<String>,
    pub process_status: ExecutionProcessStatus,
    pub exit_code: Option<i32>,
    pub turn_state: Option<TurnState>,
    pub explanation: Option<TurnExplanation>,
    pub prompt_receipt: Option<PromptReceipt>,
    pub first_observed_at_unix_ms: u64,
    pub observed_at_unix_ms: u64,
    pub finished_at_unix_ms: Option<u64>,
    pub expires_at_unix_ms: Option<u64>,
    pub activity_seq: Option<u64>,
    /// Activity covered by the last full retained-output snapshot.
    #[serde(default)]
    pub output_activity_seq: Option<u64>,
    /// Metadata advanced without a complete output capture; a bound retry is needed.
    #[serde(default = "default_output_pending")]
    pub output_pending: bool,
    pub sequence: u64,
    pub state_sequence: u64,
    pub output_sequence: u64,
    pub output_revision: u64,
    pub retained_start: u64,
    pub retained_end: u64,
    pub retained_bytes: usize,
    pub output_truncated: bool,
    pub runtime_output_truncated: bool,
    pub screen_truncated: bool,
    pub screen_retained_bytes: usize,
    pub output_coverage: String,
    pub dropped_output_file: Option<PathBuf>,
    pub unknown_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogSnapshot {
    pub sequence: u64,
    pub cursor: String,
    pub executions: Vec<ExecutionRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryChanges {
    pub sequence: u64,
    pub after_sequence: u64,
    pub cursor: String,
    pub executions: Vec<ExecutionRecord>,
    pub missing_ids: Vec<String>,
    pub resync_required: bool,
}

// Pagination continuity and retention are independent properties of this wire response.
#[expect(clippy::struct_excessive_bools, reason = "independent pagination metadata flags")]
#[derive(Clone, Debug, Serialize)]
pub struct OutputPage {
    pub execution_id: String,
    pub source: OutputSource,
    pub text: String,
    pub next_cursor: String,
    pub has_more: bool,
    pub gap: bool,
    pub resync_required: bool,
    pub retained_start: u64,
    pub retained_end: u64,
    pub output_truncated: bool,
    pub output_coverage: String,
}

/// File locks serialize adapters; immutable output revisions make index updates atomic.
#[derive(Clone, Debug)]
pub struct ExecutionCatalog {
    root: PathBuf,
    limits: HistoryLimits,
}

impl ExecutionCatalog {
    pub const fn new(root: PathBuf, limits: HistoryLimits) -> Self {
        Self { root, limits }
    }

    pub fn record(
        &self,
        scope: &ExecutionScope,
        capture: ExecutionCapture,
    ) -> HistoryResult<ExecutionRecord> {
        let now = capture.observed_at_unix_ms;
        storage::transaction(self, scope, now, move |transaction| {
            update::record(transaction, &capture)
        })
    }

    /// Update a known execution without recreating an expired or foreign record.
    pub fn update_existing(
        &self,
        scope: &ExecutionScope,
        capture: ExecutionCapture,
    ) -> HistoryResult<Option<ExecutionRecord>> {
        validate_id(&capture.execution_id)?;
        let now = capture.observed_at_unix_ms;
        storage::transaction(self, scope, now, move |transaction| {
            let Some(previous) = transaction.find(&capture.execution_id) else {
                return Ok(None);
            };
            if previous
                .execution_token
                .as_ref()
                .zip(capture.execution_token.as_ref())
                .is_none_or(|(previous, current)| previous != current)
            {
                return Err(HistoryError::IdentityConflict);
            }
            update::record(transaction, &capture).map(Some)
        })
    }

    pub fn get(
        &self,
        scope: &ExecutionScope,
        execution_id: &str,
        now: u64,
    ) -> HistoryResult<Option<ExecutionRecord>> {
        validate_id(execution_id)?;
        storage::transaction(self, scope, now, |transaction| {
            Ok(transaction.find(execution_id).cloned())
        })
    }

    pub fn list(&self, scope: &ExecutionScope, now: u64) -> HistoryResult<CatalogSnapshot> {
        storage::transaction(self, scope, now, |transaction| {
            let mut executions = transaction.index.executions.clone();
            executions.sort_by_key(|record| std::cmp::Reverse(record.sequence));
            Ok(CatalogSnapshot {
                sequence: transaction.index.sequence,
                cursor: paging::event_cursor(transaction)?,
                executions,
            })
        })
    }

    pub fn read(
        &self,
        scope: &ExecutionScope,
        execution_id: &str,
        source: OutputSource,
        cursor: Option<&str>,
        limit: usize,
        now: u64,
    ) -> HistoryResult<OutputPage> {
        validate_id(execution_id)?;
        if !(4..=MAX_PAGE_BYTES).contains(&limit) {
            return Err(HistoryError::InvalidInput(format!(
                "page limit must be between 4 and {MAX_PAGE_BYTES} bytes"
            )));
        }
        storage::transaction(self, scope, now, |transaction| {
            paging::read(transaction, execution_id, source, cursor, limit)
        })
    }

    pub fn changes(
        &self,
        scope: &ExecutionScope,
        cursor: &str,
        execution_ids: &[String],
        now: u64,
    ) -> HistoryResult<HistoryChanges> {
        if execution_ids.len() > MAX_WATCHED_EXECUTIONS {
            return Err(HistoryError::InvalidInput(
                "at most 32 execution IDs may be watched".into(),
            ));
        }
        for id in execution_ids {
            validate_id(id)?;
        }
        storage::transaction(self, scope, now, |transaction| {
            paging::changes(transaction, cursor, execution_ids)
        })
    }

    pub fn mark_unknown(
        &self,
        scope: &ExecutionScope,
        execution_id: &str,
        reason: &str,
        now: u64,
    ) -> HistoryResult<Option<ExecutionRecord>> {
        validate_id(execution_id)?;
        storage::transaction(self, scope, now, |transaction| {
            Ok(update::mark_unknown(transaction, execution_id, reason))
        })
    }
}

fn default_output_pending() -> bool {
    true
}

fn validate_id(id: &str) -> HistoryResult<()> {
    if id.is_empty() || id.len() > 512 || id.contains('\0') {
        return Err(HistoryError::InvalidInput("execution ID is empty or too long".into()));
    }
    Ok(())
}
