//! Unified typed file-mutation engine.
//!
//! Phase 1 keeps the legacy MCP catalog intact, but every old edit facade and
//! the dark `EditFiles` wire form lower into this module before planning.

mod domain;
mod wire;

use std::collections::HashSet;
use std::path::PathBuf;

#[cfg(test)]
pub(crate) use domain::CanonicalEditTarget;
pub use domain::{
    EditChange, EditCommand, EditMode, EditOperation, EditSurface, EditVerification,
    NormalizedEditCall, PreparedEditContext,
};
pub use wire::{normalize_edit_call, EditFileWire, EditFilesWire};

use crate::errors::{Result, WinxError};
use crate::server::SharedBashState;
use crate::state::bash_state::{BashState, EditCheckpoint, EditMutationPostcondition};
use crate::tools::file_write_or_edit::{
    commit_edit, ensure_parent_dirs, hash_content, invalidate_edit_read_permit_at_target,
    plan_explicit_text_edit_at_target, plan_revision_edit_at_target, write_no_follow_if_unchanged,
};

pub(crate) struct IndexedEditError {
    pub index: usize,
    pub path: PathBuf,
    pub mode: EditMode,
    pub source: WinxError,
}

impl IndexedEditError {
    fn into_winx(self, surface: EditSurface) -> WinxError {
        match surface {
            EditSurface::MultiFileEdit => WinxError::MultiFilePlanError {
                index: self.index + 1,
                path: self.path,
                source: Box::new(self.source),
            },
            EditSurface::EditFiles => WinxError::IndexedEditError {
                index: self.index + 1,
                path: self.path,
                mode: self.mode.as_str().to_string(),
                source: Box::new(self.source),
            },
            EditSurface::FileWriteOrEdit | EditSurface::ApplyPatch | EditSurface::UndoEdit => {
                WinxError::EditContextError { path: self.path, source: Box::new(self.source) }
            }
        }
    }
}

pub(crate) enum EditPlan {
    Apply(Vec<crate::tools::file_write_or_edit::PlannedEdit>),
    Undo(Box<UndoPlan>),
}

pub(crate) struct UndoPlan {
    path: PathBuf,
    file_path_str: String,
    on_disk: String,
    checkpoint: EditCheckpoint,
    undo_id: Option<String>,
    legacy_lifo: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct EditExecution {
    pub text: String,
    pub revisions: Vec<String>,
    pub undo_ids: Vec<Option<String>>,
    pub next_undo_id: Option<String>,
    pub postconditions: Vec<EditMutationPostcondition>,
    pub committed_paths: Vec<String>,
    pub uncommitted_paths: Vec<String>,
    pub partial_failure: Option<PartialEditFailure>,
}

#[derive(Clone, Debug)]
pub(crate) struct PartialEditFailure {
    pub failed_index: usize,
    pub failed_path: String,
    pub message: String,
}

/// Execute a normalized, pre-authorized command through one planner/committer.
pub(crate) async fn handle_prepared(
    bash_state_arc: &SharedBashState,
    prepared: PreparedEditContext,
) -> Result<EditExecution> {
    let mut bash_state_guard = bash_state_arc.lock().await;
    {
        let state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        if prepared.thread_id != state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(prepared.thread_id));
        }
        prepared.authorize_current_state(state)?;
    }

    let mut state = bash_state_guard.take().ok_or(WinxError::BashStateNotInitialized)?;
    let recovery_state = state.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let plan = plan_command(&mut state, &prepared)?;
            commit_plan(&mut state, &prepared, plan)
        }))
        .unwrap_or_else(|_| {
            Err(WinxError::CommandExecutionError(format!(
                "{} panicked on the blocking worker",
                prepared.surface.tool_name()
            )))
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
                "unified EditFiles blocking task failed: {error}"
            )))
        }
    }
}

