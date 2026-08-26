#![allow(clippy::unwrap_used)]
use anyhow::Result;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::state::persistence::{
    load_bash_state as load_state_file, save_bash_state as save_state_file, BashStateSnapshot,
};
use crate::state::pty::PtyShell;
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistData {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
}

/// Clamp ranges to `1..=total_lines`, drop inverted/empty ones, and sort. Shared
/// by the coverage queries so both tolerate overlapping/out-of-bounds ranges.
fn clamped_sorted(ranges: &[(usize, usize)], total_lines: usize) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = ranges
        .iter()
        .map(|&(s, e)| (s.max(1), e.min(total_lines)))
        .filter(|&(s, e)| s <= e)
        .collect();
    v.sort_unstable();
    v
}

/// Count distinct lines covered by (possibly overlapping) `ranges`, clamped to
/// `1..=total_lines`, via a single sort+sweep — O(k log k) in the range count.
fn covered_line_count(ranges: &[(usize, usize)], total_lines: usize) -> usize {
    let mut covered = 0usize;
    let mut last_end = 0usize; // highest line already counted (0 = none)
    for (s, e) in clamped_sorted(ranges, total_lines) {
        let s = s.max(last_end + 1);
        if s <= e {
            covered += e - s + 1;
            last_end = e;
        }
    }
    covered
}

impl FileWhitelistData {
    pub fn new(
        file_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
    ) -> Self {
        let mut data = Self { file_hash, line_ranges_read: Vec::new(), total_lines };
        data.merge_ranges(line_ranges_read);
        data
    }

    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= 99.0
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn get_percentage_read(&self) -> f64 {
        if self.total_lines == 0 {
            return 100.0;
        }
        // Sort+sweep over the ranges (O(k log k) in the range count) instead of
        // building an O(total_lines) HashSet on every call. Robust to overlapping
        // or out-of-range entries from older un-merged snapshots.
        let covered = covered_line_count(&self.line_ranges_read, self.total_lines);
        (covered as f64 / self.total_lines as f64) * 100.0
    }

    pub fn get_unread_ranges(&self) -> Vec<(usize, usize)> {
        if self.total_lines == 0 {
            return vec![];
        }
        let sorted = clamped_sorted(&self.line_ranges_read, self.total_lines);
        let mut unread = vec![];
        let mut next = 1usize; // next line not yet known-read
        for (s, e) in sorted {
            if s > next {
                unread.push((next, s - 1));
            }
            next = next.max(e.saturating_add(1));
        }
        if next <= self.total_lines {
            unread.push((next, self.total_lines));
        }
        unread
    }

    /// Record `[start, end]` as read, merging it into the existing intervals so
    /// `line_ranges_read` stays a bounded set of disjoint ranges. Without the
    /// merge, re-reading a file appended duplicate ranges forever (unbounded
    /// memory per session).
    pub fn add_range(&mut self, start: usize, end: usize) {
        self.merge_ranges(std::iter::once((start, end)));
    }

