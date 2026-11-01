//! Optional Eulix-backed call graph navigation.
mod codec;
pub mod index;
mod ingest;
mod runtime;
#[cfg(test)]
mod runtime_tests;
mod snapshot;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{CodeMap, CodeMapOperation};
use crate::utils::path::resolve_in_workspace;

pub(crate) async fn handle_tool_call(
    state: &Arc<Mutex<Option<BashState>>>,
    args: &CodeMap,
    budget: usize,
) -> Result<(String, Value)> {
    let (cwd, workspace) = {
        let guard = state.lock().await;
        let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (state.cwd.clone(), state.workspace_root.clone())
    };
    let root = resolve_in_workspace(&args.path, &cwd, &workspace).map_err(|error| {
        WinxError::PathSecurityError { path: args.path.clone().into(), message: error.to_string() }
    })?;
    if !root.exists() {
        return Err(WinxError::FileNotFound { path: root });
    }
    // Workspace-wide indexes cannot be rebound to allowlisted external paths or helpers.
    if !root.starts_with(&workspace) || root.components().any(|part| part.as_os_str() == ".winx") {
        return bounded(
            args,
            &runtime::View {
                index: None,
                allowed: Arc::default(),
                status: "unavailable",
                error_code: Some("index_scope_unsupported".into()),
            },
            &[],
            false,
            budget,
        );
    }
    let view = runtime::global().request(&workspace).await;
    let Some(index) = view.index.clone() else { return bounded(args, &view, &[], false, budget) };
    let fallback_args = args.clone();
    let mut fallback_view = view.clone();
    let args = args.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let scope = root
            .strip_prefix(&workspace)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .map(crate::utils::path::display_relative);
        let max = if args.max_results == 0 { 100 } else { args.max_results.min(1_000) };
        let (mut hits, truncated) = index.query_scoped(
            &args.name,
            args.operation != CodeMapOperation::Callees,
            args.operation == CodeMapOperation::Impact,
            max,
            scope.as_deref(),
        );
        // Reapply current ignore and path policy before exposing persisted provenance.
        let allowed = &view.allowed;
        hits.retain(|hit| {
            source_allowed(&workspace, allowed, &hit.file)
                && source_allowed(&workspace, allowed, &hit.call_site_file)
        });
        bounded(&args, &view, &hits, truncated, budget)
    });
    if let Ok(result) = tokio::time::timeout(std::time::Duration::from_millis(75), worker).await {
        result
            .map_err(|_| WinxError::CommandExecutionError("CodeMap graph worker failed".into()))?
    } else {
        fallback_view.error_code = Some("query_budget_exceeded".into());
        bounded(&fallback_args, &fallback_view, &[], true, budget)
    }
}

fn source_allowed(
    workspace: &Path,
    allowed: &std::collections::HashSet<std::path::PathBuf>,
    relative: &str,
) -> bool {
    let path = workspace.join(relative);
    allowed.contains(&path)
        && path.canonicalize().is_ok_and(|canonical| canonical.starts_with(workspace))
}

fn bounded(
    args: &CodeMap,
    view: &runtime::View,
    hits: &[index::GraphHit],
    truncated: bool,
    budget: usize,
) -> Result<(String, Value)> {
    let mut low = 0;
    let mut high = hits.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let (text, data) = output(args, view, &hits[..mid], truncated || mid < hits.len());
        if text.len()
            + serde_json::to_vec(&data)
                .map_err(|error| WinxError::SerializationError(error.to_string()))?
                .len()
            <= budget.saturating_sub(2_048)
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let (text, mut data) = output(args, view, &hits[..low], truncated || low < hits.len());
    let size = |data: &Value| -> Result<usize> {
        serde_json::to_vec(data)
            .map(|bytes| text.len() + bytes.len())
            .map_err(|error| WinxError::SerializationError(error.to_string()))
    };
    if size(&data)? > budget.saturating_sub(2_048) {
        // The fallback already carries the exact name. Avoid repeating a heavily
        // escaped identifier in the smaller derived-helper response envelope.
        if let Some(fields) = data.as_object_mut() {
            fields.remove("name");
        }
    }
    if size(&data)? > budget.saturating_sub(2_048) {
        return Err(WinxError::ArgumentParseError("Call graph identifiers exceed the response budget; use a shorter symbol name with references.".into()));
    }
    Ok((text, data))
}

