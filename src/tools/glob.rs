//! Implementation of the `Glob` tool — gitignore-aware file discovery.
//!
//! Walks the workspace with the `ignore` engine (so `target`/`node_modules`/
//! `.gitignore`d paths are skipped inside a git repo), matches each
//! workspace-relative path against a glob, and ranks the hits with the embedded
//! path-probability model. Read-only and workspace-confined, so it works in
//! every mode — including the restricted ones where `find`/`ls` are blocked.

use std::fmt::Write as FmtWrite;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{Glob, GlobOutput};
use crate::utils::path::{glob_matches, resolve_in_workspace};
use crate::utils::path_prob::score_paths;
use crate::utils::repo::walk_workspace_files;

/// Cap on paths returned when the caller passes 0.
const DEFAULT_MAX_RESULTS: usize = 200;

/// Returns the human-readable text block and the structured (`GlobOutput`) JSON
/// the caller attaches to the MCP result's `structuredContent`.
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: Glob,
) -> Result<(String, serde_json::Value)> {
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };

    // Canonicalize once so workspace-relative stripping is reliable even when the
    // stored root kept symlink components (otherwise a failed strip_prefix would
    // leak an absolute path and break relative glob matching).
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);

    if args.pattern.trim().is_empty() {
        return Err(WinxError::ArgumentParseError("Glob pattern must not be empty.".to_string()));
    }

    let root = resolve_in_workspace(&args.path, &cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&args.path), message: e.to_string() }
    })?;

    let pattern = ::glob::Pattern::new(&args.pattern).map_err(|e| {
        WinxError::ArgumentParseError(format!("Invalid glob '{}': {e}", args.pattern))
    })?;

    let max_results = if args.max_results == 0 { DEFAULT_MAX_RESULTS } else { args.max_results };

    let mut matches: Vec<String> = walk_workspace_files(&root)
        .iter()
        .filter_map(|file| {
            let rel = file.strip_prefix(&workspace_root).unwrap_or(file);
            glob_matches(&pattern, rel).then(|| rel.to_string_lossy().to_string())
        })
        .collect();

    if matches.is_empty() {
        let out = format!("No files match {} under {}.", args.pattern, root.display());
        let structured =
            GlobOutput { pattern: args.pattern, total: 0, shown: 0, paths: Vec::new() };
        return Ok((out, crate::tools::structured_json(&structured)?));
    }

    let total = matches.len();
    matches.sort(); // deterministic order so equal-score ties resolve stably
    rank(&mut matches);

    let shown = total.min(max_results);
    let paths: Vec<String> = matches.into_iter().take(shown).collect();
    let mut out = format!("{total} file(s) match {} (showing {shown}, ranked):\n", args.pattern);
    for path in &paths {
        out.push_str(path);
        out.push('\n');
    }
    if total > shown {
        let _ =
            write!(out, "(...{} more; raise max_results or narrow the pattern.)", total - shown);
    }
    let structured = GlobOutput { pattern: args.pattern, total, shown, paths };
    Ok((out, crate::tools::structured_json(&structured)?))
}

/// Order best-first via the embedded path-probability model (higher log-prob
/// first); falls back to alphabetical when the model can't be loaded.
fn rank(paths: &mut Vec<String>) {
    if let Some(scores) = score_paths(paths) {
        let mut indexed: Vec<(String, f64)> = paths.drain(..).zip(scores).collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        *paths = indexed.into_iter().map(|(path, _)| path).collect();
    } else {
        paths.sort();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tempfile::TempDir;

    fn state_in(dir: &TempDir) -> Arc<Mutex<Option<BashState>>> {
        let mut bs = BashState::new();
        let root = dir.path().canonicalize().unwrap();
        bs.cwd = root.clone();
        bs.workspace_root = root;
        Arc::new(Mutex::new(Some(bs)))
    }

    fn args(pattern: &str) -> Glob {
        Glob {
            pattern: pattern.to_string(),
            path: String::new(),
            max_results: 0,
            thread_id: String::new(),
        }
    }

    #[tokio::test]
    async fn matches_extension_at_any_depth() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/sub/b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();
        let st = state_in(&dir);
        let (out, _) = handle_tool_call(&st, args("*.rs")).await.unwrap();
        assert!(out.contains("src/a.rs"));
        assert!(out.contains("src/sub/b.rs"));
        assert!(!out.contains("c.txt"));
    }

    #[tokio::test]
    async fn matches_nested_glob() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.ts"), "").unwrap();
        std::fs::write(dir.path().join("top.ts"), "").unwrap();
        let st = state_in(&dir);
        let (out, _) = handle_tool_call(&st, args("src/**/*.ts")).await.unwrap();
        assert!(out.contains("src/a.ts"));
        assert!(!out.contains("top.ts"));
    }

    #[tokio::test]
    async fn reports_no_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        let st = state_in(&dir);
        let (out, _) = handle_tool_call(&st, args("*.zzz")).await.unwrap();
        assert!(out.to_lowercase().contains("no files match"));
    }

    #[tokio::test]
    async fn single_star_does_not_cross_slash() {
        // Regression: `src/*.ts` must match only direct children of src/, not
        // recurse — `**` is the recursive form.
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/top.ts"), "").unwrap();
        std::fs::write(dir.path().join("src/deep/bottom.ts"), "").unwrap();
        let st = state_in(&dir);
        let (out, _) = handle_tool_call(&st, args("src/*.ts")).await.unwrap();
        assert!(out.contains("src/top.ts"));
        assert!(!out.contains("src/deep/bottom.ts"), "single * must not cross /");
    }

    #[tokio::test]
    async fn structured_output_reports_total_and_paths() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        let st = state_in(&dir);
        let (_, structured) = handle_tool_call(&st, args("*.rs")).await.unwrap();
        assert_eq!(structured["total"], 2);
        assert_eq!(structured["shown"], 2);
        assert_eq!(structured["paths"].as_array().unwrap().len(), 2);
    }
}
