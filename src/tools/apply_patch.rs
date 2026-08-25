//! Revision-guarded, line-oriented file patches.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::tools::file_write_or_edit::{commit_edit, plan_revision_edit};
use crate::types::{normalize_thread_id, ApplyPatch, LinePatch};

const MAX_PATCHES: usize = 256;

#[derive(Debug)]
pub struct ApplyPatchOutcome {
    pub text: String,
    pub revision: String,
}

#[instrument(level = "info", skip(bash_state_arc, request))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    request: ApplyPatch,
) -> Result<ApplyPatchOutcome> {
    validate_revision(&request.expected_revision)?;
    validate_patch_count(&request.patches)?;

    let mut bash_state_guard = bash_state_arc.lock().await;
    {
        let state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let thread_id = normalize_thread_id(&request.thread_id);
        if thread_id != state.current_thread_id {
            return Err(WinxError::ThreadIdMismatch(thread_id));
        }
    }

    let mut state = bash_state_guard.take().ok_or(WinxError::BashStateNotInitialized)?;
    let recovery_state = state.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let required_ranges = required_read_ranges(&request.patches);
            let planned = plan_revision_edit(
                &state,
                &request.file_path,
                &request.expected_revision,
                &required_ranges,
                |content| apply_line_patches(content, &request.patches),
            )?;
            let revision = planned.new_revision();
            let text = commit_edit(&mut state, planned)?;
            Ok(ApplyPatchOutcome { text, revision })
        }))
        .unwrap_or_else(|_| {
            Err(WinxError::CommandExecutionError(
                "ApplyPatch panicked on the blocking worker".to_string(),
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
                "ApplyPatch blocking task failed: {error}"
            )))
        }
    }
}

