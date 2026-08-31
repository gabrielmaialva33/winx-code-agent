//! Typed recovery policy for domain failures.
//!
//! Keep recovery decisions independent from human-facing error wording. Tool
//! messages are free to improve without silently changing the state machine an
//! MCP client observes.

use std::path::Path;

use serde_json::{json, Value};

use super::outcomes::{RequiredRead, ToolNextAction, ToolResultStatus};
use crate::errors::WinxError;
use crate::tool_registry::ToolKind;

pub(super) struct ErrorRecoveryPlan {
    pub status: ToolResultStatus,
    pub error_code: &'static str,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
    pub next_action: Option<ToolNextAction>,
    pub required_reads: Vec<RequiredRead>,
}

impl ErrorRecoveryPlan {
    fn terminal(status: ToolResultStatus, error_code: &'static str) -> Self {
        Self {
            status,
            error_code,
            retryable: false,
            retry_after_ms: None,
            next_action: None,
            required_reads: Vec::new(),
        }
    }

    fn read(
        status: ToolResultStatus,
        error_code: &'static str,
        path: &Path,
        ranges: Vec<String>,
        arguments: Option<&Value>,
    ) -> Self {
        let path = path.to_string_lossy().into_owned();
        let next_action = read_action(arguments, &path, &ranges, ReadActionKind::Refresh);
        Self {
            status,
            error_code,
            retryable: true,
            retry_after_ms: None,
            next_action,
            required_reads: vec![RequiredRead { path, ranges }],
        }
    }

    fn search_conflict(
        status: ToolResultStatus,
        error_code: &'static str,
        path: &Path,
        prefer_line_patch: bool,
        arguments: Option<&Value>,
    ) -> Self {
        let path = path.to_string_lossy().into_owned();
        let ranges = Vec::new();
        let action_kind = if prefer_line_patch {
            ReadActionKind::SearchConflictLinePatch
        } else {
            ReadActionKind::SearchConflictText
        };
        let next_action = read_action(arguments, &path, &ranges, action_kind);
        Self {
            status,
            error_code,
            retryable: true,
            retry_after_ms: None,
            next_action,
            required_reads: vec![RequiredRead { path, ranges }],
        }
    }
}

#[derive(Clone, Copy)]
enum ReadActionKind {
    Refresh,
    SearchConflictLinePatch,
    SearchConflictText,
}

/// Map a domain error to the exact recovery transition exposed over MCP.
///
/// `MultiFilePlanError` supplies the request-resolved path for inner failures
/// such as SEARCH conflicts, whose source variant intentionally has no path.
pub(super) fn classify(
    tool: &str,
    error: &WinxError,
    arguments: Option<&Value>,
) -> Option<ErrorRecoveryPlan> {
    classify_with_path(tool, error, arguments, None)
}

