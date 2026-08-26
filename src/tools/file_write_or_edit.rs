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

use crate::errors::Result;
use crate::state::bash_state::BashState;
use crate::types::FileWriteOrEdit;
pub(crate) use commit::{
    commit_edit, ensure_parent_dirs, hash_content, invalidate_edit_read_permit_at_target,
    plan_explicit_text_edit_at_target, plan_revision_edit_at_target, resolve_edit_path,
    write_no_follow_if_unchanged, PlannedEdit,
};
pub(crate) use parser::uses_search_replace;

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_apply_blocks(original: &str, blocks: &str) {
    let _ = matcher::apply_blocks_with_unescape_retry(original, blocks);
}

#[instrument(level = "info", skip(bash_state_arc, file_write_or_edit))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    file_write_or_edit: FileWriteOrEdit,
) -> Result<String> {
    crate::tools::edit_files::handle_legacy_tool(
        bash_state_arc,
        crate::tools::edit_files::EditSurface::FileWriteOrEdit,
        file_write_or_edit,
    )
    .await
    .map(|outcome| outcome.text)
}
