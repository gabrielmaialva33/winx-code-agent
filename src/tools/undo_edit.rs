//! Implementation of the `UndoEdit` tool.
//!
//! Reverts a file to the content it had before the last `FileWriteOrEdit` /
//! `MultiFileEdit` in this session, using the in-memory checkpoint those tools
//! record (see `bash_state::EditCheckpoint`). Per-file LIFO: repeated undos on
//! one file walk its edits back. A brand-new file's creation has no prior content
//! and is not undoable.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::Result;
use crate::state::bash_state::BashState;
use crate::types::UndoEdit;

#[instrument(level = "info", skip(bash_state_arc, undo))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    undo: UndoEdit,
) -> Result<String> {
    crate::tools::edit_files::handle_legacy_tool(
        bash_state_arc,
        crate::tools::edit_files::EditSurface::UndoEdit,
        undo,
    )
    .await
    .map(|outcome| outcome.text)
}
