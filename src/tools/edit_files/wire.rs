use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::domain::{
    validate_aggregate, EditChange, EditCommand, EditMode, EditOperation, EditSurface,
    EditVerification, NormalizedEditCall,
};
use crate::errors::{Result, WinxError};
use crate::tools::file_write_or_edit::uses_search_replace;
use crate::types::{ApplyPatch, FileWriteOrEdit, LinePatch, MultiFileEdit, UndoEdit};

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFilesWire {
    pub operation: EditOperation,
    pub files: Vec<EditFileWire>,
    #[serde(default)]
    pub verify_command: Option<String>,
    #[serde(default)]
    pub verify_wait_for_seconds: Option<f32>,
    pub thread_id: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileWire {
    pub file_path: String,
    pub mode: EditMode,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<String>,
    #[serde(default)]
    pub patches: Option<Vec<LinePatch>>,
    #[serde(default)]
    pub undo_id: Option<String>,
}

pub fn normalize_edit_call(surface: EditSurface, arguments: Value) -> Result<NormalizedEditCall> {
    let original_arguments = arguments.clone();
    if surface == EditSurface::EditFiles {
        let request: EditFilesWire = deserialize(arguments, surface.tool_name())?;
        return normalize_new_wire(request, original_arguments);
    }
    let workspace_root =
        arguments.get("workspace_root").and_then(Value::as_str).map(str::to_string);
    let verification = parse_verification(&arguments)?;
    let mut domain_arguments = arguments;
    if let Some(map) = domain_arguments.as_object_mut() {
        map.remove("workspace_root");
        map.remove("verify_command");
        map.remove("verify_wait_for_seconds");
    }

    let (surface, thread_id, command) =
        normalize_legacy_wire(surface, domain_arguments, verification.is_some())?;
    let changes = match &command {
        EditCommand::Apply { changes } => changes.as_slice(),
        EditCommand::Undo { .. } => &[],
    };
    validate_aggregate(changes)?;
    Ok(NormalizedEditCall {
        surface,
        command,
        verification,
        thread_id,
        workspace_root,
        original_arguments,
    })
}

fn normalize_legacy_wire(
    surface: EditSurface,
    arguments: Value,
    has_verification: bool,
) -> Result<(EditSurface, String, EditCommand)> {
    Ok(match surface {
        EditSurface::FileWriteOrEdit => {
            let request: FileWriteOrEdit = deserialize(arguments, surface.tool_name())?;
            let mode = if uses_search_replace(
                request.percentage_to_change,
                &request.text_or_search_replace_blocks,
            ) {
                EditMode::SearchReplace
            } else {
                EditMode::Replace
            };
            let change =
                text_change(mode, request.file_path, request.text_or_search_replace_blocks)?;
            (
                EditSurface::FileWriteOrEdit,
                request.thread_id,
                EditCommand::Apply { changes: vec![change] },
            )
        }
        EditSurface::MultiFileEdit => {
            let request: MultiFileEdit = deserialize(arguments, surface.tool_name())?;
            if request.files.len() < 2 {
                return Err(WinxError::ArgumentParseError(
                    "MultiFileEdit needs at least 2 files; use FileWriteOrEdit for a single file."
                        .to_string(),
                ));
            }
            if request.files.len() > 100 {
                return Err(WinxError::ArgumentParseError(format!(
                    "MultiFileEdit is limited to 100 files per batch (got {}); split the change into smaller batches.",
                    request.files.len()
                )));
            }
            let changes = request
                .files
                .into_iter()
                .map(|entry| {
                    let mode = if uses_search_replace(
                        entry.percentage_to_change,
                        &entry.text_or_search_replace_blocks,
                    ) {
                        EditMode::SearchReplace
                    } else {
                        EditMode::Replace
                    };
                    text_change(mode, entry.file_path, entry.text_or_search_replace_blocks)
                })
                .collect::<Result<Vec<_>>>()?;
            (EditSurface::MultiFileEdit, request.thread_id, EditCommand::Apply { changes })
        }
        EditSurface::ApplyPatch => {
            let request: ApplyPatch = deserialize(arguments, surface.tool_name())?;
            crate::tools::apply_patch::validate_revision(&request.expected_revision)?;
            crate::tools::apply_patch::validate_patch_count(&request.patches)?;
            let change = EditChange::LinePatch {
                file_path: request.file_path,
                expected_revision: request.expected_revision,
                patches: request.patches,
            };
            (
                EditSurface::ApplyPatch,
                request.thread_id,
                EditCommand::Apply { changes: vec![change] },
            )
        }
        EditSurface::UndoEdit => {
            if has_verification {
                return Err(WinxError::InvalidInput(
                    "UndoEdit does not accept verification fields".to_string(),
                ));
            }
            let request: UndoEdit = deserialize(arguments, surface.tool_name())?;
            (
                EditSurface::UndoEdit,
                request.thread_id,
                EditCommand::Undo {
                    file_path: request.file_path,
                    undo_id: None,
                    legacy_lifo: true,
                },
            )
        }
        EditSurface::EditFiles => unreachable!("EditFiles is normalized before legacy lowering"),
    })
}

fn normalize_new_wire(
    request: EditFilesWire,
    original_arguments: Value,
) -> Result<NormalizedEditCall> {
    if request.files.is_empty() || request.files.len() > 100 {
        return Err(WinxError::InvalidInput(
            "EditFiles files must contain between 1 and 100 entries".to_string(),
        ));
    }
    let verification =
        parse_verification_fields(request.verify_command, request.verify_wait_for_seconds)?;
    let command = match request.operation {
        EditOperation::Apply => {
            let changes = request
                .files
                .into_iter()
                .enumerate()
                .map(|(index, file)| apply_wire_change(index, file))
                .collect::<Result<Vec<_>>>()?;
            validate_aggregate(&changes)?;
            EditCommand::Apply { changes }
        }
        EditOperation::Undo => {
            if verification.is_some() {
                return Err(WinxError::InvalidInput(
                    "EditFiles verification fields are valid only for operation=apply".to_string(),
                ));
            }
            if request.files.len() != 1 {
                return Err(WinxError::InvalidInput(
                    "EditFiles operation=undo requires exactly one file".to_string(),
                ));
            }
            let file = request.files.into_iter().next().ok_or_else(|| {
                WinxError::InvalidInput("EditFiles undo target is missing".to_string())
            })?;
            if file.mode != EditMode::Undo {
                return Err(WinxError::InvalidInput(
                    "EditFiles operation=undo requires files[0].mode=undo".to_string(),
                ));
            }
            reject_payload_fields(&file, "undo")?;
            let undo_id = file.undo_id.filter(|id| !id.trim().is_empty()).ok_or_else(|| {
                WinxError::InvalidInput(
                    "EditFiles operation=undo requires the exact non-empty undo_id returned by the edit"
                        .to_string(),
                )
            })?;
            EditCommand::Undo {
                file_path: file.file_path,
                undo_id: Some(undo_id),
                legacy_lifo: false,
            }
        }
    };
    Ok(NormalizedEditCall {
        surface: EditSurface::EditFiles,
        command,
        verification,
        thread_id: request.thread_id,
        workspace_root: request.workspace_root,
        original_arguments,
    })
}

fn apply_wire_change(index: usize, file: EditFileWire) -> Result<EditChange> {
    if file.mode == EditMode::Undo {
        return Err(WinxError::InvalidInput(format!(
            "files[{index}].mode=undo requires operation=undo and cannot be mixed with apply"
        )));
    }
    if file.undo_id.is_some() {
        return Err(WinxError::InvalidInput(format!(
            "files[{index}].undo_id is valid only for mode=undo"
        )));
    }
    match file.mode {
        EditMode::Replace | EditMode::SearchReplace => {
            if file.expected_revision.is_some() || file.patches.is_some() {
                return Err(WinxError::InvalidInput(format!(
                    "files[{index}] text modes accept content only"
                )));
            }
            let content = file.content.ok_or_else(|| {
                WinxError::InvalidInput(format!("files[{index}].content is required"))
            })?;
            text_change(file.mode, file.file_path, content)
        }
        EditMode::LinePatch => {
            if file.content.is_some() {
                return Err(WinxError::InvalidInput(format!(
                    "files[{index}].content is not valid for line_patch"
                )));
            }
            let expected_revision = file.expected_revision.ok_or_else(|| {
                WinxError::InvalidInput(format!(
                    "files[{index}].expected_revision is required for line_patch"
                ))
            })?;
            let patches = file.patches.ok_or_else(|| {
                WinxError::InvalidInput(format!(
                    "files[{index}].patches is required for line_patch"
                ))
            })?;
            crate::tools::apply_patch::validate_revision(&expected_revision)?;
            crate::tools::apply_patch::validate_patch_count(&patches)?;
            Ok(EditChange::LinePatch { file_path: file.file_path, expected_revision, patches })
        }
        EditMode::Undo => unreachable!("handled above"),
    }
}

fn reject_payload_fields(file: &EditFileWire, mode: &str) -> Result<()> {
    if file.content.is_some() || file.expected_revision.is_some() || file.patches.is_some() {
        return Err(WinxError::InvalidInput(format!(
            "mode={mode} does not accept content, expected_revision, or patches"
        )));
    }
    Ok(())
}

fn text_change(mode: EditMode, file_path: String, content: String) -> Result<EditChange> {
    match mode {
        EditMode::Replace => Ok(EditChange::Replace { file_path, content }),
        EditMode::SearchReplace => Ok(EditChange::SearchReplace { file_path, content }),
        _ => Err(WinxError::InvalidInput("invalid text edit mode".to_string())),
    }
}

fn parse_verification(arguments: &Value) -> Result<Option<EditVerification>> {
    let command = arguments
        .get("verify_command")
        .filter(|value| !value.is_null())
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                WinxError::InvalidInput("verify_command must be a string".to_string())
            })
        })
        .transpose()?;
    let wait = arguments
        .get("verify_wait_for_seconds")
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value::<f32>(value.clone()).map_err(|error| {
                WinxError::InvalidInput(format!("invalid verify_wait_for_seconds: {error}"))
            })
        })
        .transpose()?;
    parse_verification_fields(command, wait)
}

