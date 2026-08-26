use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::tool_policy::EditPermissionSet;
use crate::tool_registry::ToolKind;
use crate::tools::file_write_or_edit::resolve_edit_path;
use crate::types::{normalize_thread_id, LinePatch};

const MAX_FILES: usize = 100;
const MAX_TOTAL_PATCHES: usize = 1_024;
const MAX_AGGREGATE_CONTENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditOperation {
    Apply,
    Undo,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    Replace,
    SearchReplace,
    LinePatch,
    Undo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditChange {
    Replace { file_path: String, content: String },
    SearchReplace { file_path: String, content: String },
    LinePatch { file_path: String, expected_revision: String, patches: Vec<LinePatch> },
}

impl EditChange {
    pub fn file_path(&self) -> &str {
        match self {
            Self::Replace { file_path, .. }
            | Self::SearchReplace { file_path, .. }
            | Self::LinePatch { file_path, .. } => file_path,
        }
    }

    pub const fn mode(&self) -> EditMode {
        match self {
            Self::Replace { .. } => EditMode::Replace,
            Self::SearchReplace { .. } => EditMode::SearchReplace,
            Self::LinePatch { .. } => EditMode::LinePatch,
        }
    }

    fn canonical_value(&self, resolved_path: &str) -> Value {
        match self {
            Self::Replace { content, .. } => {
                json!({"file_path": resolved_path, "mode": "replace", "content": content})
            }
            Self::SearchReplace { content, .. } => json!({
                "file_path": resolved_path,
                "mode": "search_replace",
                "content": content
            }),
            Self::LinePatch { expected_revision, patches, .. } => json!({
                "file_path": resolved_path,
                "mode": "line_patch",
                "expected_revision": expected_revision,
                "patches": patches
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditCommand {
    Apply { changes: Vec<EditChange> },
    Undo { file_path: String, undo_id: Option<String>, legacy_lifo: bool },
}

impl EditCommand {
    pub const fn operation(&self) -> EditOperation {
        match self {
            Self::Apply { .. } => EditOperation::Apply,
            Self::Undo { .. } => EditOperation::Undo,
        }
    }

    pub fn modes(&self) -> Vec<EditMode> {
        match self {
            Self::Apply { changes } => changes.iter().map(EditChange::mode).collect(),
            Self::Undo { .. } => vec![EditMode::Undo],
        }
    }

    pub fn file_paths(&self) -> Vec<&str> {
        match self {
            Self::Apply { changes } => changes.iter().map(EditChange::file_path).collect(),
            Self::Undo { file_path, .. } => vec![file_path],
        }
    }

    pub fn file_count(&self) -> usize {
        self.file_paths().len()
    }
}

#[derive(Clone, Debug)]
pub struct EditVerification {
    pub command: String,
    pub wait_for_seconds: Option<f32>,
}

/// Final filesystem identity captured during edit preflight. Once constructed,
/// downstream planning and commit code must use this path directly and must not
/// resolve the caller's raw spelling (or the session cwd) again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalEditTarget {
    path: PathBuf,
    identity: String,
    existed_at_preflight: bool,
}

impl CanonicalEditTarget {
    pub(crate) fn from_preflight(path: PathBuf) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(WinxError::InvalidInput(
                "edit preflight produced a non-canonical target".to_string(),
            ));
        }
        let identity = path.to_string_lossy().into_owned();
        let existed_at_preflight = std::fs::symlink_metadata(&path).is_ok();
        Ok(Self { path, identity, existed_at_preflight })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) fn validate_binding(&self) -> Result<()> {
        let exists_now = std::fs::symlink_metadata(&self.path).is_ok();
        if exists_now != self.existed_at_preflight {
            return Err(self.binding_error("target existence changed after preflight"));
        }
        let current = if exists_now {
            self.path.canonicalize().map_err(|error| {
                self.binding_error(&format!("cannot revalidate target: {error}"))
            })?
        } else {
            resolve_missing_target(&self.path).map_err(|error| {
                self.binding_error(&format!("cannot revalidate target: {error}"))
            })?
        };
        if current != self.path {
            return Err(self.binding_error("a symlink or parent directory was retargeted"));
        }
        Ok(())
    }

    fn binding_error(&self, message: &str) -> WinxError {
        WinxError::FileChangedAfterEdit {
            path: self.path.clone(),
            message: format!(
                "canonical edit target changed after preflight ({message}); no write was attempted"
            ),
        }
    }
}

fn resolve_missing_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path;
    while std::fs::symlink_metadata(existing).is_err() {
        existing = existing.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no existing target ancestor")
        })?;
    }
    let mut resolved = existing.canonicalize()?;
    let remainder = path.strip_prefix(existing).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target escaped its captured ancestor",
        )
    })?;
    for component in remainder.components() {
        match component {
            Component::Normal(component) => resolved.push(component),
            Component::CurDir => {}
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "non-canonical target remainder",
                ));
            }
        }
    }
    Ok(resolved)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditSurface {
    FileWriteOrEdit,
    MultiFileEdit,
    ApplyPatch,
    UndoEdit,
    EditFiles,
}