fn output(
    args: &CodeMap,
    view: &runtime::View,
    hits: &[index::GraphHit],
    truncated: bool,
) -> (String, Value) {
    let operation = match args.operation {
        CodeMapOperation::Callers => "callers",
        CodeMapOperation::Callees => "callees",
        CodeMapOperation::Impact => "impact",
        _ => "graph",
    };
    let mut text = format!("Eulix {operation}: index {}. Resolved static edges are approximate; verify canonical source.\n", view.status);
    for hit in hits {
        let _ = writeln!(
            text,
            "{}:{} {} (call {}:{}, depth {})",
            hit.file, hit.line, hit.name, hit.call_site_file, hit.call_site_line, hit.depth
        );
    }
    if hits.is_empty() {
        text.push_str(
            "No indexed results available; use CodeMap references for syntax occurrences.\n",
        );
    }
    let age = view.index.as_ref().map(|index| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(index.built_at)
    });
    let mut data = json!({"mode":operation,"name":args.name,"truncated":truncated,"graph_hits":hits,
        "index_status":view.status,"index_age_seconds":age,"index_error":view.error_code,
        "snapshot_hash":view.index.as_ref().map(|index| &index.snapshot),
        "parser_version":view.index.as_ref().map(|index| &index.parser_version),"payload_bytes":0});
    if hits.is_empty() || view.status != "fresh" {
        data["next_action"] = json!({"tool":"CodeMap","arguments":{"operation":"references","name":args.name,"path":args.path,"thread_id":args.thread_id},"instruction":"Use references for syntax occurrences while the optional call graph is unavailable or refreshing."});
    }
    (text, data)
}

#[cfg(test)]
mod budget_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn graph_payload_is_bounded_and_unavailable_fallback_is_actionable() {
        let args: CodeMap =
            serde_json::from_value(json!({"operation":"callers","name":"leaf"})).unwrap();
        let view = runtime::View {
            index: None,
            allowed: Arc::default(),
            status: "unavailable",
            error_code: Some("parser_unavailable".into()),
        };
        let (_, data) = bounded(&args, &view, &[], false, 12 * 1024).unwrap();
        assert_eq!(data["next_action"]["arguments"]["operation"], "references");
        let hit = index::GraphHit {
            id: "id".into(),
            name: "leaf".into(),
            file: "a.rs".into(),
            line: 1,
            end_line: 5,
            call_site_file: "b.rs".into(),
            call_site_line: 2,
            depth: 1,
        };
        let (text, data) = bounded(&args, &view, &vec![hit; 1000], false, 12 * 1024).unwrap();
        assert!(text.len() + serde_json::to_vec(&data).unwrap().len() < 12 * 1024);
        assert_eq!(data["truncated"], true);
    }
    #[test]
    fn unavailable_fallback_with_heavily_escaped_name_fits_helper_budget() {
        let args: CodeMap =
            serde_json::from_value(json!({"operation":"callers","name":"\u{1}".repeat(1024)}))
                .unwrap();
        let view = runtime::View {
            index: None,
            allowed: Arc::default(),
            status: "unavailable",
            error_code: Some("parser_unavailable".into()),
        };
        let (text, data) = bounded(&args, &view, &[], false, 12 * 1024).unwrap();
        assert!(text.len() + serde_json::to_vec(&data).unwrap().len() <= 12 * 1024 - 2_048);
        assert_eq!(data["next_action"]["arguments"]["name"], args.name);
    }
}
