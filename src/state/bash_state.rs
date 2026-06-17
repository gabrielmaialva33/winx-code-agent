#![allow(clippy::unwrap_used)]
use anyhow::Result;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::state::persistence::{
    delete_bash_state as delete_state_file, load_bash_state as load_state_file,
    save_bash_state as save_state_file, BashStateSnapshot,
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

impl FileWhitelistData {
    pub fn new(
        file_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
    ) -> Self {
        Self { file_hash, line_ranges_read, total_lines }
    }

    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= 99.0
    }

    pub fn get_percentage_read(&self) -> f64 {
        if self.total_lines == 0 {
            return 100.0;
        }
        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }
        (lines_read.len() as f64 / self.total_lines as f64) * 100.0
    }

    pub fn get_unread_ranges(&self) -> Vec<(usize, usize)> {
        if self.total_lines == 0 {
            return vec![];
        }
        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }
        let mut unread = vec![];
        let mut start_range = None;
        for i in 1..=self.total_lines {
            if !lines_read.contains(&i) {
                if start_range.is_none() {
                    start_range = Some(i);
                }
            } else if let Some(start) = start_range {
                unread.push((start, i - 1));
                start_range = None;
            }
        }
        if let Some(start) = start_range {
            unread.push((start, self.total_lines));
        }
        unread
    }

    pub fn add_range(&mut self, start: usize, end: usize) {
        self.line_ranges_read.push((start, end));
    }

    pub fn get_read_error_message(&self, file_path: &Path) -> String {
        format!(
            "File {} needs more reading. Coverage: {:.1}%",
            file_path.display(),
            self.get_percentage_read()
        )
    }

    pub fn needs_more_reading(&self) -> bool {
        !self.is_read_enough()
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
    pub pty_shell: Arc<Mutex<Option<PtyShell>>>,
    pub initialized: bool,
    /// In-memory ring of recent edit checkpoints for `UndoEdit` (newest at the
    /// back). Deliberately not part of `BashStateSnapshot`: undo is for immediate
    /// mid-session recovery, not across restarts.
    pub edit_checkpoints: VecDeque<EditCheckpoint>,
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
            pty_shell: Arc::new(Mutex::new(None)),
            initialized: false,
            edit_checkpoints: VecDeque::new(),
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

    pub async fn init_pty_shell(&mut self) -> Result<()> {
        let shell =
            PtyShell::new(&self.cwd, self.bash_command_mode.bash_mode == BashMode::RestrictedMode)?;
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
        self.bash_command_mode.allowed_commands.is_allowed(command)
    }

    pub fn is_file_edit_allowed(&self, path: &str) -> bool {
        self.file_edit_mode.allowed_globs.is_allowed(path)
    }

    pub fn is_file_write_allowed(&self, path: &str) -> bool {
        self.write_if_empty_mode.allowed_globs.is_allowed(path)
    }
    pub fn get_mode_violation_message(&self, op: &str, _target: &str) -> String {
        format!("Operation {op} not allowed")
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
            &self.mode,
            &self.bash_command_mode,
            &self.file_edit_mode,
            &self.write_if_empty_mode,
            &self.whitelist_for_overwrite,
            &self.current_thread_id,
        )
    }

    pub fn apply_snapshot(&mut self, snapshot: &BashStateSnapshot) {
        let (cwd, root, mode, bmode, emode, wmode, whitelist, tid) = snapshot.to_state_components();

        self.cwd = PathBuf::from(cwd);
        self.workspace_root = PathBuf::from(root);
        self.mode = mode;
        self.bash_command_mode = bmode;
        self.file_edit_mode = emode;
        self.write_if_empty_mode = wmode;
        self.whitelist_for_overwrite = whitelist;
        self.current_thread_id = tid;
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