/// Compatibility entry point used by the public Rust library facades. MCP
/// dispatch performs receipt orchestration around the same prepared engine;
/// these direct library calls intentionally preserve their historical return
/// types while sharing all planner/committer behavior.
pub(crate) async fn handle_legacy_tool<T: serde::Serialize>(
    bash_state_arc: &SharedBashState,
    surface: EditSurface,
    request: T,
) -> Result<EditExecution> {
    let arguments = serde_json::to_value(request)
        .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    let normalized = normalize_edit_call(surface, arguments)?;
    let prepared = {
        let state = bash_state_arc.lock().await;
        let state = state.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        PreparedEditContext::prepare(
            normalized,
            state,
            crate::tool_policy::ToolPolicy::default().edit_permissions(),
        )?
    };
    let outcome = handle_prepared(bash_state_arc, prepared)
        .await
        .map_err(|error| preserve_legacy_error(surface, error))?;
    if surface == EditSurface::MultiFileEdit {
        if let Some(partial) = outcome.partial_failure.as_ref() {
            return Err(WinxError::CommandExecutionError(partial.message.clone()));
        }
    }
    Ok(outcome)
}

/// Direct Rust callers of the historical single-file facades matched on the
/// concrete error variants before the unified engine added path context for
/// MCP recovery. Keep that public contract intact at the adapter boundary;
/// MCP dispatch calls `handle_prepared` directly and therefore retains the
/// structured path context it needs.
fn preserve_legacy_error(surface: EditSurface, error: WinxError) -> WinxError {
    match (surface, error) {
        (
            EditSurface::FileWriteOrEdit | EditSurface::ApplyPatch | EditSurface::UndoEdit,
            WinxError::EditContextError { source, .. },
        ) => *source,
        (_, error) => error,
    }
}

fn plan_command(state: &mut BashState, prepared: &PreparedEditContext) -> Result<EditPlan> {
    match &prepared.command {
        EditCommand::Apply { changes } => plan_apply(state, prepared, changes),
        EditCommand::Undo { undo_id, legacy_lifo, .. } => {
            let path = prepared
                .targets()
                .first()
                .ok_or_else(|| WinxError::InvalidInput("undo target is missing".to_string()))?;
            path.validate_binding()?;
            let path = path.path().to_path_buf();
            let file_path_str = path.to_string_lossy().into_owned();
            let (checkpoint, planned_undo_id, wrote_hash) = if let Some(undo_id) = undo_id {
                let (latest, latest_id, wrote_hash) =
                    state.latest_receipt_bound_checkpoint(&file_path_str).ok_or_else(|| {
                        WinxError::UndoExpired { path: path.clone(), undo_id: undo_id.clone() }
                    })?;
                if latest_id != *undo_id {
                    return Err(WinxError::UndoOutOfOrder {
                        path: path.clone(),
                        undo_id: undo_id.clone(),
                        latest_undo_id: latest_id,
                    });
                }
                (latest, Some(undo_id.clone()), wrote_hash)
            } else {
                let latest = state
                    .latest_edit_checkpoint_for(&file_path_str)
                    .ok_or_else(|| WinxError::UndoCheckpointNotFound { path: path.clone() })?;
                let wrote_hash = state
                    .whitelist_for_overwrite
                    .get(&file_path_str)
                    .map(|entry| entry.file_hash.clone())
                    .ok_or_else(|| WinxError::UndoCheckpointNotFound { path: path.clone() })?;
                (latest, None, wrote_hash)
            };
            if checkpoint.path != path || checkpoint.file_path_str != file_path_str {
                return Err(WinxError::FileChangedAfterEdit {
                    path,
                    message:
                        "undo checkpoint target no longer matches the canonical preflight target"
                            .to_string(),
                });
            }
            let on_disk = std::fs::read_to_string(&path).map_err(|error| {
                WinxError::FileChangedAfterEdit {
                    path: path.clone(),
                    message: format!("the file is unavailable before undo: {error}"),
                }
            })?;
            if hash_content(&on_disk) != wrote_hash {
                return Err(WinxError::FileChangedAfterEdit {
                    path,
                    message: format!(
                        "{file_path_str} changed since the Winx edit bound to this undo checkpoint; re-read it and preserve the newer content"
                    ),
                });
            }
            Ok(EditPlan::Undo(Box::new(UndoPlan {
                path: checkpoint.path.clone(),
                file_path_str,
                on_disk,
                checkpoint,
                undo_id: planned_undo_id,
                legacy_lifo: *legacy_lifo,
            })))
        }
    }
}

