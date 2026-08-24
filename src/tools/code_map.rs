//! Implementation of the `CodeMap` tool: tree-sitter code navigation.
//!
//! One tool with two operations, consolidating what used to be the separate
//! `Outline` and `FindReferences` tools (to keep the MCP surface small). It is a
//! thin dispatcher: it builds the corresponding internal request and delegates to
//! the unchanged `outline` / `references` implementations, so all the tree-sitter
//! and ranking logic is reused as-is.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{CodeMap, CodeMapOperation, FindReferences, Outline};
use crate::utils::agent_temp::{AgentTempInfo, DerivedCodeMapPermit};
use crate::utils::path::{expand_user, resolve_in_workspace};

/// Reserve enough room for the non-canonical notice and budget counters added
/// after the syntax-navigation implementation builds its bounded payload.
const DERIVED_SCOPE_METADATA_RESERVE: usize = 512;

#[derive(Clone, Copy, Debug)]
enum SourceScope {
    Canonical,
    Derived(DerivedCodeMapPermit),
}

impl SourceScope {
    const fn payload_max_bytes(self) -> usize {
        match self {
            Self::Canonical => crate::tools::outline::CODE_MAP_PAYLOAD_MAX_BYTES,
            Self::Derived(_) => {
                crate::utils::agent_temp::MAX_DERIVED_CODE_MAP_PAYLOAD_BYTES
                    - DERIVED_SCOPE_METADATA_RESERVE
            }
        }
    }
}

#[instrument(level = "info", skip(bash_state_arc, code_map))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    code_map: CodeMap,
) -> Result<(String, serde_json::Value)> {
    if code_map.operation == CodeMapOperation::References && code_map.name.trim().is_empty() {
        return Err(WinxError::ArgumentParseError(
            "CodeMap operation 'references' requires a non-empty 'name' (the symbol to find)."
                .to_string(),
        ));
    }
    let scope = reserve_source_scope(bash_state_arc, &code_map.path).await?;
    let payload_max_bytes = scope.payload_max_bytes();
    let result = match code_map.operation {
        CodeMapOperation::Outline => {
            let outline = Outline {
                path: code_map.path,
                max_results: code_map.max_results,
                query: code_map.query,
                cursor: code_map.cursor,
                thread_id: code_map.thread_id,
            };
            crate::tools::outline::handle_tool_call_with_budget(
                bash_state_arc,
                outline,
                payload_max_bytes,
            )
            .await
        }
        CodeMapOperation::References => {
            let find = FindReferences {
                name: code_map.name,
                path: code_map.path,
                max_results: code_map.max_results,
                thread_id: code_map.thread_id,
            };
            crate::tools::references::handle_tool_call_with_budget(
                bash_state_arc,
                find,
                payload_max_bytes,
            )
            .await
        }
    }?;
    decorate_source_scope(result, scope)
}

async fn reserve_source_scope(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    raw_path: &str,
) -> Result<SourceScope> {
    let mut guard = bash_state_arc.lock().await;
    let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
    let expanded = expand_user(raw_path);
    let requested = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        state.cwd.join(&expanded)
    };
    let resolved =
        resolve_in_workspace(raw_path, &state.cwd, &state.workspace_root).map_err(|error| {
            WinxError::PathSecurityError {
                path: PathBuf::from(raw_path),
                message: error.to_string(),
            }
        })?;
    if !crate::utils::agent_temp::validate_code_map_target(
        &state.workspace_root,
        &state.current_thread_id,
        &requested,
        &resolved,
    )? {
        return Ok(SourceScope::Canonical);
    }

    let info =
        crate::utils::agent_temp::session_info(&state.workspace_root, &state.current_thread_id);
    ensure_existing_helper_file(&resolved, &info)?;
    let permit = crate::utils::agent_temp::reserve_derived_code_map(
        &mut state.derived_code_map_usage,
        &resolved,
        &info,
    )?;
    Ok(SourceScope::Derived(permit))
}

fn ensure_existing_helper_file(path: &Path, info: &AgentTempInfo) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    let message = if path.is_dir() {
        "CodeMap over a temporary directory is not allowed; map one existing, reusable helper \
         file explicitly"
    } else {
        "temporary helpers must exist before CodeMap is called; reuse an existing stable helper \
         instead of racing creation/deletion or guessing a new carrier name"
    };
    Err(WinxError::TemporaryArtifactPolicy {
        path: path.to_path_buf(),
        temporary_artifact_dir: info.directory.clone(),
        message: message.to_string(),
    })
}

fn decorate_source_scope(
    (mut text, mut structured): (String, Value),
    scope: SourceScope,
) -> Result<(String, Value)> {
    let fields = structured.as_object_mut().ok_or_else(|| {
        WinxError::SerializationError("CodeMap structured result must be an object".to_string())
    })?;
    match scope {
        SourceScope::Canonical => {
            fields.insert("source_kind".to_string(), Value::String("canonical_source".to_string()));
            fields.insert("canonical".to_string(), Value::Bool(true));
        }
        SourceScope::Derived(permit) => {
            fields.insert("source_kind".to_string(), Value::String("derived_helper".to_string()));
            fields.insert("canonical".to_string(), Value::Bool(false));
            fields.insert(
                "temporary_helper_budget".to_string(),
                json!({
                    "calls_used": permit.calls_used,
                    "calls_limit": permit.calls_limit,
                    "unique_files_used": permit.unique_files_used,
                    "unique_files_limit": permit.unique_files_limit,
                }),
            );
            text = format!(
                "Non-canonical temporary helper; reuse this file and do not create another \
                 carrier for the same task (helper maps: {}/{}, unique files: {}/{}).\n{text}",
                permit.calls_used,
                permit.calls_limit,
                permit.unique_files_used,
                permit.unique_files_limit,
            );
        }
    }
    refresh_payload_bytes(&text, &mut structured)?;
    Ok((text, structured))
}

