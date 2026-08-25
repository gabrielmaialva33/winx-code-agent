//! `FileWriteOrEdit` facade.
//!
//! Parsing, fuzzy matching, atomic commit, and result rendering live in focused
//! submodules under `file_write_or_edit/`.

mod commit;
mod matcher;
mod parser;
mod report;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{normalize_thread_id, FileWriteOrEdit};
pub(crate) use commit::{
    commit_edit, ensure_parent_dirs, hash_content, invalidate_edit_read_permit, plan_edit,
    plan_revision_edit, write_no_follow_if_unchanged,
};

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_apply_blocks(original: &str, blocks: &str) {
    let _ = matcher::apply_blocks_with_unescape_retry(original, blocks);
}

#[instrument(level = "info", skip(bash_state_arc, file_write_or_edit))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    file_write_or_edit: FileWriteOrEdit,
) -> Result<String> {
    let mut bash_state_guard = bash_state_arc.lock().await;
    {
        let bash_state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let thread_id = normalize_thread_id(&file_write_or_edit.thread_id);
        if thread_id != bash_state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(thread_id));
        }
    }

    let mut state = bash_state_guard.take().ok_or(WinxError::BashStateNotInitialized)?;
    let recovery_state = state.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let planned = match plan_edit(
                &state,
                &file_write_or_edit.file_path,
                file_write_or_edit.percentage_to_change,
                &file_write_or_edit.text_or_search_replace_blocks,
                true,
            ) {
                Ok(planned) => planned,
                Err(error) => {
                    if error.is_search_match_conflict() {
                        invalidate_edit_read_permit(&mut state, &file_write_or_edit.file_path);
                    }
                    return Err(error);
                }
            };
            commit_edit(&mut state, planned)
        }))
        .unwrap_or_else(|_| {
            Err(WinxError::CommandExecutionError(
                "FileWriteOrEdit panicked on the blocking worker".to_string(),
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
                "FileWriteOrEdit blocking task failed: {error}"
            )))
        }
    }
}
