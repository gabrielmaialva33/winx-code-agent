//! Implementation of the `MultiFileEdit` tool.
//!
//! Applies edits across several files all-or-nothing at the COMPUTE stage: every
//! file is validated and its new content computed in memory first (reusing
//! `file_write_or_edit::plan_edit`), and only if ALL succeed is anything written.
//! So a SEARCH block that fails to match in the last file leaves the earlier
//! files untouched, instead of the half-edited tree N separate `FileWriteOrEdit`
//! calls would leave.
//!
//! The write stage is a sequence of individually-atomic single-file renames
//! (`commit_edit` -> `write_no_follow`). It stops at the first I/O failure and
//! reports which files were already written; it does NOT roll them back (each is
//! already crash-safe on its own, and a second write pass could fail and corrupt
//! more state).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::tools::file_write_or_edit::{commit_edit, plan_edit};
use crate::types::{normalize_thread_id, FileEditEntry, MultiFileEdit};

/// Upper bound on files per batch. The whole batch holds the `bash_state` lock
/// across its (synchronous) file IO, so a huge batch would block the executor
/// and other sessions for a long time. A real multi-file refactor is well under
/// this; the cap is a guard against a pathological request.
const MAX_FILES_PER_BATCH: usize = 100;

#[instrument(level = "info", skip(bash_state_arc, multi))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    multi: MultiFileEdit,
) -> Result<String> {
    let mut bash_state_guard = bash_state_arc.lock().await;

    // Cheap validation up front (needs only the current thread id).
    {
        let bash_state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let thread_id = normalize_thread_id(&multi.thread_id);
        if thread_id != bash_state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(thread_id));
        }
    }
    if multi.files.len() < 2 {
        return Err(WinxError::ArgumentParseError(
            "MultiFileEdit needs at least 2 files; use FileWriteOrEdit for a single file."
                .to_string(),
        ));
    }
    if multi.files.len() > MAX_FILES_PER_BATCH {
        return Err(WinxError::ArgumentParseError(format!(
            "MultiFileEdit is limited to {MAX_FILES_PER_BATCH} files per batch (got {}); split the \
             change into smaller batches.",
            multi.files.len()
        )));
    }

    // Move the state onto the blocking pool for the synchronous plan+commit IO
    // (reading every file, then writing every file). The guard is held throughout,
    // so the slot stays locked — mutual exclusion is preserved; `take` just lets us
    // own the value across spawn_blocking. This frees the tokio worker instead of
    // pinning it on up to MAX_FILES_PER_BATCH file reads/writes.
    let mut state = bash_state_guard.take().ok_or(WinxError::BashStateNotInitialized)?;
    let recovery_state = state.clone();
    let files = multi.files;
    let joined = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_batch(&mut state, &files)
        }))
        .unwrap_or_else(|_| {
            Err(WinxError::CommandExecutionError(
                "MultiFileEdit panicked on the blocking worker".to_string(),
            ))
        });
        (state, result)
    })
    .await;

    match joined {
        Ok((state, result)) => {
            *bash_state_guard = Some(state);
            result
        }
        Err(error) => {
            *bash_state_guard = Some(recovery_state);
            Err(WinxError::CommandExecutionError(format!(
                "MultiFileEdit blocking task failed: {error}"
            )))
        }
    }
}

