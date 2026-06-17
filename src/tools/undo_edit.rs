//! Implementation of the `UndoEdit` tool.
//!
//! Reverts a file to the content it had before the last `FileWriteOrEdit` /
//! `MultiFileEdit` in this session, using the in-memory checkpoint those tools
//! record (see `bash_state::EditCheckpoint`). Per-file LIFO: repeated undos on
//! one file walk its edits back. A brand-new file's creation has no prior content
//! and is not undoable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::tools::file_write_or_edit::{ensure_parent_dirs, hash_content, write_no_follow};
use crate::types::{normalize_thread_id, UndoEdit};
use crate::utils::path::{expand_user, validate_path_in_workspace};

#[instrument(level = "info", skip(bash_state_arc, undo))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    undo: UndoEdit,
) -> Result<String> {
    let mut bash_state_guard = bash_state_arc.lock().await;
    let bash_state = bash_state_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

    let thread_id = normalize_thread_id(&undo.thread_id);
    if thread_id != bash_state.current_thread_id {
        return Err(WinxError::ThreadIdMismatch(thread_id));
    }

    // Resolve + workspace-confine the path exactly like the edit tools, so the key
    // matches the stored checkpoint and the write stays inside the workspace.
    let expanded = expand_user(&undo.file_path);
    let path = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        bash_state.cwd.join(&expanded)
    };
    let path = validate_path_in_workspace(&path, &bash_state.workspace_root)
        .map_err(|e| WinxError::PathSecurityError { path: path.clone(), message: e.to_string() })?;
    let file_path_str = path.to_string_lossy().to_string();

    // Refuse if the file changed since the edit we'd undo. The whitelist holds the
    // hash of the content winx last wrote; if the disk no longer matches, an undo
    // would silently discard those newer (external) changes.
    let on_disk = std::fs::read_to_string(&path).ok();
    let wrote_hash =
        bash_state.whitelist_for_overwrite.get(&file_path_str).map(|w| w.file_hash.clone());
    let unchanged = matches!((&on_disk, &wrote_hash), (Some(c), Some(h)) if &hash_content(c) == h);
    if !unchanged {
        return Err(WinxError::FileAccessError {
            path,
            message: format!(
                "{file_path_str} changed since its last winx edit (or was deleted), so UndoEdit \
                 was refused to avoid discarding those changes. Re-read the file and edit it \
                 manually."
            ),
        });
    }

    let Some(checkpoint) = bash_state.pop_edit_checkpoint_for(&file_path_str) else {
        return Err(WinxError::FileAccessError {
            path,
            message: format!(
                "No undo checkpoint for {file_path_str} in this session. winx keeps the last few \
                 edits per session in memory; a brand-new file's creation is not undoable."
            ),
        });
    };

    // Restore the prior content atomically, then roll the whitelist back so a
    // later edit's hash gate matches the reverted content (or drop it, forcing a
    // re-read, when there was none).
    ensure_parent_dirs(&path)?;
    write_no_follow(&path, checkpoint.prior_content.as_bytes())?;
    match checkpoint.prior_whitelist {
        Some(whitelist) => {
            bash_state.whitelist_for_overwrite.insert(file_path_str.clone(), whitelist);
        }
        None => {
            bash_state.whitelist_for_overwrite.remove(&file_path_str);
        }
    }

    let remaining =
        bash_state.edit_checkpoints.iter().filter(|cp| cp.file_path_str == file_path_str).count();
    let lines = checkpoint.prior_content.lines().count();
    Ok(format!(
        "Reverted {file_path_str} to its content before the last edit ({lines} lines). \
         {remaining} earlier checkpoint(s) remain for this file."
    ))
}
