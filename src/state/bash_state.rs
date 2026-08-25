#![allow(clippy::unwrap_used)]
use anyhow::Result;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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
        self.edit_mutation_receipts
            .retain(|receipt| now_unix_ms.saturating_sub(receipt.committed_at_unix_ms) <= ttl_ms);
        let index = self
            .edit_mutation_receipts
            .iter()
            .position(|receipt| receipt.fingerprint == fingerprint)?;
        let receipt = self.edit_mutation_receipts.remove(index)?;
        self.edit_mutation_receipts.push_back(receipt.clone());
        Some(receipt)
    }

    pub(crate) fn record_edit_mutation_receipt(&mut self, receipt: EditMutationReceipt) {
        if let Some(index) = self
            .edit_mutation_receipts
            .iter()
            .position(|existing| existing.fingerprint == receipt.fingerprint)
        {
            self.edit_mutation_receipts.remove(index);
        }
        self.edit_mutation_receipts.push_back(receipt);
        while self.edit_mutation_receipts.len() > EDIT_MUTATION_RECEIPT_CAP {
            self.edit_mutation_receipts.pop_front();
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
    /// cap. Large files are skipped (not undoable) to bound memory.
    pub fn push_edit_checkpoint(&mut self, checkpoint: EditCheckpoint) {
        if checkpoint.prior_content.len() > EDIT_CHECKPOINT_MAX_CONTENT_BYTES {
            info!(
                file = %checkpoint.file_path_str,
                "UndoEdit: not checkpointing a file over 1 MB (too large to hold in memory)"
            );
            return;
        }
        self.edit_checkpoints.push_back(checkpoint);
        while self.edit_checkpoints.len() > EDIT_CHECKPOINT_CAP {
            self.edit_checkpoints.pop_front();
        }
    }

    /// Remove and return the most recent checkpoint for `file_path_str` (per-file
    /// LIFO, so repeated undos on one file walk its edits back while leaving other
    /// files' checkpoints in place). `None` if that file has no checkpoint.
    pub fn pop_edit_checkpoint_for(&mut self, file_path_str: &str) -> Option<EditCheckpoint> {
        let index =
            self.edit_checkpoints.iter().rposition(|cp| cp.file_path_str == file_path_str)?;
        self.edit_checkpoints.remove(index)
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

pub fn generate_thread_id() -> String {
    let mut rng = rand::rng();
    format!("tid_{:x}", rng.random::<u64>())
}

#[cfg(test)]
mod whitelist_range_tests {
    use super::{
        BashState, EditMutationPostcondition, EditMutationReceipt, FileWhitelistData,
        MAX_WHITELIST_FILES,
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
}