fn classify_with_path(
    tool: &str,
    error: &WinxError,
    arguments: Option<&Value>,
    contextual_path: Option<&Path>,
) -> Option<ErrorRecoveryPlan> {
    match error {
        WinxError::MultiFilePlanError { path, source, .. }
        | WinxError::EditContextError { path, source }
        | WinxError::IndexedEditError { path, source, .. } => {
            classify_with_path(tool, source, arguments, Some(path))
        }
        WinxError::FileReadRequired { path, ranges, .. } => Some(ErrorRecoveryPlan::read(
            ToolResultStatus::NeedsRead,
            "read_required",
            contextual_path.unwrap_or(path),
            ranges.clone(),
            arguments,
        )),
        WinxError::FileChangedAfterEdit { path, .. } => Some(ErrorRecoveryPlan::read(
            ToolResultStatus::Conflict,
            "file_changed_after_edit",
            contextual_path.unwrap_or(path),
            Vec::new(),
            arguments,
        )),
        WinxError::ConcurrentFileModification { path, .. } => {
            let mut plan = ErrorRecoveryPlan::read(
                ToolResultStatus::Conflict,
                "file_changed_during_read",
                contextual_path.unwrap_or(path),
                Vec::new(),
                arguments,
            );
            plan.retry_after_ms = Some(50);
            Some(plan)
        }
        WinxError::FileRevisionMismatch { path, .. } => Some(ErrorRecoveryPlan::read(
            ToolResultStatus::NeedsRead,
            "revision_mismatch",
            contextual_path.unwrap_or(path),
            Vec::new(),
            arguments,
        )),
        WinxError::SearchBlockNotFound(_) | WinxError::SearchBlockAmbiguous { .. } => {
            let path = contextual_path.map(Path::to_path_buf)?;
            Some(ErrorRecoveryPlan::search_conflict(
                ToolResultStatus::Conflict,
                if matches!(error, WinxError::SearchBlockAmbiguous { .. }) {
                    "search_block_ambiguous"
                } else {
                    "search_block_not_found"
                },
                &path,
                tool == ToolKind::EditFiles.as_str(),
                arguments,
            ))
        }
        WinxError::FileNotFound { .. } => {
            Some(ErrorRecoveryPlan::terminal(ToolResultStatus::NotFound, "file_not_found"))
        }
        WinxError::UndoCheckpointNotFound { .. } => Some(ErrorRecoveryPlan::terminal(
            ToolResultStatus::NotFound,
            "undo_checkpoint_not_found",
        )),
        WinxError::UndoExpired { .. } => {
            Some(ErrorRecoveryPlan::terminal(ToolResultStatus::NotFound, "undo_expired"))
        }
        WinxError::UndoOutOfOrder { .. } => {
            Some(ErrorRecoveryPlan::terminal(ToolResultStatus::Conflict, "undo_not_latest"))
        }
        WinxError::FileOperationDenied { .. } => {
            Some(ErrorRecoveryPlan::terminal(ToolResultStatus::Denied, "file_operation_denied"))
        }
        _ => None,
    }
}

fn read_action(
    arguments: Option<&Value>,
    path: &str,
    ranges: &[String],
    kind: ReadActionKind,
) -> Option<ToolNextAction> {
    if path.is_empty() {
        return None;
    }
    let file_paths = if ranges.is_empty() {
        vec![Value::String(path.to_string())]
    } else {
        ranges.iter().map(|range| Value::String(format!("{path}:{range}"))).collect()
    };
    let mut value = json!({"file_paths": file_paths});
    copy_session_binding(arguments, &mut value);
    let instruction = if matches!(kind, ReadActionKind::SearchConflictLinePatch) {
        "Call ReadFiles exactly as specified; Bash/cat does not refresh the edit guard. Then make \
         one corrected EditFiles retry with mode=line_patch, copying the returned revision and \
         visible line coordinates. Do not retry SEARCH or fall back to shell, sed, or Python for \
         the ordinary file edit."
    } else if matches!(kind, ReadActionKind::SearchConflictText) {
        "Call ReadFiles exactly as specified before another edit attempt. Bash/cat does not \
         refresh the edit guard. Rebuild SEARCH from the returned current text, then make one \
         corrected retry; never resend the failed edit unchanged."
    } else {
        "Call ReadFiles exactly as specified before another edit attempt. Bash/cat does not \
         refresh the edit guard. Rebuild the intended edit from the returned current text and \
         revision; never resend stale or failed input unchanged."
    };
    Some(ToolNextAction {
        tool: ToolKind::ReadFiles.as_str().to_string(),
        instruction: instruction.to_string(),
        arguments: Some(value),
    })
}

fn copy_session_binding(arguments: Option<&Value>, target: &mut Value) {
    for key in ["thread_id", "workspace_root"] {
        if let Some(value) = string_argument(arguments, key) {
            target[key] = Value::String(value);
        }
    }
}