fn validate_revision(revision: &str) -> Result<()> {
    let hash = revision.strip_prefix("sha256:").unwrap_or_default();
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WinxError::ParameterValidationError {
            field: "expected_revision".to_string(),
            message: "must be the exact sha256:<64 lowercase hex> token returned by ReadFiles"
                .to_string(),
        });
    }
    if hash.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(WinxError::ParameterValidationError {
            field: "expected_revision".to_string(),
            message: "revision tokens are case-sensitive; copy the ReadFiles value exactly"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_patch_count(patches: &[LinePatch]) -> Result<()> {
    if patches.is_empty() || patches.len() > MAX_PATCHES {
        return Err(WinxError::ParameterValidationError {
            field: "patches".to_string(),
            message: format!("must contain between 1 and {MAX_PATCHES} patches"),
        });
    }
    Ok(())
}

fn required_read_ranges(patches: &[LinePatch]) -> Vec<(usize, usize)> {
    patches
        .iter()
        .map(|patch| {
            if patch.delete_lines == 0 {
                (patch.start_line, patch.start_line)
            } else {
                (
                    patch.start_line,
                    patch.start_line.saturating_add(patch.delete_lines).saturating_sub(1),
                )
            }
        })
        .collect()
}

fn apply_line_patches(content: &str, patches: &[LinePatch]) -> Result<String> {
    let spans = line_spans(content);
    validate_patch_coordinates(patches, spans.len())?;
    let mut output = content.to_string();
    for patch in patches.iter().rev() {
        let start_index = patch.start_line - 1;
        let byte_start = spans.get(start_index).map_or(content.len(), |(start, _)| *start);
        let byte_end = if patch.delete_lines == 0 {
            byte_start
        } else {
            spans[start_index + patch.delete_lines - 1].1
        };
        output.replace_range(byte_start..byte_end, &patch.replacement);
    }
    Ok(output)
}

fn validate_patch_coordinates(patches: &[LinePatch], total_lines: usize) -> Result<()> {
    let mut previous_start = 0;
    let mut previous_end = 0;
    for (index, patch) in patches.iter().enumerate() {
        if patch.start_line == 0 || patch.start_line > total_lines.saturating_add(1) {
            return invalid_patch(index, format!("start_line must be in 1..={}", total_lines + 1));
        }
        let start = patch.start_line - 1;
        let end = start.checked_add(patch.delete_lines).ok_or_else(|| {
            WinxError::ParameterValidationError {
                field: format!("patches[{index}].delete_lines"),
                message: "line range overflow".to_string(),
            }
        })?;
        if end > total_lines {
            return invalid_patch(index, "delete_lines extends past the original file".to_string());
        }
        if index > 0 && (start < previous_end || start == previous_start) {
            return invalid_patch(
                index,
                "patches must be strictly ordered and non-overlapping in the original revision"
                    .to_string(),
            );
        }
        previous_start = start;
        previous_end = end;
    }
    Ok(())
}

fn invalid_patch<T>(index: usize, message: String) -> Result<T> {
    Err(WinxError::ParameterValidationError { field: format!("patches[{index}]"), message })
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use tokio::sync::Mutex;

    use crate::errors::ReadRequirement;
    use crate::types::{Modes, ReadFiles};

    fn patch(start_line: usize, delete_lines: usize, replacement: &str) -> LinePatch {
        LinePatch { start_line, delete_lines, replacement: replacement.to_string() }
    }

    #[test]
    fn applies_multiple_original_revision_patches_from_the_end() -> Result<()> {
        let output = apply_line_patches(
            "one\ntwo\nthree\nfour\n",
            &[patch(2, 1, "TWO\n"), patch(4, 0, "before four\n")],
        )?;
        assert_eq!(output, "one\nTWO\nthree\nbefore four\nfour\n");
        Ok(())
    }

    #[test]
    fn supports_append_and_empty_file_insert() -> Result<()> {
        assert_eq!(apply_line_patches("one\n", &[patch(2, 0, "two\n")])?, "one\ntwo\n");
        assert_eq!(apply_line_patches("", &[patch(1, 0, "first\n")])?, "first\n");
        Ok(())
    }

    #[test]
    fn rejects_overlapping_or_out_of_order_coordinates() {
        assert!(apply_line_patches("one\ntwo\nthree\n", &[patch(2, 2, "x\n"), patch(3, 1, "y\n")])
            .is_err());
        assert!(apply_line_patches("one\n", &[patch(3, 0, "x")]).is_err());
    }

    fn state_for(root: &std::path::Path, thread_id: &str) -> Arc<Mutex<Option<BashState>>> {
        let mut state = BashState::new();
        state.initialized = true;
        state.cwd = root.to_path_buf();
        state.workspace_root = root.to_path_buf();
        state.current_thread_id = normalize_thread_id(thread_id);
        state.mode = Modes::Wcgw;
        Arc::new(Mutex::new(Some(state)))
    }

    #[tokio::test]
    async fn revision_patch_uses_only_visible_lines_and_stale_retry_is_safe() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("sample.txt");
        std::fs::write(&path, "one\ntwo\nthree\n")?;
        let state = state_for(temp.path(), "patch-test");
        let read = crate::tools::read_files::handle_tool_call_detailed(
            &state,
            ReadFiles {
                file_paths: vec![path.to_string_lossy().into_owned()],
                thread_id: "patch-test".to_string(),
                start_line_nums: vec![Some(2)],
                end_line_nums: vec![Some(2)],
            },
        )
        .await?;
        let revision = read.files[0].revision.clone();

        let outcome = handle_tool_call(
            &state,
            ApplyPatch {
                file_path: path.to_string_lossy().into_owned(),
                expected_revision: revision.clone(),
                patches: vec![patch(2, 1, "TWO\n")],
                thread_id: "patch-test".to_string(),
            },
        )
        .await?;
        assert_ne!(outcome.revision, revision);
        assert_eq!(std::fs::read_to_string(&path)?, "one\nTWO\nthree\n");
        assert!(state
            .lock()
            .await
            .as_ref()
            .is_some_and(|state| state.whitelist_for_overwrite.is_empty()));

        let stale_result = handle_tool_call(
            &state,
            ApplyPatch {
                file_path: path.to_string_lossy().into_owned(),
                expected_revision: revision,
                patches: vec![patch(2, 1, "WRONG\n")],
                thread_id: "patch-test".to_string(),
            },
        )
        .await;
        assert!(matches!(stale_result, Err(WinxError::FileRevisionMismatch { .. })));
        assert_eq!(std::fs::read_to_string(&path)?, "one\nTWO\nthree\n");
        Ok(())
    }

    #[tokio::test]
    async fn patch_cannot_touch_an_unread_line() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("sample.txt");
        std::fs::write(&path, "one\ntwo\nthree\n")?;
        let state = state_for(temp.path(), "coverage-test");
        let read = crate::tools::read_files::handle_tool_call_detailed(
            &state,
            ReadFiles {
                file_paths: vec![path.to_string_lossy().into_owned()],
                thread_id: "coverage-test".to_string(),
                start_line_nums: vec![Some(2)],
                end_line_nums: vec![Some(2)],
            },
        )
        .await?;
        let patch_result = handle_tool_call(
            &state,
            ApplyPatch {
                file_path: path.to_string_lossy().into_owned(),
                expected_revision: read.files[0].revision.clone(),
                patches: vec![patch(3, 1, "THREE\n")],
                thread_id: "coverage-test".to_string(),
            },
        )
        .await;
        assert!(matches!(
            patch_result,
            Err(WinxError::FileReadRequired { reason: ReadRequirement::InsufficientCoverage, .. })
        ));
        assert_eq!(std::fs::read_to_string(path)?, "one\ntwo\nthree\n");
        Ok(())
    }
}