impl EditSurface {
    pub const fn legacy_tool_kind(self) -> Option<ToolKind> {
        match self {
            Self::FileWriteOrEdit => Some(ToolKind::FileWriteOrEdit),
            Self::MultiFileEdit => Some(ToolKind::MultiFileEdit),
            Self::ApplyPatch => Some(ToolKind::ApplyPatch),
            Self::UndoEdit => Some(ToolKind::UndoEdit),
            Self::EditFiles => None,
        }
    }

    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::FileWriteOrEdit => "FileWriteOrEdit",
            Self::MultiFileEdit => "MultiFileEdit",
            Self::ApplyPatch => "ApplyPatch",
            Self::UndoEdit => "UndoEdit",
            Self::EditFiles => "EditFiles",
        }
    }

    pub const fn is_legacy(self) -> bool {
        !matches!(self, Self::EditFiles)
    }

    pub const fn from_public_tool(tool: ToolKind) -> Option<Self> {
        match tool {
            ToolKind::FileWriteOrEdit => Some(Self::FileWriteOrEdit),
            ToolKind::MultiFileEdit => Some(Self::MultiFileEdit),
            ToolKind::ApplyPatch => Some(Self::ApplyPatch),
            ToolKind::UndoEdit => Some(Self::UndoEdit),
            ToolKind::EditFiles => Some(Self::EditFiles),
            _ => None,
        }
    }

    pub(crate) const fn source_permissions(self) -> EditPermissionSet {
        match self.legacy_tool_kind() {
            Some(tool) => EditPermissionSet::for_legacy_tool(tool),
            None => EditPermissionSet::all_mutations(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NormalizedEditCall {
    pub surface: EditSurface,
    pub command: EditCommand,
    pub verification: Option<EditVerification>,
    pub thread_id: String,
    pub workspace_root: Option<String>,
    pub original_arguments: Value,
}

#[derive(Clone, Debug)]
pub struct PreparedEditContext {
    pub surface: EditSurface,
    pub command: EditCommand,
    pub verification: Option<EditVerification>,
    pub thread_id: String,
    pub workspace_root: Option<String>,
    /// Canonical project identity captured from the initialized session. Raw
    /// caller spelling is retained only for compatibility responses.
    pub canonical_workspace_root: String,
    pub(crate) targets: Vec<CanonicalEditTarget>,
    pub original_arguments: Value,
    pub(crate) effective_permissions: EditPermissionSet,
}

impl PreparedEditContext {
    pub fn prepare(
        normalized: NormalizedEditCall,
        state: &BashState,
        effective_permissions: EditPermissionSet,
    ) -> Result<Self> {
        let thread_id = normalize_thread_id(&normalized.thread_id);
        if thread_id != state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(thread_id));
        }

        let file_count = normalized.command.file_count();
        if file_count == 0 || file_count > MAX_FILES {
            return Err(WinxError::InvalidInput(format!(
                "EditFiles files must contain between 1 and {MAX_FILES} entries"
            )));
        }
        if matches!(normalized.command, EditCommand::Undo { .. }) && file_count != 1 {
            return Err(WinxError::InvalidInput(
                "EditFiles undo requires exactly one file".to_string(),
            ));
        }
        if normalized.verification.is_some() && !effective_permissions.allows_verification() {
            return Err(WinxError::CommandNotAllowed(
                "edit verification requires BashCommand authority in the effective tool policy"
                    .to_string(),
            ));
        }

        let source_permissions = normalized.surface.source_permissions();
        let mut targets = Vec::with_capacity(file_count);
        let mut seen = HashSet::with_capacity(file_count);
        for (mode, requested) in
            normalized.command.modes().into_iter().zip(normalized.command.file_paths())
        {
            if !effective_permissions.allows(mode, file_count)
                || !source_permissions.allows(mode, file_count)
            {
                return Err(WinxError::FileOperationDenied {
                    path: requested.into(),
                    message: format!(
                        "{} does not authorize mode={} with {} file(s)",
                        normalized.surface.tool_name(),
                        mode.as_str(),
                        file_count
                    ),
                });
            }
            let (_, resolved) = resolve_edit_path(state, requested)?;
            let target = CanonicalEditTarget::from_preflight(resolved)?;
            let resolved_string = target.identity().to_string();
            let mode_allowed = match mode {
                EditMode::Replace => state.is_file_write_allowed(&resolved_string),
                EditMode::SearchReplace | EditMode::LinePatch | EditMode::Undo => {
                    state.is_file_edit_allowed(&resolved_string)
                }
            };
            if !mode_allowed {
                return Err(WinxError::FileOperationDenied {
                    path: target.path().to_path_buf(),
                    message: format!(
                        "{} is not allowed for this path in the current shell mode",
                        mode.as_str()
                    ),
                });
            }
            if !seen.insert(resolved_string.clone()) {
                return Err(if normalized.surface == EditSurface::MultiFileEdit {
                    WinxError::ArgumentParseError(format!(
                        "MultiFileEdit targets '{resolved_string}' more than once; edits to the same file don't compose - combine them into a single entry."
                    ))
                } else {
                    WinxError::InvalidInput(format!(
                        "EditFiles targets {resolved_string:?} more than once; combine edits for one file"
                    ))
                });
            }
            targets.push(target);
        }

        Ok(Self {
            surface: normalized.surface,
            command: normalized.command,
            verification: normalized.verification,
            thread_id,
            workspace_root: normalized.workspace_root,
            canonical_workspace_root: state.workspace_root.to_string_lossy().into_owned(),
            targets,
            original_arguments: normalized.original_arguments,
            effective_permissions,
        })
    }

    /// Recheck every authorization decision while the session state is locked.
    /// This closes the gap between preflight and persisted-receipt lookup when a
    /// concurrent mode change tightens file authority.
    pub(crate) fn authorize_current_state(&self, state: &BashState) -> Result<()> {
        if self.thread_id != state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(self.thread_id.clone()));
        }
        let file_count = self.targets.len();
        for (mode, target) in self.command.modes().into_iter().zip(&self.targets) {
            // Binding is revalidated by the planner immediately before I/O.
            // Receipt replay deliberately checks only authority here: the
            // committed mutation itself may have created a previously missing
            // target, which must not invalidate an otherwise exact replay.
            let path = target.identity();
            if !self.effective_permissions.allows(mode, file_count) {
                return Err(WinxError::FileOperationDenied {
                    path: path.into(),
                    message: format!(
                        "effective tool policy no longer authorizes mode={} with {file_count} file(s)",
                        mode.as_str()
                    ),
                });
            }
            let allowed = match mode {
                EditMode::Replace => state.is_file_write_allowed(path),
                EditMode::SearchReplace | EditMode::LinePatch | EditMode::Undo => {
                    state.is_file_edit_allowed(path)
                }
            };
            if !allowed {
                return Err(WinxError::FileOperationDenied {
                    path: path.into(),
                    message: format!(
                        "{} is no longer allowed for this path in the current shell mode",
                        mode.as_str()
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn canonical_value(&self) -> Value {
        let command = match &self.command {
            EditCommand::Apply { changes } => json!({
                "operation": "apply",
                "files": changes
                    .iter()
                    .zip(&self.targets)
                    .map(|(change, target)| change.canonical_value(target.identity()))
                    .collect::<Vec<_>>()
            }),
            EditCommand::Undo { undo_id, .. } => json!({
                "operation": "undo",
                "files": [{
                    "file_path": self.targets.first().map(CanonicalEditTarget::identity).unwrap_or_default(),
                    "mode": "undo",
                    "undo_id": undo_id
                }]
            }),
        };
        json!({
            "schema": "EditFiles/v1",
            "thread_id": self.thread_id,
            "workspace_root": self.canonical_workspace_root,
            "command": command
        })
    }

    pub fn audit_summary(&self) -> String {
        format!(
            "operation={} files={} modes={} verify={}",
            self.command.operation().as_str(),
            self.targets.len(),
            self.command.modes().iter().map(|mode| mode.as_str()).collect::<Vec<_>>().join(","),
            self.verification.is_some()
        )
    }

    pub(crate) fn targets(&self) -> &[CanonicalEditTarget] {
        &self.targets
    }

    pub(crate) fn target_paths(&self) -> Vec<String> {
        self.targets.iter().map(|target| target.identity().to_string()).collect()
    }
}

impl EditOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Undo => "undo",
        }
    }
}

impl EditMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::SearchReplace => "search_replace",
            Self::LinePatch => "line_patch",
            Self::Undo => "undo",
        }
    }
}

pub(super) fn validate_aggregate(changes: &[EditChange]) -> Result<()> {
    let total_patches = changes
        .iter()
        .map(|change| match change {
            EditChange::LinePatch { patches, .. } => patches.len(),
            _ => 0,
        })
        .sum::<usize>();
    if total_patches > MAX_TOTAL_PATCHES {
        return Err(WinxError::InvalidInput(format!(
            "EditFiles is limited to {MAX_TOTAL_PATCHES} total line patches per call"
        )));
    }
    let total_content = changes
        .iter()
        .map(|change| match change {
            EditChange::Replace { content, .. } | EditChange::SearchReplace { content, .. } => {
                content.len()
            }
            EditChange::LinePatch { patches, .. } => {
                patches.iter().map(|patch| patch.replacement.len()).sum()
            }
        })
        .sum::<usize>();
    if total_content > MAX_AGGREGATE_CONTENT_BYTES {
        return Err(WinxError::InvalidInput(format!(
            "EditFiles payload content exceeds the {MAX_AGGREGATE_CONTENT_BYTES}-byte aggregate limit"
        )));
    }
    Ok(())
}