/// Plan every file (all-or-nothing at the compute stage), reject duplicate
/// targets, then commit sequentially. Synchronous (file IO) — runs on the
/// blocking pool, never on a tokio worker.
fn apply_batch(bash_state: &mut BashState, files: &[FileEditEntry]) -> Result<String> {
    // PHASE 1: plan every file (validate + compute new content) with NO writes.
    // Any failure aborts the whole batch having touched nothing on disk.
    let mut planned = Vec::with_capacity(files.len());
    for (index, entry) in files.iter().enumerate() {
        let edit = plan_edit(
            bash_state,
            &entry.file_path,
            entry.percentage_to_change,
            &entry.text_or_search_replace_blocks,
            false,
        )
        .map_err(|error| contextualize_plan_error(bash_state, index, entry, error))?;
        planned.push(edit);
    }

    // Reject duplicate targets, checked on the RESOLVED path (so `a.txt` and its
    // absolute form can't both slip through and clobber each other): two entries
    // for the same file don't compose - the second is computed from the original,
    // not the first's result, so it would silently overwrite the first.
    let mut seen = HashSet::with_capacity(planned.len());
    for edit in &planned {
        if !seen.insert(edit.target()) {
            return Err(WinxError::ArgumentParseError(format!(
                "MultiFileEdit targets '{}' more than once; edits to the same file don't compose - \
                 combine them into a single entry.",
                edit.target()
            )));
        }
    }

    let temp_edits = planned
        .iter()
        .map(|edit| crate::utils::agent_temp::TempEdit {
            path: edit.path(),
            previous_bytes: edit.previous_bytes(),
            new_bytes: edit.new_bytes(),
        })
        .collect::<Vec<_>>();
    crate::utils::agent_temp::validate_batch_quota(
        &bash_state.workspace_root,
        &bash_state.current_thread_id,
        &temp_edits,
    )?;

    // PHASE 2: commit sequentially. Each write is individually atomic (temp +
    // rename). On the first failure, stop and report honestly without rolling
    // back already-written files.
    let total = planned.len();
    let mut summaries = Vec::with_capacity(total);
    for (committed, edit) in planned.into_iter().enumerate() {
        let target = edit.target().to_string();
        match commit_edit(bash_state, edit) {
            Ok(summary) => summaries.push(format!("[{target}]\n{summary}")),
            Err(e) => {
                return Err(WinxError::CommandExecutionError(format!(
                    "MultiFileEdit: committed {committed} of {total} files, then failed writing \
                     {target}: {e}\nThe {committed} already-written file(s) were NOT rolled back. \
                     Re-read the affected files and retry the rest."
                )));
            }
        }
    }

    Ok(format!("MultiFileEdit applied all {total} edits:\n\n{}", summaries.join("\n\n")))
}

/// Preserve the original failure class so the MCP orchestration layer can emit
/// `needs_read`, stale-file recovery, or SEARCH conflict guidance. SEARCH errors
/// do not carry a path themselves, so wrap them in path-aware
/// `MultiFilePlanError` context without flattening the source into a string.
fn contextualize_plan_error(
    bash_state: &BashState,
    index: usize,
    entry: &FileEditEntry,
    error: WinxError,
) -> WinxError {
    let context = format!(
        "MultiFileEdit aborted before writing anything - file {} ({}) failed validation",
        index + 1,
        entry.file_path
    );
    match error {
        WinxError::FileAccessError { path, message } => {
            WinxError::FileAccessError { path, message: format!("{context}: {message}") }
        }
        WinxError::TemporaryArtifactPolicy { path, temporary_artifact_dir, message } => {
            WinxError::TemporaryArtifactPolicy {
                path,
                temporary_artifact_dir,
                message: format!("{context}: {message}"),
            }
        }
        source @ (WinxError::SearchBlockNotFound(_) | WinxError::SearchBlockAmbiguous { .. }) => {
            WinxError::MultiFilePlanError {
                index: index + 1,
                path: resolved_entry_path(bash_state, &entry.file_path),
                source: Box::new(source),
            }
        }
        WinxError::SearchReplaceSyntaxError(message) => {
            WinxError::SearchReplaceSyntaxError(format!("{context}: {message}"))
        }
        WinxError::SearchReplaceSyntaxErrorDetailed {
            message,
            line_number,
            block_type,
            suggestions,
        } => WinxError::SearchReplaceSyntaxErrorDetailed {
            message: format!("{context}: {message}"),
            line_number,
            block_type,
            suggestions,
        },
        other => other,
    }
}

fn resolved_entry_path(bash_state: &BashState, value: &str) -> PathBuf {
    let expanded = crate::utils::path::expand_user(value);
    let path = if Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        bash_state.cwd.join(expanded)
    };
    path.canonicalize().unwrap_or(path)
}
