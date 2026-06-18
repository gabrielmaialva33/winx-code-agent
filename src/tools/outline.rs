//! Implementation of the `Outline` tool — a tree-sitter symbol map.
//!
//! A file path returns that file's definitions (functions, types, methods, ...);
//! a directory (or empty = the whole workspace) returns a ranked, token-budgeted
//! repo symbol map. Read-only and workspace-confined, so it works in every mode.
//! Reuses the bundled tree-sitter grammars, the `ignore` walker, and the
//! path-probability ranker that already power the other read tools.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{Outline, OutlineFile, OutlineOutput, OutlineSymbol};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::resolve_in_workspace;
use crate::utils::path_prob::score_paths;
use crate::utils::repo::walk_workspace_files;
use crate::utils::symbols::{self, Symbol};

/// Cap on files in repo-map mode when the caller passes 0.
const DEFAULT_MAX_FILES: usize = 50;
/// Cap on symbols in single-file mode when the caller passes 0.
const DEFAULT_MAX_SYMBOLS: usize = 500;
/// Skip files larger than this when outlining (huge generated files are noise).
const MAX_OUTLINE_FILE_SIZE: u64 = 2_000_000;
/// Byte proxy (~4 bytes/token) for the rendered repo-map budget.
const OUTLINE_MAX_BYTES: usize = 24_000 * 4;
/// Stop after reading+parsing this many files in repo mode, so a tree of
/// definition-less supported files can't be fully scanned (a scan-budget cap).
const MAX_FILES_SCANNED: usize = 20_000;

type Configs = HashMap<String, Option<TagsConfiguration>>;

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: Outline,
) -> Result<(String, serde_json::Value)> {
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);
    let root = resolve_in_workspace(&args.path, &cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&args.path), message: e.to_string() }
    })?;

    let mut context = TagsContext::new();
    let mut configs: Configs = HashMap::new();

    if root.is_file() {
        outline_one(&root, &workspace_root, &args, &mut context, &mut configs)
    } else if root.is_dir() {
        outline_repo(&root, &workspace_root, &args, &mut context, &mut configs)
    } else {
        // Don't silently degrade a typo'd file path into a whole-workspace scan.
        Err(WinxError::FileAccessError {
            path: root.clone(),
            message: "path not found (or not a regular file/directory)".to_string(),
        })
    }
}

/// A `file`-mode result carrying only a status message (no symbols).
fn empty_file_outline(message: String) -> Result<(String, serde_json::Value)> {
    let structured = OutlineOutput {
        mode: "file".to_string(),
        files_shown: 0,
        files: Vec::new(),
        truncated: false,
    };
    Ok((message, crate::tools::structured_json(&structured)?))
}

/// Lowercase extension of `path` ("" if none).
fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase()
}

/// Workspace-relative display path.
fn rel_of(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root).unwrap_or(path).to_string_lossy().to_string()
}

/// Compile-once-per-language cached config lookup.
fn config_for<'a>(configs: &'a mut Configs, ext: &str) -> Option<&'a TagsConfiguration> {
    configs.entry(ext.to_string()).or_insert_with(|| symbols::config_for(ext)).as_ref()
}

fn render_symbols(out: &mut String, syms: &[Symbol]) {
    for s in syms {
        let _ = writeln!(out, "  {:>5}  {:<9} {}", s.line, s.kind, s.name);
    }
}

fn to_output(syms: Vec<Symbol>) -> Vec<OutlineSymbol> {
    syms.into_iter().map(|s| OutlineSymbol { name: s.name, kind: s.kind, line: s.line }).collect()
}