fn parse_verification_fields(
    command: Option<String>,
    wait_for_seconds: Option<f32>,
) -> Result<Option<EditVerification>> {
    let command = command.map(|command| command.trim().to_string());
    match command {
        Some(command) if command.is_empty() => {
            Err(WinxError::InvalidInput("verify_command must not be empty".to_string()))
        }
        Some(_command)
            if wait_for_seconds
                .is_some_and(|wait| !wait.is_finite() || !(0.0..=60.0).contains(&wait)) =>
        {
            Err(WinxError::InvalidInput(
                "verify_wait_for_seconds must be between 0 and 60".to_string(),
            ))
        }
        Some(command) => Ok(Some(EditVerification { command, wait_for_seconds })),
        None if wait_for_seconds.is_some() => Err(WinxError::InvalidInput(
            "verify_wait_for_seconds requires verify_command".to_string(),
        )),
        None => Ok(None),
    }
}

fn deserialize<T: serde::de::DeserializeOwned>(value: Value, tool: &str) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|error| WinxError::InvalidInput(format!("Invalid {tool} parameters: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_wire_keeps_explicit_workspace_and_verification_fields() -> Result<()> {
        let normalized = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "apply",
                "files": [{
                    "file_path": "/workspace/file.rs",
                    "mode": "replace",
                    "content": "fn main() {}"
                }],
                "verify_command": " cargo check ",
                "verify_wait_for_seconds": 12.5,
                "thread_id": "thread",
                "workspace_root": "/workspace"
            }),
        )?;
        assert_eq!(normalized.workspace_root.as_deref(), Some("/workspace"));
        let verification = normalized.verification.ok_or_else(|| {
            WinxError::InvalidInput("expected normalized verification".to_string())
        })?;
        assert_eq!(verification.command, "cargo check");
        assert_eq!(verification.wait_for_seconds, Some(12.5));
        Ok(())
    }

    #[test]
    fn dark_wire_rejects_unknown_entry_fields_and_mixed_undo() {
        let unknown = serde_json::json!({
            "operation": "apply",
            "files": [{
                "file_path": "/workspace/file.rs",
                "mode": "replace",
                "content": "content",
                "percentage_to_change": 100
            }],
            "thread_id": "thread"
        });
        assert!(normalize_edit_call(EditSurface::EditFiles, unknown).is_err());

        let mixed = serde_json::json!({
            "operation": "apply",
            "files": [{
                "file_path": "/workspace/file.rs",
                "mode": "undo",
                "undo_id": "undo_123"
            }],
            "thread_id": "thread"
        });
        assert!(normalize_edit_call(EditSurface::EditFiles, mixed).is_err());
    }

    #[test]
    fn legacy_marker_heuristic_does_not_leak_into_explicit_modes() -> Result<()> {
        let content = "<<<<<<< SEARCH\nold\n=======\nnew\n>>>>>>> REPLACE\n";
        let legacy = normalize_edit_call(
            EditSurface::FileWriteOrEdit,
            serde_json::json!({
                "file_path": "/workspace/file.rs",
                "percentage_to_change": 100,
                "text_or_search_replace_blocks": content,
                "thread_id": "thread"
            }),
        )?;
        let explicit = normalize_edit_call(
            EditSurface::EditFiles,
            serde_json::json!({
                "operation": "apply",
                "files": [{
                    "file_path": "/workspace/file.rs",
                    "mode": "replace",
                    "content": content
                }],
                "thread_id": "thread"
            }),
        )?;
        assert_eq!(legacy.command.modes(), vec![EditMode::SearchReplace]);
        assert_eq!(explicit.command.modes(), vec![EditMode::Replace]);
        Ok(())
    }
}
