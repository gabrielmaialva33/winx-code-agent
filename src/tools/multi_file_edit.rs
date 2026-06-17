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
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::tools::file_write_or_edit::{commit_edit, plan_edit};
use crate::types::{normalize_thread_id, MultiFileEdit};

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
    let bash_state = bash_state_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

    let thread_id = normalize_thread_id(&multi.thread_id);
    if thread_id != bash_state.current_thread_id {
        return Err(WinxError::ThreadIdMismatch(thread_id));
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

    // PHASE 1: plan every file (validate + compute new content) with NO writes.
    // Any failure aborts the whole batch having touched nothing on disk.
    let mut planned = Vec::with_capacity(multi.files.len());
    for (index, entry) in multi.files.iter().enumerate() {
        let edit = plan_edit(
            bash_state,
            &entry.file_path,
            entry.percentage_to_change,
            &entry.text_or_search_replace_blocks,
        )
        .map_err(|e| {
            // Plan failures (bad path, mode gate, stale/unread file, SEARCH miss)
            // are all caused by the agent's input, so keep them client-classified
            // (invalid_request) rather than wrapping in a server-error variant.
            WinxError::ArgumentParseError(format!(
                "MultiFileEdit aborted before writing anything - file {} ({}) failed validation: {e}",
                index + 1,
                entry.file_path
            ))
        })?;
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