    /// Merge `new` ranges into `line_ranges_read`, keeping it sorted and disjoint
    /// (adjacent inclusive ranges like `(1,3)` and `(4,5)` collapse to `(1,5)`).
    pub fn merge_ranges(&mut self, new: impl IntoIterator<Item = (usize, usize)>) {
        self.line_ranges_read.extend(new);
        self.line_ranges_read.retain(|(s, e)| s <= e);
        self.line_ranges_read.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(self.line_ranges_read.len());
        for (s, e) in self.line_ranges_read.drain(..) {
            match merged.last_mut() {
                Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.line_ranges_read = merged;
    }

    pub fn needs_more_reading(&self) -> bool {
        !self.is_read_enough()
    }

    /// Whether every line in one inclusive range was visible to the model for
    /// this exact file hash.
    pub fn covers_range(&self, start: usize, end: usize) -> bool {
        start <= end
            && self
                .line_ranges_read
                .iter()
                .any(|(covered_start, covered_end)| *covered_start <= start && *covered_end >= end)
    }
}

/// How many edit checkpoints to keep per session for `UndoEdit`. In-memory only
/// (not persisted), oldest dropped past the cap, bounding memory on long sessions.
const EDIT_CHECKPOINT_CAP: usize = 10;
/// Aggregate source-content budget in addition to the historical count and
/// per-file limits.
const EDIT_CHECKPOINT_CONTENT_BYTES_CAP: usize = 4 * 1024 * 1024;
/// Aggregate metadata budget. In particular this prevents externally-created
/// `EditCheckpoint` values with enormous read-range vectors from bypassing the
/// source-content budget.
const EDIT_CHECKPOINT_METADATA_BYTES_CAP: usize = 256 * 1024;
const EDIT_CHECKPOINT_MAX_READ_RANGES: usize = 1_024;

/// Largest prior-content a checkpoint will hold. Files (up to the 50 MB edit
/// ceiling) above this aren't checkpointed, so a session editing huge assets
/// can't pile up to ~CAP * 50 MB of undo snapshots in memory; those edits just
/// aren't undoable.
const EDIT_CHECKPOINT_MAX_CONTENT_BYTES: usize = 1_000_000;

/// Maximum number of files whose read coverage is retained in one session.
/// Evicting an old entry is fail-closed: a later edit simply has to read that
/// file again, while long-lived HTTP/plugin sessions keep bounded memory and
/// persisted state.
const MAX_WHITELIST_FILES: usize = 1_024;

/// Recent image fingerprints remembered per live session. The cache contains
/// no image bytes and is intentionally not persisted; it only prevents an LLM
/// from resending the same unchanged image in one conversation.
const IMAGE_DELIVERY_CACHE_CAP: usize = 32;

/// Recent committed-edit receipts retained per logical session. The records
/// contain only request/file fingerprints and are persisted so an MCP adapter
/// restart cannot blindly repeat a mutation whose response was lost.
const EDIT_MUTATION_RECEIPT_CAP: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EditMutationPostcondition {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EditMutationReceipt {
    #[serde(default)]
    pub schema_version: u8,
    pub fingerprint: String,
    pub receipt_id: String,
    pub tool: String,
    pub status: String,
    pub committed_at_unix_ms: u64,
    pub postconditions: Vec<EditMutationPostcondition>,
    #[serde(default)]
    pub persisted: bool,
    #[serde(default)]
    pub verification_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_verification_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_state: Option<EditVerificationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_execution: Option<EditVerificationExecution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_undo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undo_ids: Vec<Option<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EditVerificationState {
    Pending,
    Reserved,
    Running,
    Interrupted,
    Skipped,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EditVerificationExecution {
    pub generation: u64,
    pub guardian_epoch: String,
    pub session_epoch: String,
}

/// A single file's pre-edit state, captured by `FileWriteOrEdit`/`MultiFileEdit`
/// after a successful write so `UndoEdit` can restore it. Only existing files get
/// one (a brand-new file's creation is not undoable - there is no prior content).
#[derive(Debug, Clone)]
pub struct EditCheckpoint {
    /// Resolved, workspace-confined path string (matches `whitelist_for_overwrite` keys).
    pub file_path_str: String,
    pub path: PathBuf,
    /// File content before the edit, to be written back on undo.
    pub prior_content: String,
    /// The whitelist entry before the edit, restored on undo so the hash gate of a
    /// later edit matches the reverted content. `None` if there was none.
    pub prior_whitelist: Option<FileWhitelistData>,
}

#[derive(Debug, Clone)]
struct EditCheckpointMetadata {
    checkpoint_fingerprint: String,
    undo_id: String,
    wrote_hash: String,
}

#[derive(Debug, Clone)]
pub struct BashState {
    pub cwd: PathBuf,
    pub workspace_root: PathBuf,
    pub current_thread_id: String,
    pub mode: Modes,
    pub bash_command_mode: BashCommandMode,
    pub file_edit_mode: FileEditMode,
    pub write_if_empty_mode: WriteIfEmptyMode,
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistData>,
    /// Least-recently-used order for `whitelist_for_overwrite`, oldest first.
    /// Snapshots intentionally persist only the guarded data; restored entries
    /// receive a deterministic order when loaded.
    whitelist_recency: VecDeque<String>,
    pub pty_shell: Arc<Mutex<Option<PtyShell>>>,
    /// Serializes foreground command startup across cloned request-local states.
    /// Status/input actions remain concurrent so callers can drive a running TUI.
    pub foreground_command_gate: Arc<Mutex<()>>,
    pub initialized: bool,
    /// In-memory ring of recent edit checkpoints for `UndoEdit` (newest at the
    /// back). Deliberately not part of `BashStateSnapshot`: undo is for immediate
    /// mid-session recovery, not across restarts.
    pub edit_checkpoints: VecDeque<EditCheckpoint>,
    edit_checkpoint_metadata: VecDeque<EditCheckpointMetadata>,
    /// In-memory aggregate guard for syntax maps over non-canonical helpers.
    /// Canonical `CodeMap` calls are intentionally not counted.
    pub derived_code_map_usage: crate::utils::agent_temp::DerivedCodeMapUsage,
    image_deliveries: VecDeque<String>,
    edit_mutation_receipts: VecDeque<EditMutationReceipt>,
}

impl Default for BashState {
    fn default() -> Self {
        Self::new()
    }
}

impl BashState {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        Self {
            cwd: cwd.clone(),
            workspace_root: cwd,
            current_thread_id: generate_thread_id(),
            mode: Modes::Wcgw,
            bash_command_mode: BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            file_edit_mode: FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
            write_if_empty_mode: WriteIfEmptyMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
            whitelist_for_overwrite: HashMap::new(),
            whitelist_recency: VecDeque::new(),
            pty_shell: Arc::new(Mutex::new(None)),
            foreground_command_gate: Arc::new(Mutex::new(())),
            initialized: false,
            edit_checkpoints: VecDeque::new(),
            edit_checkpoint_metadata: VecDeque::new(),
            derived_code_map_usage: crate::utils::agent_temp::DerivedCodeMapUsage::default(),
            image_deliveries: VecDeque::new(),
            edit_mutation_receipts: VecDeque::new(),
        }
    }

    pub(crate) fn edit_mutation_receipt(
        &mut self,
        fingerprint: &str,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Option<EditMutationReceipt> {
        self.prune_edit_mutation_receipts(now_unix_ms, ttl_ms);
        let index = self
            .edit_mutation_receipts
            .iter()
            .rposition(|receipt| receipt.fingerprint == fingerprint)?;
        let receipt = self.edit_mutation_receipts.remove(index)?;
        self.edit_mutation_receipts.push_back(receipt.clone());
        Some(receipt)
    }

    pub(crate) fn edit_mutation_receipt_variant(
        &mut self,
        fingerprint: &str,
        verification_id: Option<&str>,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Option<EditMutationReceipt> {
        self.prune_edit_mutation_receipts(now_unix_ms, ttl_ms);
        let index = self.edit_mutation_receipts.iter().rposition(|receipt| {
            receipt.fingerprint == fingerprint
                && receipt.verification_id.as_deref() == verification_id
        })?;
        let receipt = self.edit_mutation_receipts.remove(index)?;
        self.edit_mutation_receipts.push_back(receipt.clone());
        Some(receipt)
    }

    #[cfg(test)]
    pub(crate) fn record_edit_mutation_receipt(&mut self, receipt: EditMutationReceipt) {
        self.record_edit_mutation_receipts([receipt]);
    }

    pub(crate) fn record_edit_mutation_receipts(
        &mut self,
        receipts: impl IntoIterator<Item = EditMutationReceipt>,
    ) {
        let receipts = receipts.into_iter().collect::<Vec<_>>();
        for receipt in &receipts {
            self.edit_mutation_receipts.retain(|existing| {
                !(existing.fingerprint == receipt.fingerprint
                    && existing.verification_id == receipt.verification_id
                    && existing.legacy_verification_id == receipt.legacy_verification_id)
            });
        }
        self.edit_mutation_receipts.extend(receipts);
        while self.edit_mutation_receipts.len() > EDIT_MUTATION_RECEIPT_CAP {
            let Some(group) =
                self.edit_mutation_receipts.front().map(|receipt| receipt.receipt_id.clone())
            else {
                break;
            };
            self.edit_mutation_receipts.retain(|receipt| receipt.receipt_id != group);
        }
    }

    fn prune_edit_mutation_receipts(&mut self, now_unix_ms: u64, ttl_ms: u64) {
        let expired_groups = self
            .edit_mutation_receipts
            .iter()
            .filter(|receipt| now_unix_ms.saturating_sub(receipt.committed_at_unix_ms) > ttl_ms)
            .map(|receipt| receipt.receipt_id.clone())
            .collect::<std::collections::HashSet<_>>();
        self.edit_mutation_receipts.retain(|receipt| !expired_groups.contains(&receipt.receipt_id));
    }

    pub(crate) fn edit_mutation_receipt_by_verification_id(
        &mut self,
        verification_id: &str,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Option<EditMutationReceipt> {
        self.prune_edit_mutation_receipts(now_unix_ms, ttl_ms);
        let index = self.edit_mutation_receipts.iter().position(|receipt| {
            receipt.schema_version >= 1
                && receipt.tool == "EditFiles"
                && receipt.verification_id.as_deref() == Some(verification_id)
        })?;
        let receipt = self.edit_mutation_receipts.remove(index)?;
        self.edit_mutation_receipts.push_back(receipt.clone());
        Some(receipt)
    }

    pub(crate) fn edit_mutation_receipt_by_legacy_verification_id(
        &mut self,
        verification_id: &str,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Option<EditMutationReceipt> {
        self.prune_edit_mutation_receipts(now_unix_ms, ttl_ms);
        let index = self.edit_mutation_receipts.iter().position(|receipt| {
            receipt.tool != "EditFiles"
                && receipt.legacy_verification_id.as_deref() == Some(verification_id)
        })?;
        let receipt = self.edit_mutation_receipts.remove(index)?;
        self.edit_mutation_receipts.push_back(receipt.clone());
        Some(receipt)
    }

    pub(crate) fn update_edit_mutation_verification(
        &mut self,
        verification_id: &str,
        status: &str,
        verification_state: EditVerificationState,
        execution: Option<&EditVerificationExecution>,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> bool {
        self.prune_edit_mutation_receipts(now_unix_ms, ttl_ms);
        let mut updated = false;
        for receipt in &mut self.edit_mutation_receipts {
            if receipt.verification_id.as_deref() == Some(verification_id) {
                receipt.status = status.to_string();
                receipt.verification_pending = matches!(
                    verification_state,
                    EditVerificationState::Pending
                        | EditVerificationState::Reserved
                        | EditVerificationState::Running
                        | EditVerificationState::Interrupted
                );
                receipt.verification_state = Some(verification_state);
                receipt.verification_execution.clone_from(&execution.cloned());
                updated = true;
            }
        }
        updated
    }

    pub(crate) fn mark_edit_mutation_verification_volatile(&mut self, verification_id: &str) {
        for receipt in &mut self.edit_mutation_receipts {
            if receipt.verification_id.as_deref() == Some(verification_id) {
                receipt.persisted = false;
            }
        }
    }

    pub(crate) fn mark_edit_mutation_receipt_volatile(&mut self, fingerprint: &str) {
        if let Some(receipt) = self
            .edit_mutation_receipts
            .iter_mut()
            .find(|receipt| receipt.fingerprint == fingerprint)
        {
            receipt.persisted = false;
        }
    }

    /// Return whether this image was already delivered, refreshing its LRU
    /// position on a hit. Fingerprints are content-based, so aliases and copies
    /// cannot bypass deduplication accidentally.
    pub(crate) fn image_was_delivered(&mut self, fingerprint: &str) -> bool {
        let Some(index) = self.image_deliveries.iter().position(|item| item == fingerprint) else {
            return false;
        };
        if let Some(item) = self.image_deliveries.remove(index) {
            self.image_deliveries.push_back(item);
        }
        true
    }

    /// Record a successful image delivery without retaining its payload.
    pub(crate) fn record_image_delivery(&mut self, fingerprint: String) {
        if let Some(index) = self.image_deliveries.iter().position(|item| item == &fingerprint) {
            self.image_deliveries.remove(index);
        }
        self.image_deliveries.push_back(fingerprint);
        while self.image_deliveries.len() > IMAGE_DELIVERY_CACHE_CAP {
            self.image_deliveries.pop_front();
        }
    }

    /// Record a pre-edit checkpoint for `UndoEdit`, dropping the oldest past the
    /// cap. This public API intentionally retains its 0.2.x signature and data
    /// type; opaque receipt metadata lives in a separate private queue.
    pub fn push_edit_checkpoint(&mut self, checkpoint: EditCheckpoint) {
        let _ = self.push_edit_checkpoint_inner(checkpoint, None);
    }

    pub(crate) fn push_receipt_bound_edit_checkpoint(
        &mut self,
        checkpoint: EditCheckpoint,
        wrote_hash: String,
    ) -> Option<String> {
        let undo_id = format!("undo_{:032x}", rand::random::<u128>());
        self.push_edit_checkpoint_inner(
            checkpoint,
            Some(EditCheckpointMetadata {
                checkpoint_fingerprint: String::new(),
                undo_id: undo_id.clone(),
                wrote_hash,
            }),
        )
        .then_some(undo_id)
    }

    fn push_edit_checkpoint_inner(
        &mut self,
        mut checkpoint: EditCheckpoint,
        metadata: Option<EditCheckpointMetadata>,
    ) -> bool {
        if checkpoint.prior_content.len() > EDIT_CHECKPOINT_MAX_CONTENT_BYTES {
            info!(
                file = %checkpoint.file_path_str,
                "UndoEdit: not checkpointing a file over 1 MB (too large to hold in memory)"
            );
            return false;
        }
        sanitize_checkpoint_metadata(&mut checkpoint);
        let fingerprint = checkpoint_fingerprint(&checkpoint);
        self.edit_checkpoints.push_back(checkpoint);
        if let Some(mut metadata) = metadata {
            metadata.checkpoint_fingerprint.clone_from(&fingerprint);
            self.edit_checkpoint_metadata.push_back(metadata);
        }
        while self.edit_checkpoints.len() > EDIT_CHECKPOINT_CAP
            || self.undo_content_bytes() > EDIT_CHECKPOINT_CONTENT_BYTES_CAP
            || self.undo_metadata_bytes() > EDIT_CHECKPOINT_METADATA_BYTES_CAP
        {
            if let Some(removed) = self.edit_checkpoints.pop_front() {
                self.remove_checkpoint_metadata(&checkpoint_fingerprint(&removed));
            } else {
                break;
            }
        }
        self.reconcile_checkpoint_metadata();
        self.edit_checkpoints.iter().any(|item| checkpoint_fingerprint(item) == fingerprint)
    }

    /// Remove and return the most recent checkpoint for `file_path_str` (per-file
    /// LIFO, so repeated undos on one file walk its edits back while leaving other
    /// files' checkpoints in place). `None` if that file has no checkpoint.
    pub fn pop_edit_checkpoint_for(&mut self, file_path_str: &str) -> Option<EditCheckpoint> {
        let index =
            self.edit_checkpoints.iter().rposition(|cp| cp.file_path_str == file_path_str)?;
        let checkpoint = self.edit_checkpoints.remove(index)?;
        self.remove_checkpoint_metadata(&checkpoint_fingerprint(&checkpoint));
        Some(checkpoint)
    }

    pub(crate) fn pop_latest_edit_checkpoint_by_id(
        &mut self,
        file_path_str: &str,
        undo_id: &str,
    ) -> Option<EditCheckpoint> {
        let index = self
            .edit_checkpoints
            .iter()
            .rposition(|checkpoint| checkpoint.file_path_str == file_path_str)?;
        let fingerprint = checkpoint_fingerprint(self.edit_checkpoints.get(index)?);
        if self
            .edit_checkpoint_metadata
            .iter()
            .rev()
            .find(|metadata| metadata.checkpoint_fingerprint == fingerprint)?
            .undo_id
            != undo_id
        {
            return None;
        }
        let checkpoint = self.edit_checkpoints.remove(index)?;
        self.remove_checkpoint_metadata(&fingerprint);
        Some(checkpoint)
    }

    /// Clone the most recent per-file checkpoint without consuming it. Undo
    /// removes the checkpoint only after the replacement commits successfully.
    pub fn latest_edit_checkpoint_for(&self, file_path_str: &str) -> Option<EditCheckpoint> {
        self.edit_checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.file_path_str == file_path_str)
            .cloned()
    }

    pub(crate) fn undo_checkpoint_count_for(&self, file_path_str: &str) -> usize {
        self.edit_checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.file_path_str == file_path_str)
            .count()
    }

    pub(crate) fn next_undo_id_for(&self, file_path_str: &str) -> Option<String> {
        let checkpoint = self
            .edit_checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.file_path_str == file_path_str)?;
        let fingerprint = checkpoint_fingerprint(checkpoint);
        self.edit_checkpoint_metadata
            .iter()
            .rev()
            .find(|metadata| metadata.checkpoint_fingerprint == fingerprint)
            .map(|metadata| metadata.undo_id.clone())
    }

    pub(crate) fn latest_receipt_bound_checkpoint(
        &self,
        file_path_str: &str,
    ) -> Option<(EditCheckpoint, String, String)> {
        let checkpoint = self
            .edit_checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.file_path_str == file_path_str)?;
        let fingerprint = checkpoint_fingerprint(checkpoint);
        let metadata = self
            .edit_checkpoint_metadata
            .iter()
            .rev()
            .find(|metadata| metadata.checkpoint_fingerprint == fingerprint)?;
        Some((checkpoint.clone(), metadata.undo_id.clone(), metadata.wrote_hash.clone()))
    }

    /// Whether an opaque undo handle still has its private in-memory checkpoint.
    /// Mutation receipts outlive the bounded checkpoint queue, so replay must
    /// consult this before advertising a handle that may have been evicted.
    pub(crate) fn has_receipt_bound_checkpoint(&self, file_path_str: &str, undo_id: &str) -> bool {
        self.edit_checkpoints.iter().rev().any(|checkpoint| {
            if checkpoint.file_path_str != file_path_str {
                return false;
            }
            let fingerprint = checkpoint_fingerprint(checkpoint);
            self.edit_checkpoint_metadata.iter().rev().any(|metadata| {
                metadata.checkpoint_fingerprint == fingerprint && metadata.undo_id == undo_id
            })
        })
    }

    fn undo_content_bytes(&self) -> usize {
        self.edit_checkpoints.iter().map(|checkpoint| checkpoint.prior_content.len()).sum()
    }

    fn undo_metadata_bytes(&self) -> usize {
        self.edit_checkpoints.iter().map(checkpoint_metadata_bytes).sum()
    }

    fn remove_checkpoint_metadata(&mut self, fingerprint: &str) {
        if let Some(index) = self
            .edit_checkpoint_metadata
            .iter()
            .position(|metadata| metadata.checkpoint_fingerprint == fingerprint)
        {
            self.edit_checkpoint_metadata.remove(index);
        }
    }

    fn reconcile_checkpoint_metadata(&mut self) {
        let mut available = HashMap::<String, usize>::new();
        for checkpoint in &self.edit_checkpoints {
            *available.entry(checkpoint_fingerprint(checkpoint)).or_default() += 1;
        }
        self.edit_checkpoint_metadata.retain(|metadata| {
            let Some(count) = available.get_mut(&metadata.checkpoint_fingerprint) else {
                return false;
            };
            if *count == 0 {
                return false;
            }
            *count -= 1;
            true
        });
    }

    /// Merge visible read coverage for `path` and make it the newest guarded
    /// entry. Coverage from a different file version is discarded instead of
    /// being combined with the new hash, which could otherwise make disjoint
    /// reads across two versions look like one complete read.
    pub fn record_read_coverage(
        &mut self,
        path: &str,
        ranges: impl IntoIterator<Item = (usize, usize)>,
        file_hash: String,
        total_lines: usize,
    ) {
        let ranges = ranges.into_iter().collect::<Vec<_>>();
        match self.whitelist_for_overwrite.get_mut(path) {
            Some(existing)
                if existing.file_hash == file_hash && existing.total_lines == total_lines =>
            {
                existing.merge_ranges(ranges);
            }
            Some(existing) => {
                *existing = FileWhitelistData::new(file_hash, ranges, total_lines);
            }
            None => {
                self.whitelist_for_overwrite.insert(
                    path.to_string(),
                    FileWhitelistData::new(file_hash, ranges, total_lines),
                );
            }
        }
        self.touch_whitelist(path);
        self.enforce_whitelist_cap();
    }

    /// Replace a whitelist entry after a successful edit or undo.
    pub fn set_whitelist_entry(&mut self, path: &str, entry: FileWhitelistData) {
        self.whitelist_for_overwrite.insert(path.to_string(), entry);
        self.touch_whitelist(path);
        self.enforce_whitelist_cap();
    }

    /// Remove a whitelist entry and its LRU metadata.
    pub fn remove_whitelist_entry(&mut self, path: &str) -> Option<FileWhitelistData> {
        self.whitelist_recency.retain(|candidate| candidate != path);
        self.whitelist_for_overwrite.remove(path)
    }

    fn touch_whitelist(&mut self, path: &str) {
        self.whitelist_recency.retain(|candidate| candidate != path);
        self.whitelist_recency.push_back(path.to_string());
    }

    fn rebuild_whitelist_recency(&mut self) {
        let mut paths = self.whitelist_for_overwrite.keys().cloned().collect::<Vec<_>>();
        paths.sort_unstable();
        self.whitelist_recency = paths.into();
        self.enforce_whitelist_cap();
    }

    fn enforce_whitelist_cap(&mut self) {
        self.whitelist_recency.retain(|path| self.whitelist_for_overwrite.contains_key(path));
        while self.whitelist_for_overwrite.len() > MAX_WHITELIST_FILES {
            let victim = self.whitelist_recency.pop_front().or_else(|| {
                // The map remains public for compatibility, so tolerate callers
                // that inserted directly without updating the LRU metadata.
                self.whitelist_for_overwrite.keys().min().cloned()
            });
            let Some(victim) = victim else { break };
            self.whitelist_for_overwrite.remove(&victim);
        }
    }

    pub async fn init_pty_shell(&mut self) -> Result<()> {
        let cwd = self.cwd.clone();
        let workspace_root = self.workspace_root.clone();
        let temporary_artifact_dir =
            crate::utils::agent_temp::session_info(&workspace_root, &self.current_thread_id)
                .directory;
        let restricted = self.bash_command_mode.bash_mode == BashMode::RestrictedMode;
        // PtyShell::new forks+execs a shell and does a ~300ms blocking prompt init
        // (thread::sleep + drain_output busy-wait). Run it on the blocking pool so
        // it never pins a tokio worker thread.
        let shell = tokio::task::spawn_blocking(move || {
            PtyShell::new_with_agent_paths(
                &cwd,
                restricted,
                Some(&workspace_root),
                Some(&temporary_artifact_dir),
            )
        })
        .await
        .map_err(|e| {
            crate::errors::WinxError::ShellInitializationError(format!("PTY init task failed: {e}"))
        })??;
        *self.pty_shell.lock().await = Some(shell);
        Ok(())
    }

    pub fn update_cwd(&mut self, path: &Path) -> Result<()> {
        self.cwd = path.to_path_buf();
        Ok(())
    }

    pub fn update_workspace_root(&mut self, path: &Path) -> Result<()> {
        self.workspace_root = path.to_path_buf();
        Ok(())
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.mode == Modes::Architect {
            crate::utils::bash_parser::is_architect_command_allowed(command)
        } else {
            self.bash_command_mode.allowed_commands.is_allowed(command)
        }
    }

    pub fn is_file_edit_allowed(&self, path: &str) -> bool {
        self.file_edit_mode.allowed_globs.is_allowed(path)
    }

    pub fn is_file_write_allowed(&self, path: &str) -> bool {
        self.write_if_empty_mode.allowed_globs.is_allowed(path)
    }
    pub fn save_state_to_disk(&self) -> Result<()> {
        let snapshot = self.snapshot();
        save_state_file(&self.current_thread_id, &snapshot)?;
        Ok(())
    }

    pub fn snapshot(&self) -> BashStateSnapshot {
        BashStateSnapshot::from_state(
            &self.cwd.to_string_lossy(),
            &self.workspace_root.to_string_lossy(),
            self.mode,
            &self.bash_command_mode,
            &self.file_edit_mode,
            &self.write_if_empty_mode,
            &self.whitelist_for_overwrite,
            &self.current_thread_id,
            &self.edit_mutation_receipts,
        )
    }

    pub fn apply_snapshot(&mut self, snapshot: &BashStateSnapshot) {
        let (cwd, root, mode, bmode, emode, wmode, whitelist, tid) = snapshot.to_state_components();
        let root = PathBuf::from(root);
        let image_identity_changed = self.current_thread_id != tid || self.workspace_root != root;

        self.cwd = PathBuf::from(cwd);
        self.workspace_root = root;
        self.mode = mode;
        self.bash_command_mode = bmode;
        self.file_edit_mode = emode;
        self.write_if_empty_mode = wmode;
        self.whitelist_for_overwrite = whitelist;
        self.rebuild_whitelist_recency();
        self.current_thread_id = tid;
        self.edit_mutation_receipts = snapshot.edit_mutation_receipts.iter().cloned().collect();
        // Undo checkpoints are deliberately live-process only. Applying a
        // persisted snapshot is a restart/reattach boundary and must never make
        // a stale opaque undo receipt valid again.
        self.edit_checkpoints.clear();
        self.edit_checkpoint_metadata.clear();
        for receipt in &mut self.edit_mutation_receipts {
            // Opaque undo handles are meaningful only while their private
            // in-memory checkpoints exist. Never advertise one after a
            // restart/reattach boundary has deliberately dropped that history.
            receipt.undo_ids.clear();
            receipt.next_undo_id = None;
            if matches!(
                receipt.verification_state,
                Some(EditVerificationState::Reserved | EditVerificationState::Running)
            ) {
                receipt.verification_state = Some(EditVerificationState::Interrupted);
                receipt.verification_pending = true;
                receipt.verification_execution = None;
                receipt.status = "completed_with_issues".to_string();
            }
        }
        if image_identity_changed {
            self.image_deliveries.clear();
        }
        self.initialized = true;
    }

    pub fn load_state_from_disk(&mut self, thread_id: &str) -> Result<bool> {
        if let Some(snapshot) = load_state_file(thread_id)? {
            self.apply_snapshot(&snapshot);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn new_with_thread_id(thread_id: Option<&str>) -> Self {
        let mut state = Self::new();

        if let Some(tid) = thread_id {
            if !tid.is_empty() {
                if let Ok(true) = state.load_state_from_disk(tid) {
                    info!("Loaded state for thread_id '{}'", tid);
                } else {
                    state.current_thread_id = tid.to_string();
                }
            }
        }

        state
    }
}

fn sanitize_checkpoint_metadata(checkpoint: &mut EditCheckpoint) {
    let Some(whitelist) = checkpoint.prior_whitelist.as_mut() else { return };
    let mut ranges = std::mem::take(&mut whitelist.line_ranges_read);
    ranges.sort_unstable();
    let mut bounded: Vec<(usize, usize)> =
        Vec::with_capacity(ranges.len().min(EDIT_CHECKPOINT_MAX_READ_RANGES));
    for (start, end) in ranges {
        if start > end {
            continue;
        }
        if let Some((_, previous_end)) = bounded.last_mut() {
            if start <= previous_end.saturating_add(1) {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        if bounded.len() < EDIT_CHECKPOINT_MAX_READ_RANGES {
            bounded.push((start, end));
        } else {
            break;
        }
    }
    whitelist.line_ranges_read = bounded;
}

fn checkpoint_metadata_bytes(checkpoint: &EditCheckpoint) -> usize {
    let whitelist = checkpoint.prior_whitelist.as_ref().map_or(0, |whitelist| {
        whitelist
            .file_hash
            .len()
            .saturating_add(whitelist.line_ranges_read.len().saturating_mul(2 * size_of::<usize>()))
            .saturating_add(size_of::<usize>())
    });
    checkpoint
        .file_path_str
        .len()
        .saturating_add(checkpoint.path.as_os_str().as_encoded_bytes().len())
        .saturating_add(whitelist)
}

fn checkpoint_fingerprint(checkpoint: &EditCheckpoint) -> String {
    fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(bytes);
    }
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, checkpoint.file_path_str.as_bytes());
    hash_field(&mut hasher, checkpoint.path.as_os_str().as_encoded_bytes());
    hash_field(&mut hasher, checkpoint.prior_content.as_bytes());
    if let Some(whitelist) = checkpoint.prior_whitelist.as_ref() {
        hash_field(&mut hasher, whitelist.file_hash.as_bytes());
        hasher.update(u64::try_from(whitelist.total_lines).unwrap_or(u64::MAX).to_le_bytes());
        for (start, end) in &whitelist.line_ranges_read {
            hasher.update(u64::try_from(*start).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(u64::try_from(*end).unwrap_or(u64::MAX).to_le_bytes());
        }
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub fn generate_thread_id() -> String {
    let mut rng = rand::rng();
    format!("tid_{:x}", rng.random::<u64>())
}

#[cfg(test)]
mod whitelist_range_tests {
    #![allow(clippy::expect_used)]

    use std::collections::HashMap;

    use super::{
        BashState, EditCheckpoint, EditMutationPostcondition, EditMutationReceipt,
        EditVerificationState, FileWhitelistData, EDIT_CHECKPOINT_CAP,
        EDIT_CHECKPOINT_CONTENT_BYTES_CAP, EDIT_CHECKPOINT_MAX_READ_RANGES,
        EDIT_CHECKPOINT_METADATA_BYTES_CAP, EDIT_MUTATION_RECEIPT_CAP, MAX_WHITELIST_FILES,
    };

    fn wl(ranges: &[(usize, usize)], total: usize) -> FileWhitelistData {
        FileWhitelistData::new("h".to_string(), ranges.to_vec(), total)
    }

    #[test]
    fn merge_collapses_overlap_and_adjacency() {
        // (1,3)+(4,5) are adjacent (inclusive) -> (1,5); (7,9)+(8,12) overlap -> (7,12).
        let w = wl(&[(4, 5), (1, 3), (8, 12), (7, 9)], 20);
        assert_eq!(w.line_ranges_read, vec![(1, 5), (7, 12)]);
    }

    #[test]
    fn re_reading_does_not_grow_unbounded() {
        let mut w = wl(&[(1, 10)], 100);
        for _ in 0..1000 {
            w.merge_ranges(std::iter::once((1, 10)));
            w.merge_ranges(std::iter::once((5, 15)));
        }
        // 1000 re-reads collapse to a single interval, not 2000 entries.
        assert_eq!(w.line_ranges_read, vec![(1, 15)]);
    }

    #[test]
    fn percentage_counts_distinct_lines_with_overlap() {
        // lines 1..=5 and 3..=8 cover 1..=8 = 8 of 10 = 80%.
        let w = wl(&[(1, 5), (3, 8)], 10);
        assert!((w.get_percentage_read() - 80.0).abs() < 1e-9);
        assert!(wl(&[(1, 10)], 10).is_read_enough());
    }

    #[test]
    fn unread_ranges_are_the_gaps() {
        // read 2..=4 and 7..=8 of 10 -> unread 1, 5..=6, 9..=10.
        let w = wl(&[(2, 4), (7, 8)], 10);
        assert_eq!(w.get_unread_ranges(), vec![(1, 1), (5, 6), (9, 10)]);
    }

    #[test]
    fn out_of_range_entries_are_clamped() {
        // A (0, 999) range on a 10-line file counts as full coverage, not a panic.
        let w = wl(&[(0, 999)], 10);
        assert!((w.get_percentage_read() - 100.0).abs() < 1e-9);
        assert!(w.get_unread_ranges().is_empty());
    }

    #[test]
    fn coverage_from_different_file_versions_is_not_combined() {
        let mut state = BashState::new();
        state.record_read_coverage("/workspace/file.rs", [(1, 50)], "old-hash".to_string(), 100);
        state.record_read_coverage("/workspace/file.rs", [(51, 100)], "new-hash".to_string(), 100);

        let coverage = &state.whitelist_for_overwrite["/workspace/file.rs"];
        assert_eq!(coverage.file_hash, "new-hash");
        assert_eq!(coverage.line_ranges_read, vec![(51, 100)]);
        assert_eq!(coverage.get_unread_ranges(), vec![(1, 50)]);
    }

    #[test]
    fn whitelist_is_lru_bounded_and_recent_entries_survive() {
        let mut state = BashState::new();
        for index in 0..MAX_WHITELIST_FILES {
            let path = format!("/workspace/file-{index:04}.rs");
            state.record_read_coverage(&path, [(1, 1)], format!("hash-{index}"), 1);
        }

        // Refresh the oldest entry, then force one eviction. The next-oldest
        // entry must be discarded while the refreshed one stays guarded.
        state.record_read_coverage("/workspace/file-0000.rs", [(1, 1)], "hash-0".to_string(), 1);
        state.record_read_coverage("/workspace/newest.rs", [(1, 1)], "newest-hash".to_string(), 1);

        assert_eq!(state.whitelist_for_overwrite.len(), MAX_WHITELIST_FILES);
        assert!(state.whitelist_for_overwrite.contains_key("/workspace/file-0000.rs"));
        assert!(!state.whitelist_for_overwrite.contains_key("/workspace/file-0001.rs"));
        assert!(state.whitelist_for_overwrite.contains_key("/workspace/newest.rs"));
    }

    #[test]
    fn image_delivery_cache_survives_same_session_snapshot_sync_only() {
        let mut state = BashState::new();
        state.workspace_root = "/workspace/one".into();
        state.current_thread_id = "one".to_string();
        state.record_image_delivery("fingerprint".to_string());

        let same_session = state.snapshot();
        state.apply_snapshot(&same_session);
        assert!(state.image_was_delivered("fingerprint"));

        let mut other = BashState::new();
        other.workspace_root = "/workspace/two".into();
        other.current_thread_id = "two".to_string();
        state.apply_snapshot(&other.snapshot());
        assert!(!state.image_was_delivered("fingerprint"));
    }

    #[test]
    fn mutation_receipts_survive_snapshot_and_expire_closed() -> Result<(), &'static str> {
        let mut state = BashState::new();
        state.record_edit_mutation_receipt(EditMutationReceipt {
            schema_version: 0,
            fingerprint: "fingerprint".to_string(),
            receipt_id: "edit_receipt".to_string(),
            tool: "FileWriteOrEdit".to_string(),
            status: "completed".to_string(),
            committed_at_unix_ms: 1_000,
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            persisted: true,
            verification_pending: false,
            verification_id: None,
            legacy_verification_id: None,
            verification_state: None,
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        });

        let snapshot = state.snapshot();
        let mut restored = BashState::new();
        restored.apply_snapshot(&snapshot);
        let receipt = restored
            .edit_mutation_receipt("fingerprint", 1_500, 1_000)
            .ok_or("fresh persisted receipt missing")?;
        assert_eq!(receipt.receipt_id, "edit_receipt");
        assert!(receipt.persisted);
        assert!(restored.edit_mutation_receipt("fingerprint", 3_000, 1_000).is_none());
        Ok(())
    }

    #[test]
    fn receipt_bound_verification_updates_without_extending_commit_ttl() -> Result<(), &'static str>
    {
        let mut state = BashState::new();
        state.record_edit_mutation_receipt(EditMutationReceipt {
            schema_version: 1,
            fingerprint: "fingerprint".to_string(),
            receipt_id: "edit_receipt".to_string(),
            tool: "EditFiles".to_string(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: 1_000,
            postconditions: Vec::new(),
            persisted: true,
            verification_pending: true,
            verification_id: Some("verify_receipt".to_string()),
            legacy_verification_id: None,
            verification_state: Some(EditVerificationState::Pending),
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        });
        assert!(state.update_edit_mutation_verification(
            "verify_receipt",
            "completed",
            EditVerificationState::Passed,
            None,
            1_500,
            1_000,
        ));
        let receipt = state
            .edit_mutation_receipt("fingerprint", 1_500, 1_000)
            .ok_or("updated receipt missing")?;
        assert_eq!(receipt.status, "completed");
        assert!(!receipt.verification_pending);
        assert_eq!(receipt.committed_at_unix_ms, 1_000);
        assert!(state.edit_mutation_receipt("fingerprint", 3_000, 1_000).is_none());
        Ok(())
    }

    fn verification_receipt(
        fingerprint: &str,
        tool: &str,
        canonical_id: &str,
        legacy_id: Option<&str>,
    ) -> EditMutationReceipt {
        EditMutationReceipt {
            schema_version: 1,
            fingerprint: fingerprint.to_string(),
            receipt_id: format!("edit_{fingerprint}"),
            tool: tool.to_string(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: 1_000,
            postconditions: Vec::new(),
            persisted: true,
            verification_pending: true,
            verification_id: Some(canonical_id.to_string()),
            legacy_verification_id: legacy_id.map(str::to_string),
            verification_state: Some(EditVerificationState::Pending),
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        }
    }

    #[test]
    fn canonical_verification_lookup_ignores_reordered_legacy_shadow_collision(
    ) -> Result<(), &'static str> {
        let mut state = BashState::new();
        state.record_edit_mutation_receipt(verification_receipt(
            "canonical",
            "EditFiles",
            "verify_same",
            None,
        ));
        state.record_edit_mutation_receipt(verification_receipt(
            "shadow",
            "FileWriteOrEdit",
            "verify_same",
            Some("verify_legacy"),
        ));

        let shadow = state
            .edit_mutation_receipt_by_legacy_verification_id("verify_legacy", 1_001, 10_000)
            .ok_or("legacy shadow missing")?;
        assert_eq!(shadow.tool, "FileWriteOrEdit");
        let canonical = state
            .edit_mutation_receipt_by_verification_id("verify_same", 1_001, 10_000)
            .ok_or("canonical receipt missing")?;
        assert_eq!(canonical.tool, "EditFiles");
        assert_eq!(canonical.fingerprint, "canonical");
        Ok(())
    }

    #[test]
    fn mutation_receipt_lru_evicts_canonical_shadow_groups_atomically() {
        let mut state = BashState::new();
        for index in 0..=(EDIT_MUTATION_RECEIPT_CAP / 2) {
            let receipt_id = format!("edit-{index}");
            let canonical_id = format!("verify-{index}");
            let mut canonical = verification_receipt(
                &format!("canonical-{index}"),
                "EditFiles",
                &canonical_id,
                None,
            );
            canonical.receipt_id.clone_from(&receipt_id);
            let mut shadow = verification_receipt(
                &format!("legacy-{index}"),
                "FileWriteOrEdit",
                &canonical_id,
                Some(&format!("legacy-verify-{index}")),
            );
            shadow.receipt_id = receipt_id;
            state.record_edit_mutation_receipts([canonical, shadow]);
        }

        let groups = state.edit_mutation_receipts.iter().fold(
            HashMap::<&str, usize>::new(),
            |mut groups, receipt| {
                *groups.entry(receipt.receipt_id.as_str()).or_default() += 1;
                groups
            },
        );
        assert!(state.edit_mutation_receipts.len() <= EDIT_MUTATION_RECEIPT_CAP);
        assert!(groups.values().all(|count| *count == 2));
    }

    fn checkpoint(path: &str, _id: usize, prior_content: String) -> EditCheckpoint {
        EditCheckpoint {
            file_path_str: path.to_string(),
            path: path.into(),
            prior_content,
            prior_whitelist: None,
        }
    }

    #[test]
    fn undo_history_is_lifo_per_file_and_bounded_by_count() {
        let mut state = BashState::new();
        let mut ids = Vec::new();
        for id in 0..=(EDIT_CHECKPOINT_CAP + 2) {
            ids.push(
                state
                    .push_receipt_bound_edit_checkpoint(
                        checkpoint("/workspace/hot.rs", id, "x".to_string()),
                        format!("hash-{id}"),
                    )
                    .expect("retained newest checkpoint"),
            );
        }
        assert_eq!(state.undo_checkpoint_count_for("/workspace/hot.rs"), EDIT_CHECKPOINT_CAP);
        let latest = ids.last().expect("latest id");
        assert_eq!(state.next_undo_id_for("/workspace/hot.rs").as_deref(), Some(latest.as_str()));
        assert!(state.pop_latest_edit_checkpoint_by_id("/workspace/hot.rs", &ids[1]).is_none());

        for id in 0..(EDIT_CHECKPOINT_CAP + 8) {
            let path = format!("/workspace/file-{id}.rs");
            state.push_edit_checkpoint(checkpoint(&path, 100 + id, String::new()));
        }
        assert!(state.edit_checkpoints.len() <= EDIT_CHECKPOINT_CAP);
    }

    #[test]
    fn undo_content_and_metadata_budgets_are_deterministic() {
        let mut state = BashState::new();
        let chunk = "x".repeat(900_000);
        state.push_edit_checkpoint(checkpoint("/workspace/cold.rs", 1, chunk.clone()));
        for id in 2..=6 {
            state.push_edit_checkpoint(checkpoint("/workspace/hot.rs", id, chunk.clone()));
        }

        assert!(state.undo_content_bytes() <= EDIT_CHECKPOINT_CONTENT_BYTES_CAP);
        assert!(state.edit_checkpoints.len() <= EDIT_CHECKPOINT_CAP);

        let mut huge = checkpoint("/workspace/ranges.rs", 7, String::new());
        huge.prior_whitelist = Some(FileWhitelistData {
            file_hash: "h".repeat(1_024),
            line_ranges_read: (0..100_000).map(|index| (index * 2 + 1, index * 2 + 1)).collect(),
            total_lines: 200_000,
        });
        state.push_edit_checkpoint(huge);
        let retained = state.latest_edit_checkpoint_for("/workspace/ranges.rs").expect("retained");
        assert!(
            retained.prior_whitelist.expect("whitelist").line_ranges_read.len()
                <= EDIT_CHECKPOINT_MAX_READ_RANGES
        );
        assert!(state.undo_metadata_bytes() <= EDIT_CHECKPOINT_METADATA_BYTES_CAP);
    }

    #[test]
    fn undo_content_is_intentionally_absent_after_snapshot_restart() {
        let mut state = BashState::new();
        state.push_edit_checkpoint(checkpoint(
            "/workspace/file.rs",
            1,
            "private prior source".to_string(),
        ));
        state.record_edit_mutation_receipt(EditMutationReceipt {
            schema_version: 1,
            fingerprint: "apply-fingerprint".to_string(),
            receipt_id: "edit-apply".to_string(),
            tool: "EditFiles".to_string(),
            status: "completed".to_string(),
            committed_at_unix_ms: 1_000,
            postconditions: Vec::new(),
            persisted: true,
            verification_pending: false,
            verification_id: None,
            legacy_verification_id: None,
            verification_state: None,
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: Some("undo-next".to_string()),
            undo_ids: vec![Some("undo-current".to_string()), None],
        });
        let snapshot = state.snapshot();
        let mut restored = BashState::new();
        restored.apply_snapshot(&snapshot);

        assert!(restored.next_undo_id_for("/workspace/file.rs").is_none());
        assert_eq!(restored.undo_content_bytes(), 0);
        let replay = restored
            .edit_mutation_receipt("apply-fingerprint", 1_001, 10_000)
            .expect("mutation receipt remains available");
        assert!(replay.undo_ids.is_empty());
        assert!(replay.next_undo_id.is_none());
    }

    #[test]
    fn restart_interrupts_reserved_and_running_verifications_without_a_binding() {
        let mut state = BashState::new();
        for (fingerprint, verification_state, execution) in [
            ("reserved", EditVerificationState::Reserved, None),
            (
                "running",
                EditVerificationState::Running,
                Some(super::EditVerificationExecution {
                    generation: 9,
                    guardian_epoch: "old-guardian".to_string(),
                    session_epoch: "old-session".to_string(),
                }),
            ),
        ] {
            state.record_edit_mutation_receipt(EditMutationReceipt {
                schema_version: 1,
                fingerprint: fingerprint.to_string(),
                receipt_id: format!("edit-{fingerprint}"),
                tool: "EditFiles".to_string(),
                status: "completed_with_issues".to_string(),
                committed_at_unix_ms: 1_000,
                postconditions: Vec::new(),
                persisted: true,
                verification_pending: true,
                verification_id: Some(format!("verify-{fingerprint}")),
                legacy_verification_id: None,
                verification_state: Some(verification_state),
                verification_execution: execution,
                uncommitted_paths: Vec::new(),
                next_undo_id: None,
                undo_ids: Vec::new(),
            });
        }

        let snapshot = state.snapshot();
        let mut restored = BashState::new();
        restored.apply_snapshot(&snapshot);
        for fingerprint in ["reserved", "running"] {
            let receipt = restored
                .edit_mutation_receipt(fingerprint, 1_001, 10_000)
                .expect("receipt survives restart");
            assert_eq!(receipt.verification_state, Some(EditVerificationState::Interrupted));
            assert!(receipt.verification_execution.is_none());
            assert!(receipt.verification_pending);
        }
    }
}