fn string_argument(arguments: Option<&Value>, key: &str) -> Option<String> {
    arguments?.get(key)?.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ReadRequirement;

    #[test]
    fn recovery_does_not_depend_on_human_message_text() -> crate::errors::Result<()> {
        let error = WinxError::FileReadRequired {
            path: "/workspace/src/lib.rs".into(),
            reason: ReadRequirement::InsufficientCoverage,
            ranges: vec!["20-40".into(), "90-".into()],
            message: "wording may change freely".into(),
        };
        let arguments = json!({
            "thread_id": "thread",
            "workspace_root": "/workspace"
        });
        let plan = classify("ReadFiles", &error, Some(&arguments)).ok_or_else(|| {
            WinxError::ParseError("typed recovery plan was not produced".to_string())
        })?;
        assert_eq!(plan.status, ToolResultStatus::NeedsRead);
        assert_eq!(plan.error_code, "read_required");
        assert_eq!(plan.required_reads[0].ranges, ["20-40", "90-"]);
        let action = plan.next_action.ok_or_else(|| {
            WinxError::ParseError("recovery plan omitted next action".to_string())
        })?;
        let action_arguments = action.arguments.ok_or_else(|| {
            WinxError::ParseError("recovery action omitted arguments".to_string())
        })?;
        assert_eq!(
            action_arguments["file_paths"],
            json!(["/workspace/src/lib.rs:20-40", "/workspace/src/lib.rs:90-"])
        );
        Ok(())
    }

    #[test]
    fn multi_file_context_supplies_path_for_search_conflict() -> crate::errors::Result<()> {
        let error = WinxError::MultiFilePlanError {
            index: 2,
            path: "/workspace/src/main.rs".into(),
            source: Box::new(WinxError::SearchBlockNotFound("missing".into())),
        };
        let plan = classify("MultiFileEdit", &error, None).ok_or_else(|| {
            WinxError::ParseError("typed recovery plan was not produced".to_string())
        })?;
        assert_eq!(plan.error_code, "search_block_not_found");
        assert_eq!(plan.required_reads[0].path, "/workspace/src/main.rs");
        Ok(())
    }

    #[test]
    fn transparent_single_edit_context_supplies_path_without_raw_wire_introspection(
    ) -> crate::errors::Result<()> {
        let error = WinxError::EditContextError {
            path: "/workspace/src/lib.rs".into(),
            source: Box::new(WinxError::SearchBlockNotFound("missing".into())),
        };
        let plan = classify(
            "FileWriteOrEdit",
            &error,
            Some(&json!({"thread_id": "thread", "workspace_root": "/workspace"})),
        )
        .ok_or_else(|| WinxError::ParseError("typed recovery plan was not produced".to_string()))?;
        assert_eq!(plan.error_code, "search_block_not_found");
        assert_eq!(plan.required_reads[0].path, "/workspace/src/lib.rs");
        Ok(())
    }

    #[test]
    fn unified_search_conflict_recovers_through_revision_bound_line_patch(
    ) -> crate::errors::Result<()> {
        let error = WinxError::IndexedEditError {
            index: 1,
            path: "/workspace/src/lib.rs".into(),
            mode: "search_replace".to_string(),
            source: Box::new(WinxError::SearchBlockNotFound("stale".to_string())),
        };
        let arguments = json!({
            "files": [{
                "file_path": "/workspace/src/lib.rs",
                "mode": "search_replace",
                "content": "stale"
            }],
            "thread_id": "thread",
            "workspace_root": "/workspace"
        });
        let plan = classify("EditFiles", &error, Some(&arguments)).ok_or_else(|| {
            WinxError::ParseError("typed recovery plan was not produced".to_string())
        })?;
        let action = plan.next_action.ok_or_else(|| {
            WinxError::ParseError("typed recovery plan omitted next action".to_string())
        })?;
        assert!(action.instruction.contains("mode=line_patch"));
        assert!(action.instruction.contains("returned revision"));
        assert!(action.instruction.contains("Do not retry SEARCH"));
        assert!(!action.instruction.contains("Rebuild SEARCH"));
        Ok(())
    }

    #[test]
    fn missing_receipt_bound_undo_is_nonretryable_and_expired() -> crate::errors::Result<()> {
        let error = WinxError::UndoExpired {
            path: "/workspace/src/lib.rs".into(),
            undo_id: "undo_missing".to_string(),
        };
        let plan = classify("EditFiles", &error, None)
            .ok_or_else(|| WinxError::ParseError("typed undo plan missing".to_string()))?;
        assert_eq!(plan.status, ToolResultStatus::NotFound);
        assert_eq!(plan.error_code, "undo_expired");
        assert!(!plan.retryable);
        assert!(plan.next_action.is_none());
        Ok(())
    }

    #[test]
    fn generic_io_wording_is_not_promoted_to_a_recovery_transition() {
        let error = WinxError::FileAccessError {
            path: "/workspace/file".into(),
            message: "hasn't been read but this is only an opaque OS message".into(),
        };
        assert!(classify("ReadFiles", &error, None).is_none());
    }
}