fn outline_one(
    file: &Path,
    workspace_root: &Path,
    args: &Outline,
    context: &mut TagsContext,
    configs: &mut Configs,
) -> Result<(String, serde_json::Value)> {
    let rel = rel_of(file, workspace_root);
    let max = if args.max_results == 0 { DEFAULT_MAX_SYMBOLS } else { args.max_results };
    let ext = ext_of(file);

    // Distinguish the real reasons we'd return no symbols instead of collapsing
    // them all into a misleading "no definitions" (no silent fallback).
    if !symbols::supports(&ext) {
        return empty_file_outline(format!(
            "No symbols in {rel}: unsupported language (extension `.{ext}`)."
        ));
    }
    let text = match read_file_to_string(file, MAX_OUTLINE_FILE_SIZE) {
        Ok(text) => text,
        Err(e) => return empty_file_outline(format!("Could not outline {rel}: {e}")),
    };
    let Some(config) = config_for(configs, &ext) else {
        return empty_file_outline(format!("No symbols in {rel}: no tags query for `.{ext}`."));
    };

    let mut syms = symbols::extract(context, config, &text);
    let total = syms.len();
    let truncated = total > max;
    if truncated {
        syms.truncate(max);
    }

    let mut out = String::new();
    if syms.is_empty() {
        return empty_file_outline(format!("No definitions found in {rel}."));
    }
    if truncated {
        let _ = writeln!(out, "{rel} ({} of {total} symbols):", syms.len());
    } else {
        let noun = if total == 1 { "symbol" } else { "symbols" };
        let _ = writeln!(out, "{rel} ({total} {noun}):");
    }
    render_symbols(&mut out, &syms);
    if truncated {
        let _ = write!(out, "(...{} more; raise max_results)", total - syms.len());
    }

    let structured = OutlineOutput {
        mode: "file".to_string(),
        files_shown: 1,
        files: vec![OutlineFile { file: rel, symbols: to_output(syms) }],
        truncated,
    };
    Ok((out, crate::tools::structured_json(&structured)?))
}

/// Collect supported files under `root`, ranked best-first by the path-prob model
/// (alphabetical fallback). Returns `(absolute, workspace-relative)` pairs.
fn ranked_supported_files(root: &Path, workspace_root: &Path) -> Vec<(PathBuf, String)> {
    let mut files: Vec<(PathBuf, String)> = walk_workspace_files(root)
        .into_iter()
        .filter(|abs| symbols::supports(&ext_of(abs)))
        .map(|abs| {
            let rel = rel_of(&abs, workspace_root);
            (abs, rel)
        })
        .collect();

    // Pre-sort alphabetically so equal-score ties resolve deterministically
    // (stable sort keeps this order).
    files.sort_by(|a, b| a.1.cmp(&b.1));
    // Score the names borrowed as &str — no per-path String clone (score_paths is
    // generic over AsRef<str>). Scope `rels` so its borrow of `files` ends before
    // we move `files` below.
    let ranking = {
        let rels: Vec<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
        score_paths(&rels)
    };
    if let Some(weights) = ranking {
        // Pair each file with its weight and sort by weight desc, MOVING the files
        // (no per-entry clone of the old reindex). Stable sort preserves the
        // alphabetical pre-sort for equal weights.
        let mut pairs: Vec<(f64, (PathBuf, String))> = weights.into_iter().zip(files).collect();
        pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        files = pairs.into_iter().map(|(_, f)| f).collect();
    }
    files
}