fn refresh_payload_bytes(text: &str, structured: &mut Value) -> Result<()> {
    if structured.get("payload_bytes").is_none() {
        return Ok(());
    }
    for _ in 0..3 {
        let serialized = serde_json::to_vec(structured)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let measured = text.len().saturating_add(serialized.len());
        if structured["payload_bytes"].as_u64() == u64::try_from(measured).ok() {
            break;
        }
        structured["payload_bytes"] = json!(measured);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fmt::Write as _;

    use tempfile::TempDir;

    use super::*;

    fn state_in(dir: &TempDir, thread_id: &str) -> Arc<Mutex<Option<BashState>>> {
        let mut state = BashState::new();
        let root = dir.path().canonicalize().unwrap();
        state.cwd = root.clone();
        state.workspace_root = root;
        state.current_thread_id = thread_id.to_string();
        state.initialized = true;
        Arc::new(Mutex::new(Some(state)))
    }

    fn outline(path: impl Into<String>) -> CodeMap {
        CodeMap {
            operation: CodeMapOperation::Outline,
            path: path.into(),
            name: String::new(),
            max_results: 0,
            query: String::new(),
            cursor: String::new(),
            thread_id: "active".to_string(),
        }
    }

    #[tokio::test]
    async fn canonical_and_derived_results_are_explicitly_distinguished() {
        let workspace = TempDir::new().unwrap();
        std::fs::write(workspace.path().join("main.py"), "def canonical():\n    pass\n").unwrap();
        let helper = crate::utils::agent_temp::session_info(workspace.path(), "active")
            .directory
            .join("review.py");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, "def derived():\n    pass\n").unwrap();
        let state = state_in(&workspace, "active");

        let (_, canonical) = handle_tool_call(&state, outline("main.py")).await.unwrap();
        assert_eq!(canonical["source_kind"], "canonical_source");
        assert_eq!(canonical["canonical"], true);
        assert!(canonical.get("temporary_helper_budget").is_none());

        let relative = helper.strip_prefix(workspace.path().canonicalize().unwrap()).unwrap();
        let (text, derived) =
            handle_tool_call(&state, outline(relative.to_string_lossy())).await.unwrap();
        assert!(text.starts_with("Non-canonical temporary helper"), "{text}");
        assert_eq!(derived["source_kind"], "derived_helper");
        assert_eq!(derived["canonical"], false);
        assert_eq!(derived["temporary_helper_budget"]["calls_used"], 1);
        assert_eq!(derived["temporary_helper_budget"]["unique_files_used"], 1);
    }

    #[tokio::test]
    async fn derived_helper_response_has_the_smaller_end_to_end_budget() {
        let workspace = TempDir::new().unwrap();
        let helper = crate::utils::agent_temp::session_info(workspace.path(), "active")
            .directory
            .join("dense.py");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        let mut source = String::new();
        for index in 0..600 {
            let _ = writeln!(source, "def derived_{index}():\n    pass");
        }
        std::fs::write(&helper, source).unwrap();
        let state = state_in(&workspace, "active");
        let relative = helper.strip_prefix(workspace.path().canonicalize().unwrap()).unwrap();

        let (text, structured) =
            handle_tool_call(&state, outline(relative.to_string_lossy())).await.unwrap();
        let total = text.len() + serde_json::to_vec(&structured).unwrap().len();
        assert!(
            total <= crate::utils::agent_temp::MAX_DERIVED_CODE_MAP_PAYLOAD_BYTES,
            "derived payload used {total} bytes: {structured}"
        );
        assert_eq!(structured["truncated"], true);
    }

    #[tokio::test]
    async fn helper_directory_and_unique_carrier_churn_are_rejected() {
        let workspace = TempDir::new().unwrap();
        std::fs::write(workspace.path().join("main.py"), "def canonical():\n    pass\n").unwrap();
        let info = crate::utils::agent_temp::session_info(workspace.path(), "active");
        std::fs::create_dir_all(&info.directory).unwrap();
        let state = state_in(&workspace, "active");
        let directory =
            info.directory.strip_prefix(workspace.path().canonicalize().unwrap()).unwrap();
        let directory_error =
            handle_tool_call(&state, outline(directory.to_string_lossy())).await.unwrap_err();
        assert!(directory_error.to_string().contains("one existing"), "{directory_error}");

        for index in 0..crate::utils::agent_temp::MAX_DERIVED_CODE_MAP_UNIQUE_FILES {
            let helper = info.directory.join(format!("carrier-{index}.py"));
            std::fs::write(&helper, format!("def helper_{index}():\n    pass\n")).unwrap();
            let relative = helper.strip_prefix(workspace.path().canonicalize().unwrap()).unwrap();
            handle_tool_call(&state, outline(relative.to_string_lossy())).await.unwrap();
        }
        let excess = info.directory.join("carrier-excess.py");
        std::fs::write(&excess, "def excess():\n    pass\n").unwrap();
        let relative = excess.strip_prefix(workspace.path().canonicalize().unwrap()).unwrap();
        let error =
            handle_tool_call(&state, outline(relative.to_string_lossy())).await.unwrap_err();
        assert!(matches!(error, WinxError::DerivedCodeMapBudget { .. }));

        let (_, canonical) = handle_tool_call(&state, outline("main.py")).await.unwrap();
        assert_eq!(canonical["source_kind"], "canonical_source");
    }
}
