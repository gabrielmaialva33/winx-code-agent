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
//! (`commit_edit` -> preconditioned atomic replacement). It stops at the first I/O failure and
//! reports which files were already written; it does NOT roll them back (each is
//! already crash-safe on its own, and a second write pass could fail and corrupt
//! more state).

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::Result;
use crate::state::bash_state::BashState;
use crate::types::MultiFileEdit;

#[instrument(level = "info", skip(bash_state_arc, multi))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    multi: MultiFileEdit,
) -> Result<String> {
    crate::tools::edit_files::handle_legacy_tool(
        bash_state_arc,
        crate::tools::edit_files::EditSurface::MultiFileEdit,
        multi,
    )
    .await
    .map(|outcome| outcome.text)
}