fn plan_apply(
    state: &mut BashState,
    prepared: &PreparedEditContext,
    changes: &[EditChange],
) -> Result<EditPlan> {
    let mut planned = Vec::with_capacity(changes.len());
    for (index, change) in changes.iter().enumerate() {
        let target = prepared.targets().get(index).ok_or_else(|| {
            WinxError::InvalidInput(format!("canonical target missing for edit {}", index + 1))
        })?;
        target.validate_binding()?;
        let result = match change {
            EditChange::Replace { content, .. } => {
                plan_explicit_text_edit_at_target(state, target.path(), false, content, false)
            }
            EditChange::SearchReplace { content, .. } => {
                plan_explicit_text_edit_at_target(state, target.path(), true, content, false)
            }
            EditChange::LinePatch { expected_revision, patches, .. } => {
                let required_ranges = crate::tools::apply_patch::required_read_ranges(patches);
                plan_revision_edit_at_target(
                    state,
                    target.path(),
                    expected_revision,
                    &required_ranges,
                    |content| crate::tools::apply_patch::apply_line_patches(content, patches),
                )
            }
        };
        match result {
            Ok(edit) => planned.push(edit),
            Err(error) => {
                if error.is_search_match_conflict() {
                    invalidate_edit_read_permit_at_target(state, target.path());
                }
                return Err(IndexedEditError {
                    index,
                    path: target.path().to_path_buf(),
                    mode: change.mode(),
                    source: error,
                }
                .into_winx(prepared.surface));
            }
        }
    }
    let mut seen = HashSet::with_capacity(planned.len());
    for edit in &planned {
        if !seen.insert(edit.target()) {
            return Err(WinxError::InvalidInput(format!(
                "EditFiles targets {:?} more than once",
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
            is_new: edit.previous_bytes() == 0 && !edit.path().exists(),
        })
        .collect::<Vec<_>>();
    crate::utils::agent_temp::validate_batch_quota(
        &state.workspace_root,
        &state.current_thread_id,
        &temp_edits,
    )?;
    Ok(EditPlan::Apply(planned))
}

fn commit_plan(
    state: &mut BashState,
    prepared: &PreparedEditContext,
    plan: EditPlan,
) -> Result<EditExecution> {
    match plan {
        EditPlan::Apply(planned) => {
            commit_apply_with(state, prepared, planned, |state, edit, _| commit_edit(state, edit))
        }
        EditPlan::Undo(plan) => {
            prepared
                .targets()
                .first()
                .ok_or_else(|| WinxError::InvalidInput("undo target is missing".to_string()))?
                .validate_binding()?;
            commit_undo(state, *plan)
        }
    }
}

fn commit_apply_with<F>(
    state: &mut BashState,
    prepared: &PreparedEditContext,
    planned: Vec<crate::tools::file_write_or_edit::PlannedEdit>,
    mut commit: F,
) -> Result<EditExecution>
where
    F: FnMut(
        &mut BashState,
        crate::tools::file_write_or_edit::PlannedEdit,
        usize,
    ) -> Result<String>,
{
    let targets = planned.iter().map(|edit| edit.target().to_string()).collect::<Vec<_>>();
    let total = targets.len();
    let mut summaries = Vec::with_capacity(total);
    let mut revisions = Vec::with_capacity(total);
    let mut undo_ids = Vec::with_capacity(total);
    let mut postconditions = Vec::with_capacity(total);
    for (committed, edit) in planned.into_iter().enumerate() {
        let binding = prepared.targets().get(committed).ok_or_else(|| {
            WinxError::InvalidInput(format!(
                "canonical target missing before commit {}",
                committed + 1
            ))
        })?;
        binding.validate_binding()?;
        let target = edit.target().to_string();
        let revision = edit.new_revision();
        let new_hash = edit.new_hash();
        let prior_undo = state.next_undo_id_for(&target);
        match commit(state, edit, committed) {
            Ok(summary) => {
                let current_undo = state.next_undo_id_for(&target);
                undo_ids.push((current_undo != prior_undo).then_some(current_undo).flatten());
                revisions.push(revision);
                postconditions
                    .push(EditMutationPostcondition { path: target.clone(), sha256: new_hash });
                summaries.push(format!("[{target}]\n{summary}"));
            }
            Err(error) => {
                if committed == 0 {
                    return Err(error);
                }
                let message = format!(
                            "{} committed {committed} of {total} files, then failed writing {target}: {error}. The committed prefix was not rolled back; re-read and retry only the uncommitted suffix.",
                            prepared.surface.tool_name()
                        );
                return Ok(EditExecution {
                    text: message.clone(),
                    revisions,
                    undo_ids,
                    next_undo_id: None,
                    postconditions,
                    committed_paths: targets[..committed].to_vec(),
                    uncommitted_paths: targets[committed..].to_vec(),
                    partial_failure: Some(PartialEditFailure {
                        failed_index: committed + 1,
                        failed_path: target,
                        message,
                    }),
                });
            }
        }
    }
    let text = match prepared.surface {
        EditSurface::MultiFileEdit => {
            format!("MultiFileEdit applied all {total} edits:\n\n{}", summaries.join("\n\n"))
        }
        EditSurface::EditFiles if total > 1 => {
            format!("EditFiles applied all {total} edits:\n\n{}", summaries.join("\n\n"))
        }
        _ => summaries
            .into_iter()
            .next()
            .and_then(|summary| summary.split_once('\n').map(|(_, text)| text.to_string()))
            .unwrap_or_default(),
    };
    Ok(EditExecution {
        text,
        revisions,
        undo_ids,
        next_undo_id: None,
        postconditions,
        committed_paths: targets,
        uncommitted_paths: Vec::new(),
        partial_failure: None,
    })
}

fn commit_undo(state: &mut BashState, plan: UndoPlan) -> Result<EditExecution> {
    ensure_parent_dirs(&plan.path)?;
    write_no_follow_if_unchanged(
        &plan.path,
        plan.checkpoint.prior_content.as_bytes(),
        Some(&plan.on_disk),
    )?;
    let removed = if plan.legacy_lifo {
        state.pop_edit_checkpoint_for(&plan.file_path_str)
    } else {
        state.pop_latest_edit_checkpoint_by_id(
            &plan.file_path_str,
            plan.undo_id.as_deref().unwrap_or_default(),
        )
    };
    if removed.is_none() {
        return Err(WinxError::UndoExpired {
            path: plan.path,
            undo_id: plan.undo_id.unwrap_or_default(),
        });
    }
    match plan.checkpoint.prior_whitelist {
        Some(whitelist) => state.set_whitelist_entry(&plan.file_path_str, whitelist),
        None => {
            state.remove_whitelist_entry(&plan.file_path_str);
        }
    }
    let remaining = state.undo_checkpoint_count_for(&plan.file_path_str);
    let next_undo_id = state.next_undo_id_for(&plan.file_path_str);
    let lines = plan.checkpoint.prior_content.lines().count();
    Ok(EditExecution {
        text: format!(
            "Reverted {} to its content before the last edit ({lines} lines). {remaining} earlier checkpoint(s) remain for this file.",
            plan.file_path_str
        ),
        revisions: vec![crate::tools::read_files::revision_from_hash(&hash_content(
            &plan.checkpoint.prior_content,
        ))],
        undo_ids: Vec::new(),
        next_undo_id,
        postconditions: vec![EditMutationPostcondition {
            path: plan.file_path_str.clone(),
            sha256: hash_content(&plan.checkpoint.prior_content),
        }],
        committed_paths: vec![plan.file_path_str],
        uncommitted_paths: Vec::new(),
        partial_failure: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;

    use super::*;
    use crate::tool_policy::ToolPolicy;
    use crate::types::{AllowedGlobs, Modes};
    use tokio::sync::Mutex;

    fn state_for(root: &std::path::Path) -> BashState {
        let mut state = BashState::new();
        state.initialized = true;
        state.current_thread_id = "thread".to_string();
        state.cwd = root.to_path_buf();
        state.workspace_root = root.to_path_buf();
        state.mode = Modes::Wcgw;
        state
    }

    fn canonical_identity(path: &std::path::Path) -> String {
        path.canonicalize().expect("test edit target must exist").to_string_lossy().into_owned()
    }

    fn record_full_read(state: &mut BashState, path: &std::path::Path, content: &str) {
        let total_lines = content.lines().count().max(1);
        state.record_read_coverage(
            &canonical_identity(path),
            [(1, total_lines)],
            hash_content(content),
            total_lines,
        );
    }

    fn prepare_dark(state: &BashState, files: &serde_json::Value) -> Result<PreparedEditContext> {
        let normalized = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "apply",
                "files": files,
                "thread_id": "thread",
                "workspace_root": state.workspace_root
            }),
        )?;
        PreparedEditContext::prepare(normalized, state, ToolPolicy::default().edit_permissions())
    }

    fn prepare_dark_undo(
        state: &BashState,
        file_path: &std::path::Path,
        undo_id: &str,
    ) -> Result<PreparedEditContext> {
        let normalized = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "undo",
                "files": [{
                    "file_path": file_path,
                    "mode": "undo",
                    "undo_id": undo_id
                }],
                "thread_id": "thread",
                "workspace_root": state.workspace_root
            }),
        )?;
        PreparedEditContext::prepare(normalized, state, ToolPolicy::default().edit_permissions())
    }

    #[test]
    fn explicit_modes_have_canonical_identity_independent_of_legacy_wire() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file.txt");
        let mut state = BashState::new();
        state.initialized = true;
        state.current_thread_id = "thread".to_string();
        state.cwd = root.path().to_path_buf();
        state.workspace_root = root.path().to_path_buf();
        state.mode = Modes::Wcgw;
        let legacy = normalize_edit_call(
            EditSurface::FileWriteOrEdit,
            serde_json::json!({
                "file_path": &path,
                "percentage_to_change": 100,
                "text_or_search_replace_blocks": "new\n",
                "thread_id": "thread",
                "workspace_root": root.path()
            }),
        )?;
        let modern = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "apply",
                "files": [{"file_path": path, "mode": "replace", "content": "new\n"}],
                "thread_id": "thread",
                "workspace_root": root.path()
            }),
        )?;
        let permissions = ToolPolicy::default().edit_permissions();
        let legacy = PreparedEditContext::prepare(legacy, &state, permissions)?;
        let modern = PreparedEditContext::prepare(modern, &state, permissions)?;
        assert_eq!(legacy.canonical_value(), modern.canonical_value());
        Ok(())
    }

    #[test]
    fn canonical_identity_ignores_equivalent_raw_workspace_spelling() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file.txt");
        let state = state_for(root.path());
        let legacy = normalize_edit_call(
            EditSurface::FileWriteOrEdit,
            serde_json::json!({
                "file_path": path,
                "percentage_to_change": 100,
                "text_or_search_replace_blocks": "new\n",
                "thread_id": "thread",
                "workspace_root": root.path().join(".")
            }),
        )?;
        let modern = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "apply",
                "files": [{"file_path": path, "mode": "replace", "content": "new\n"}],
                "thread_id": "thread",
                "workspace_root": root.path()
            }),
        )?;
        let permissions = ToolPolicy::default().edit_permissions();
        assert_eq!(
            PreparedEditContext::prepare(legacy, &state, permissions)?.canonical_value(),
            PreparedEditContext::prepare(modern, &state, permissions)?.canonical_value()
        );
        Ok(())
    }

    #[test]
    fn error_context_preserves_exact_legacy_surface_and_typed_dark_index() {
        let source = || WinxError::SearchBlockNotFound("missing block".to_string());
        let indexed = |source| IndexedEditError {
            index: 0,
            path: "/workspace/file.rs".into(),
            mode: EditMode::SearchReplace,
            source,
        };

        let single = indexed(source()).into_winx(EditSurface::FileWriteOrEdit);
        assert_eq!(single.to_string(), source().to_string());
        assert!(!single.to_string().contains("MultiFileEdit aborted"));
        assert!(matches!(single, WinxError::EditContextError { .. }));

        let patch = indexed(source()).into_winx(EditSurface::ApplyPatch);
        assert_eq!(patch.to_string(), source().to_string());
        assert!(!patch.to_string().contains("MultiFileEdit aborted"));

        let dark = indexed(source()).into_winx(EditSurface::EditFiles);
        assert!(matches!(dark, WinxError::IndexedEditError { .. }));
        assert!(dark.to_string().starts_with("EditFiles aborted before writing anything"));
        assert!(!dark.to_string().contains("MultiFileEdit aborted"));

        let legacy_batch = indexed(source()).into_winx(EditSurface::MultiFileEdit);
        assert_eq!(
            legacy_batch.to_string(),
            "MultiFileEdit aborted before writing anything - file 1 (/workspace/file.rs) failed validation: Search block not found in content: missing block"
        );
    }

    #[test]
    fn prepared_authorization_fails_closed_after_mode_tightening() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file.txt");
        let mut state = state_for(root.path());
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([{
                "file_path": path,
                "mode": "replace",
                "content": "new\n"
            }]),
        )?;

        state.mode = Modes::Architect;
        state.write_if_empty_mode.allowed_globs = AllowedGlobs::List(Vec::new());
        state.file_edit_mode.allowed_globs = AllowedGlobs::List(Vec::new());
        assert!(matches!(
            prepared.authorize_current_state(&state),
            Err(WinxError::FileOperationDenied { .. })
        ));
        Ok(())
    }

    #[test]
    fn dark_alias_respects_each_custom_legacy_permission_shape_without_widening() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = state_for(root.path());
        let first = root.path().join("first.rs");
        let second = root.path().join("second.rs");
        let normalize = |files| {
            normalize_edit_call(
                EditSurface::EditFiles,
                serde_json::json!({
                    "operation": "apply",
                    "files": files,
                    "thread_id": "thread",
                    "workspace_root": root.path()
                }),
            )
        };

        let single_replace = normalize(serde_json::json!([{
            "file_path": first,
            "mode": "replace",
            "content": "one"
        }]))?;
        let batch_replace = normalize(serde_json::json!([
            {"file_path": first, "mode": "replace", "content": "one"},
            {"file_path": second, "mode": "replace", "content": "two"}
        ]))?;
        let line_patch = normalize(serde_json::json!([{
            "file_path": first,
            "mode": "line_patch",
            "expected_revision": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "patches": [{"start_line": 1, "delete_lines": 0, "replacement": "one"}]
        }]))?;

        let file_write = ToolPolicy::from_allowed_tools(["FileWriteOrEdit"])?;
        assert!(PreparedEditContext::prepare(
            single_replace.clone(),
            &state,
            file_write.edit_permissions()
        )
        .is_ok());
        assert!(PreparedEditContext::prepare(
            batch_replace.clone(),
            &state,
            file_write.edit_permissions()
        )
        .is_err());
        assert!(PreparedEditContext::prepare(
            line_patch.clone(),
            &state,
            file_write.edit_permissions()
        )
        .is_err());

        let apply_patch = ToolPolicy::from_allowed_tools(["ApplyPatch"])?;
        assert!(PreparedEditContext::prepare(line_patch, &state, apply_patch.edit_permissions())
            .is_ok());
        assert!(PreparedEditContext::prepare(
            single_replace.clone(),
            &state,
            apply_patch.edit_permissions()
        )
        .is_err());

        let multi = ToolPolicy::from_allowed_tools(["MultiFileEdit"])?;
        assert!(
            PreparedEditContext::prepare(batch_replace, &state, multi.edit_permissions()).is_ok()
        );
        assert!(
            PreparedEditContext::prepare(single_replace, &state, multi.edit_permissions()).is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn receipt_bound_undo_is_strict_lifo_even_when_old_written_content_cycles() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file.txt");
        std::fs::write(&path, "one\n")?;
        let mut state = state_for(root.path());
        record_full_read(&mut state, &path, "one\n");
        let path_identity = canonical_identity(&path);
        let slot = Arc::new(Mutex::new(Some(state)));

        let mut undo_ids = Vec::new();
        for content in ["two\n", "one\n", "two\n"] {
            let prepared = {
                let guard = slot.lock().await;
                prepare_dark(
                    guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?,
                    &serde_json::json!([{
                        "file_path": path,
                        "mode": "replace",
                        "content": content
                    }]),
                )?
            };
            let outcome = handle_prepared(&slot, prepared).await?;
            undo_ids.push(
                outcome
                    .undo_ids
                    .first()
                    .and_then(Clone::clone)
                    .ok_or_else(|| WinxError::ParseError("missing undo id".to_string()))?,
            );
        }
        assert_eq!(std::fs::read_to_string(&path)?, "two\n");

        let stale_undo = {
            let guard = slot.lock().await;
            let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
            prepare_dark_undo(state, &path, &undo_ids[0])?
        };
        let Err(error) = handle_prepared(&slot, stale_undo).await else {
            return Err(WinxError::ParseError("stale undo unexpectedly succeeded".to_string()));
        };
        assert!(matches!(error, WinxError::UndoOutOfOrder { .. }));
        assert_eq!(std::fs::read_to_string(&path)?, "two\n");

        let latest = {
            let guard = slot.lock().await;
            let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
            prepare_dark_undo(state, &path, &undo_ids[2])?
        };
        let outcome = handle_prepared(&slot, latest.clone()).await?;
        assert_eq!(outcome.next_undo_id.as_deref(), Some(undo_ids[1].as_str()));
        assert_eq!(std::fs::read_to_string(&path)?, "one\n");

        let count = slot
            .lock()
            .await
            .as_ref()
            .ok_or(WinxError::BashStateNotInitialized)?
            .undo_checkpoint_count_for(&path_identity);
        let Err(replay_error) = handle_prepared(&slot, latest).await else {
            return Err(WinxError::ParseError(
                "duplicate undo unexpectedly popped twice".to_string(),
            ));
        };
        assert!(matches!(replay_error, WinxError::UndoOutOfOrder { .. }));
        assert_eq!(
            slot.lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .undo_checkpoint_count_for(&path_identity),
            count
        );
        Ok(())
    }

    #[tokio::test]
    async fn relative_target_is_bound_to_preflight_cwd() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first_dir = root.path().join("first");
        let second_dir = root.path().join("second");
        std::fs::create_dir_all(&first_dir)?;
        std::fs::create_dir_all(&second_dir)?;
        let first = first_dir.join("same.txt");
        let second = second_dir.join("same.txt");
        std::fs::write(&first, "first old\n")?;
        std::fs::write(&second, "second old\n")?;
        let mut state = state_for(root.path());
        state.cwd.clone_from(&first_dir);
        record_full_read(&mut state, &first, "first old\n");
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([{
                "file_path": "same.txt",
                "mode": "replace",
                "content": "first new\n"
            }]),
        )?;
        state.cwd = second_dir;
        let slot = Arc::new(Mutex::new(Some(state)));

        handle_prepared(&slot, prepared).await?;
        assert_eq!(std::fs::read_to_string(first)?, "first new\n");
        assert_eq!(std::fs::read_to_string(second)?, "second old\n");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_retarget_after_preflight_cannot_redirect_edit_or_undo() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let first_dir = root.path().join("first");
        let second_dir = root.path().join("second");
        std::fs::create_dir_all(&first_dir)?;
        std::fs::create_dir_all(&second_dir)?;
        let first = first_dir.join("file.txt");
        let second = second_dir.join("file.txt");
        std::fs::write(&first, "first old\n")?;
        std::fs::write(&second, "second old\n")?;
        let link = root.path().join("current");
        symlink(&first_dir, &link)?;

        let mut state = state_for(root.path());
        record_full_read(&mut state, &first, "first old\n");
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([{
                "file_path": "current/file.txt",
                "mode": "replace",
                "content": "first new\n"
            }]),
        )?;
        std::fs::remove_file(&link)?;
        symlink(&second_dir, &link)?;
        let slot = Arc::new(Mutex::new(Some(state)));
        let outcome = handle_prepared(&slot, prepared).await?;
        assert_eq!(std::fs::read_to_string(&first)?, "first new\n");
        assert_eq!(std::fs::read_to_string(&second)?, "second old\n");

        std::fs::remove_file(&link)?;
        symlink(&first_dir, &link)?;
        let undo_id = outcome
            .undo_ids
            .first()
            .and_then(Clone::clone)
            .ok_or_else(|| WinxError::ParseError("missing undo id".to_string()))?;
        let undo = {
            let guard = slot.lock().await;
            prepare_dark_undo(
                guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?,
                &link.join("file.txt"),
                &undo_id,
            )?
        };
        std::fs::remove_file(&link)?;
        symlink(&second_dir, &link)?;
        handle_prepared(&slot, undo).await?;
        assert_eq!(std::fs::read_to_string(first)?, "first old\n");
        assert_eq!(std::fs::read_to_string(second)?, "second old\n");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_parent_retarget_cannot_redirect_commit_even_with_identical_bytes(
    ) -> Result<()> {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let parent = workspace.path().join("bound");
        let saved_parent = workspace.path().join("bound-original");
        std::fs::create_dir(&parent)?;
        let intended = parent.join("file.txt");
        let redirected = outside.path().join("file.txt");
        std::fs::write(&intended, "same bytes\n")?;
        std::fs::write(&redirected, "same bytes\n")?;

        let mut state = state_for(workspace.path());
        record_full_read(&mut state, &intended, "same bytes\n");
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([{
                "file_path": intended,
                "mode": "replace",
                "content": "agent replacement\n"
            }]),
        )?;

        std::fs::rename(&parent, &saved_parent)?;
        symlink(outside.path(), &parent)?;
        let slot = Arc::new(Mutex::new(Some(state)));
        let error = handle_prepared(&slot, prepared)
            .await
            .expect_err("retargeted canonical parent must fail closed");

        assert!(matches!(error, WinxError::FileChangedAfterEdit { .. }));
        assert_eq!(std::fs::read_to_string(saved_parent.join("file.txt"))?, "same bytes\n");
        assert_eq!(std::fs::read_to_string(redirected)?, "same bytes\n");
        Ok(())
    }

    #[test]
    fn injected_write_stage_failure_reports_committed_prefix() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = root.path().join("first.txt");
        let second = root.path().join("second.txt");
        std::fs::write(&first, "first old\n")?;
        std::fs::write(&second, "second old\n")?;
        let mut state = state_for(root.path());
        record_full_read(&mut state, &first, "first old\n");
        record_full_read(&mut state, &second, "second old\n");
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([
                {"file_path": first, "mode": "replace", "content": "first new\n"},
                {"file_path": second, "mode": "replace", "content": "second new\n"}
            ]),
        )?;
        let EditPlan::Apply(planned) = plan_command(&mut state, &prepared)? else {
            return Err(WinxError::ParseError("expected apply plan".to_string()));
        };
        let outcome = commit_apply_with(&mut state, &prepared, planned, |state, edit, index| {
            if index == 1 {
                return Err(WinxError::FileWriteError {
                    path: edit.path().to_path_buf(),
                    message: "injected write-stage failure".to_string(),
                });
            }
            commit_edit(state, edit)
        })?;

        assert!(outcome.partial_failure.is_some());
        assert_eq!(outcome.committed_paths, vec![canonical_identity(&first)]);
        assert_eq!(outcome.uncommitted_paths, vec![canonical_identity(&second)]);
        assert_eq!(std::fs::read_to_string(first)?, "first new\n");
        assert_eq!(std::fs::read_to_string(second)?, "second old\n");
        Ok(())
    }

    #[test]
    fn postcondition_is_the_planned_commit_not_a_later_external_write() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file.txt");
        std::fs::write(&path, "old\n")?;
        let mut state = state_for(root.path());
        record_full_read(&mut state, &path, "old\n");
        let prepared = prepare_dark(
            &state,
            &serde_json::json!([{
                "file_path": &path,
                "mode": "replace",
                "content": "committed\n"
            }]),
        )?;
        let EditPlan::Apply(planned) = plan_command(&mut state, &prepared)? else {
            return Err(WinxError::ParseError("expected apply plan".to_string()));
        };
        let external_path = path.clone();
        let outcome = commit_apply_with(&mut state, &prepared, planned, |state, edit, _| {
            let summary = commit_edit(state, edit)?;
            std::fs::write(&external_path, "external\n")?;
            Ok(summary)
        })?;

        assert_eq!(outcome.postconditions.len(), 1);
        assert_eq!(outcome.postconditions[0].sha256, hash_content("committed\n"));
        assert_ne!(outcome.postconditions[0].sha256, hash_content("external\n"));
        assert_eq!(std::fs::read_to_string(path)?, "external\n");
        Ok(())
    }
}