fn outline_repo(
    root: &Path,
    workspace_root: &Path,
    args: &Outline,
    context: &mut TagsContext,
    configs: &mut Configs,
) -> Result<(String, serde_json::Value)> {
    let max_files = if args.max_results == 0 { DEFAULT_MAX_FILES } else { args.max_results };

    let mut out = String::new();
    let mut out_files: Vec<OutlineFile> = Vec::new();
    let mut truncated = false;
    let mut scanned = 0usize;

    for (abs, rel) in ranked_supported_files(root, workspace_root) {
        if out_files.len() >= max_files
            || out.len() >= OUTLINE_MAX_BYTES
            || scanned >= MAX_FILES_SCANNED
        {
            truncated = true;
            break;
        }
        // read_file_to_string enforces the size cap and rejects non-UTF-8, so a
        // separate metadata stat is redundant — skip on any read error.
        let Ok(text) = read_file_to_string(&abs, MAX_OUTLINE_FILE_SIZE) else { continue };
        scanned += 1;
        let ext = ext_of(&abs);
        let mut syms = match config_for(configs, &ext) {
            Some(config) => symbols::extract(context, config, &text),
            None => continue,
        };
        if syms.is_empty() {
            continue;
        }
        // Per-file symbol cap: one definition-dense file (minified/generated)
        // must not blow the budget or the structured array.
        if syms.len() > DEFAULT_MAX_SYMBOLS {
            syms.truncate(DEFAULT_MAX_SYMBOLS);
            truncated = true;
        }
        let _ = writeln!(out, "{rel}");
        render_symbols(&mut out, &syms);
        out_files.push(OutlineFile { file: rel, symbols: to_output(syms) });
    }

    if out_files.is_empty() {
        out = format!("No code symbols found under {}.", root.display());
    } else if truncated {
        let _ = write!(out, "(...capped; narrow `path` or raise max_results)");
    }

    let files_shown = out_files.len();
    let structured =
        OutlineOutput { mode: "repo".to_string(), files_shown, files: out_files, truncated };
    Ok((out, crate::tools::structured_json(&structured)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::fmt::Write as _;
    use tempfile::TempDir;

    fn state_in(dir: &TempDir) -> Arc<Mutex<Option<BashState>>> {
        let mut bs = BashState::new();
        let root = dir.path().canonicalize().unwrap();
        bs.cwd = root.clone();
        bs.workspace_root = root;
        Arc::new(Mutex::new(Some(bs)))
    }

    fn args(path: &str) -> Outline {
        Outline { path: path.to_string(), max_results: 0, thread_id: String::new() }
    }

    #[tokio::test]
    async fn single_file_lists_symbols() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn alpha() {}\nstruct Beta;\n").unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("a.rs")).await.unwrap();
        assert!(out.contains("alpha"));
        assert!(out.contains("Beta"));
        assert_eq!(structured["mode"], "file");
        let syms = structured["files"][0]["symbols"].as_array().unwrap();
        assert!(syms.iter().any(|s| s["name"] == "alpha"));
    }

    #[tokio::test]
    async fn repo_map_ranks_files() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        std::fs::write(dir.path().join("src/util.rs"), "fn two() {}\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not code\n").unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("")).await.unwrap();
        assert_eq!(structured["mode"], "repo");
        assert_eq!(structured["files_shown"], 2); // only the 2 .rs files
        assert!(out.contains("one"));
        assert!(out.contains("two"));
        assert!(!out.contains("notes.txt"));
    }

    #[tokio::test]
    async fn no_symbols_message() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.json"), "{\"a\":1}\n").unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("data.json")).await.unwrap();
        assert!(out.to_lowercase().contains("no symbols"));
        assert_eq!(structured["files_shown"], 0);
    }

    #[tokio::test]
    async fn nonexistent_path_errors_instead_of_repo_scan() {
        // Regression: a typo'd file path must error, not silently become an empty
        // whole-workspace scan.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("real.rs"), "fn x() {}\n").unwrap();
        let st = state_in(&dir);
        assert!(handle_tool_call(&st, args("nope_typo.rs")).await.is_err());
    }

    #[tokio::test]
    async fn repo_per_file_cap_marks_truncated() {
        // Regression (B1): a definition-dense file must be clipped and reported
        // as truncated, not blow the budget while claiming completeness.
        let dir = TempDir::new().unwrap();
        let mut src = String::new();
        for i in 0..(DEFAULT_MAX_SYMBOLS + 100) {
            let _ = writeln!(src, "fn f{i}() {{}}");
        }
        std::fs::write(dir.path().join("big.rs"), src).unwrap();
        let st = state_in(&dir);
        let (_, structured) = handle_tool_call(&st, args("")).await.unwrap();
        let syms = structured["files"][0]["symbols"].as_array().unwrap();
        assert!(syms.len() <= DEFAULT_MAX_SYMBOLS, "got {}", syms.len());
        assert_eq!(structured["truncated"], true);
    }
}
