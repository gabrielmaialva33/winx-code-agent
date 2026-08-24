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
use std::time::UNIX_EPOCH;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{CodeMapFallback, Outline, OutlineFile, OutlineOutput, OutlineSymbol};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::resolve_in_workspace;
use crate::utils::path_prob::score_paths;
use crate::utils::repo::walk_workspace_files;
use crate::utils::symbols::{self, Symbol};
use crate::utils::workspace_stats::active_files_for_context;

/// Cap on files in repo-map mode when the caller passes 0.
const DEFAULT_MAX_FILES: usize = 20;
/// Cap on symbols in single-file mode when the caller passes 0.
const DEFAULT_MAX_SYMBOLS: usize = 250;
/// Preserve breadth in a repo map: dense files can be outlined directly later.
const REPO_MAX_SYMBOLS_PER_FILE: usize = 64;
/// Skip files larger than this when outlining (huge generated files are noise).
const MAX_OUTLINE_FILE_SIZE: u64 = 2_000_000;
/// Combined text + structured navigation payload. The MCP envelope and JSON
/// escaping still fit comfortably under a 64 KiB response in ordinary source.
const CODE_MAP_PAYLOAD_MAX_BYTES: usize = 44 * 1024;
/// Leave room for cursor, snapshot, counters, and the truncation notice.
const CODE_MAP_PAYLOAD_FIXED_RESERVE: usize = 1_024;
/// Stop after reading+parsing this many files in repo mode, so a tree of
/// definition-less supported files can't be fully scanned (a scan-budget cap).
const MAX_FILES_SCANNED: usize = 20_000;
const MAX_CURSOR_BYTES: usize = 1_024;

type Configs = HashMap<String, Option<TagsConfiguration>>;

#[derive(Debug, Deserialize, Serialize)]
struct RepoCursor {
    version: u8,
    offset: usize,
    snapshot: String,
}

fn navigation_payload_bytes(text: &str, files: &[OutlineFile]) -> usize {
    text.len()
        .saturating_add(serde_json::to_vec(files).map_or(usize::MAX, |serialized| serialized.len()))
}

fn finalize_output(text: &str, mut output: OutlineOutput) -> Result<serde_json::Value> {
    // `payload_bytes` describes the payload that contains the field itself. A
    // few iterations make the decimal digit count converge without estimation.
    for _ in 0..3 {
        let serialized = serde_json::to_vec(&output)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let measured = text.len().saturating_add(serialized.len());
        if measured == output.payload_bytes {
            break;
        }
        output.payload_bytes = measured;
    }
    crate::tools::structured_json(&output)
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = query
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn contextual_path_score(rel: &str, terms: &[String], active_paths: &[String]) -> usize {
    let lower = rel.to_ascii_lowercase();
    let file_name = Path::new(&lower).file_name().and_then(|name| name.to_str()).unwrap_or("");
    let active = active_paths
        .iter()
        .position(|path| rel == path || rel.ends_with(path))
        .map_or(0, |index| 1_000usize.saturating_sub(index * 50));
    active.saturating_add(terms.iter().fold(0usize, |score, term| {
        let bonus = if file_name == term || file_name.strip_suffix(".rs") == Some(term) {
            200
        } else if file_name.contains(term) {
            80
        } else if lower.split('/').any(|component| component == term) {
            60
        } else if lower.contains(term) {
            20
        } else {
            0
        };
        score.saturating_add(bonus)
    }))
}

fn repo_snapshot_hash(root: &Path, files: &[(PathBuf, String)], query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(query.as_bytes());
    for (path, rel) in files {
        hasher.update([0]);
        hasher.update(rel.as_bytes());
        if let Ok(metadata) = path.metadata() {
            hasher.update(metadata.len().to_le_bytes());
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            hasher.update(modified.to_le_bytes());
        }
    }
    let digest = hasher.finalize();
    let mut short = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(short, "{byte:02x}");
    }
    short
}

fn encode_cursor(offset: usize, snapshot: &str) -> Result<String> {
    let bytes =
        serde_json::to_vec(&RepoCursor { version: 1, offset, snapshot: snapshot.to_string() })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> Result<Option<RepoCursor>> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_CURSOR_BYTES {
        return Err(WinxError::ArgumentParseError("CodeMap cursor is too large".to_string()));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        WinxError::ArgumentParseError(
            "Invalid CodeMap cursor; restart the outline without cursor".to_string(),
        )
    })?;
    let cursor: RepoCursor = serde_json::from_slice(&bytes).map_err(|_| {
        WinxError::ArgumentParseError(
            "Invalid CodeMap cursor; restart the outline without cursor".to_string(),
        )
    })?;
    if cursor.version != 1 {
        return Err(WinxError::ArgumentParseError(
            "Unsupported CodeMap cursor version; restart without cursor".to_string(),
        ));
    }
    Ok(Some(cursor))
}

fn repo_cursor_offset(value: &str, snapshot_hash: &str, ranked_len: usize) -> Result<usize> {
    let Some(cursor) = decode_cursor(value)? else { return Ok(0) };
    if cursor.snapshot != snapshot_hash {
        return Err(WinxError::ArgumentParseError(
            "CodeMap cursor is stale because the ranked workspace snapshot changed; restart without cursor"
                .to_string(),
        ));
    }
    if cursor.offset > ranked_len {
        return Err(WinxError::ArgumentParseError(
            "CodeMap cursor offset is outside the current workspace snapshot; restart without cursor"
                .to_string(),
        ));
    }
    Ok(cursor.offset)
}

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: Outline,
) -> Result<(String, serde_json::Value)> {
    let (cwd, workspace_root, thread_id) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (
            bash_state.cwd.clone(),
            bash_state.workspace_root.clone(),
            bash_state.current_thread_id.clone(),
        )
    };

    tokio::task::spawn_blocking(move || outline_with_paths(&args, &cwd, workspace_root, &thread_id))
        .await
        .map_err(|error| {
            WinxError::CommandExecutionError(format!("CodeMap outline worker failed: {error}"))
        })?
}

fn outline_with_paths(
    args: &Outline,
    cwd: &Path,
    workspace_root: PathBuf,
    thread_id: &str,
) -> Result<(String, serde_json::Value)> {
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);
    let root = resolve_in_workspace(&args.path, cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&args.path), message: e.to_string() }
    })?;

    let mut context = TagsContext::new();
    let mut configs: Configs = HashMap::new();

    if root.is_file() {
        if !args.cursor.is_empty() {
            return Err(WinxError::ArgumentParseError(
                "CodeMap cursor is valid only for directory outlines".to_string(),
            ));
        }
        outline_one(&root, &workspace_root, thread_id, args, &mut context, &mut configs)
    } else if root.is_dir() {
        outline_repo(&root, &workspace_root, args, &mut context, &mut configs)
    } else {
        // Don't silently degrade a typo'd file path into a whole-workspace scan.
        Err(WinxError::FileAccessError {
            path: root.clone(),
            message: "path not found (or not a regular file/directory)".to_string(),
        })
    }
}

/// A `file`-mode result carrying only a status message (no symbols).
fn empty_file_outline(
    message: String,
    extension: String,
    language_supported: bool,
    fallback: Option<CodeMapFallback>,
) -> Result<(String, serde_json::Value)> {
    let structured = OutlineOutput {
        mode: "file".to_string(),
        files_shown: 0,
        files: Vec::new(),
        truncated: false,
        next_cursor: None,
        snapshot_hash: None,
        files_scanned: 1,
        payload_bytes: 0,
        file_extension: Some(extension),
        language_supported: Some(language_supported),
        fallback,
    };
    let value = finalize_output(&message, structured)?;
    Ok((message, value))
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

fn render_file_outline(rel: &str, total: usize, syms: &[Symbol]) -> String {
    let mut out = String::new();
    if syms.len() < total {
        let _ = writeln!(out, "{rel} ({} of {total} symbols):", syms.len());
    } else {
        let noun = if total == 1 { "symbol" } else { "symbols" };
        let _ = writeln!(out, "{rel} ({total} {noun}):");
    }
    render_symbols(&mut out, syms);
    if syms.len() < total {
        let _ = write!(out, "(...{} more; narrow path or raise max_results)", total - syms.len());
    }
    out
}

fn to_output(syms: Vec<Symbol>) -> Vec<OutlineSymbol> {
    syms.into_iter().map(|s| OutlineSymbol { name: s.name, kind: s.kind, line: s.line }).collect()
}

fn outline_one(
    file: &Path,
    workspace_root: &Path,
    thread_id: &str,
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
        let temporary_artifact_dir =
            crate::utils::agent_temp::session_info(workspace_root, thread_id).directory;
        let fallback = CodeMapFallback {
            tool: "ReadFiles".to_string(),
            file_paths: vec![file.to_string_lossy().into_owned()],
            reason: "unsupported_language".to_string(),
            temporary_artifact_dir: temporary_artifact_dir.to_string_lossy().into_owned(),
        };
        return empty_file_outline(
            format!(
            "No symbols in {rel}: unsupported language (extension `.{ext}`). Use ReadFiles for \
             exact canonical source; do not transform source solely to make CodeMap parse it. A \
             genuinely useful derived helper may live in temporary_artifact_dir with short names \
             and source-path/line provenance."
        ),
            ext,
            false,
            Some(fallback),
        );
    }
    let text = match read_file_to_string(file, MAX_OUTLINE_FILE_SIZE) {
        Ok(text) => text,
        Err(e) => {
            return empty_file_outline(format!("Could not outline {rel}: {e}"), ext, true, None)
        }
    };
    let Some(config) = config_for(configs, &ext) else {
        let temporary_artifact_dir =
            crate::utils::agent_temp::session_info(workspace_root, thread_id).directory;
        return empty_file_outline(
            format!("No symbols in {rel}: the `.{ext}` tags query could not be loaded."),
            ext,
            true,
            Some(CodeMapFallback {
                tool: "ReadFiles".to_string(),
                file_paths: vec![file.to_string_lossy().into_owned()],
                reason: "parser_unavailable".to_string(),
                temporary_artifact_dir: temporary_artifact_dir.to_string_lossy().into_owned(),
            }),
        );
    };

    let syms = symbols::extract(context, config, &text);
    let total = syms.len();
    if syms.is_empty() {
        return empty_file_outline(format!("No definitions found in {rel}."), ext, true, None);
    }

    // Find the largest prefix that respects both max_results and the combined
    // text + structured response budget. Binary search avoids repeatedly
    // serializing hundreds of almost-identical prefixes.
    let upper = total.min(max);
    let mut low = 0usize;
    let mut high = upper;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let selected = &syms[..middle];
        let candidate_text = render_file_outline(&rel, total, selected);
        let candidate_files =
            vec![OutlineFile { file: rel.clone(), symbols: to_output(selected.to_vec()) }];
        if navigation_payload_bytes(&candidate_text, &candidate_files)
            <= CODE_MAP_PAYLOAD_MAX_BYTES - CODE_MAP_PAYLOAD_FIXED_RESERVE
        {
            low = middle;
        } else {
            high = middle - 1;
        }
    }

    let selected = &syms[..low];
    let truncated = low < total;
    let out = if selected.is_empty() {
        format!(
            "{total} definitions found in {rel}, but the first symbol exceeds the CodeMap response budget. Use ReadFiles for exact source."
        )
    } else {
        render_file_outline(&rel, total, selected)
    };

    let structured = OutlineOutput {
        mode: "file".to_string(),
        files_shown: 1,
        files: vec![OutlineFile { file: rel, symbols: to_output(selected.to_vec()) }],
        truncated,
        next_cursor: None,
        snapshot_hash: None,
        files_scanned: 1,
        payload_bytes: 0,
        file_extension: Some(ext),
        language_supported: Some(true),
        fallback: None,
    };
    let value = finalize_output(&out, structured)?;
    Ok((out, value))
}

/// Collect supported files under `root`, ranked by explicit task focus and
/// workspace activity before the generic path-probability prior. Returns
/// `(absolute, workspace-relative)` pairs.
fn ranked_supported_files(
    root: &Path,
    workspace_root: &Path,
    query: &str,
    active_paths: &[String],
) -> Vec<(PathBuf, String)> {
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
    let path_ranking = {
        let rels: Vec<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
        score_paths(&rels)
    }
    .unwrap_or_else(|| vec![0.0; files.len()]);
    let terms = query_terms(query);
    let mut ranked = path_ranking
        .into_iter()
        .zip(files)
        .map(|(path_score, file)| {
            let context_score = contextual_path_score(&file.1, &terms, active_paths);
            (context_score, path_score, file)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| left.2 .1.cmp(&right.2 .1))
    });
    ranked.into_iter().map(|(_, _, file)| file).collect()
}

fn fitting_repo_symbol_prefix(
    current_text: &str,
    current_files: &[OutlineFile],
    rel: &str,
    symbols: &[Symbol],
    item_budget: usize,
) -> usize {
    let mut low = 0usize;
    let mut high = symbols.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let selected = &symbols[..middle];
        let mut chunk = String::new();
        let _ = writeln!(chunk, "{rel}");
        render_symbols(&mut chunk, selected);
        let mut candidate_text = current_text.to_string();
        candidate_text.push_str(&chunk);
        let mut candidate_files = current_files.to_vec();
        candidate_files
            .push(OutlineFile { file: rel.to_string(), symbols: to_output(selected.to_vec()) });
        if navigation_payload_bytes(&candidate_text, &candidate_files) <= item_budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn outline_repo(
    root: &Path,
    workspace_root: &Path,
    args: &Outline,
    context: &mut TagsContext,
    configs: &mut Configs,
) -> Result<(String, serde_json::Value)> {
    let max_files = if args.max_results == 0 { DEFAULT_MAX_FILES } else { args.max_results };
    let active_paths = active_files_for_context(workspace_root);
    let ranked = ranked_supported_files(root, workspace_root, &args.query, &active_paths);
    let snapshot_hash = repo_snapshot_hash(root, &ranked, &args.query);
    let start_offset = repo_cursor_offset(&args.cursor, &snapshot_hash, ranked.len())?;

    let item_budget = CODE_MAP_PAYLOAD_MAX_BYTES - CODE_MAP_PAYLOAD_FIXED_RESERVE;
    let mut out = String::new();
    if start_offset > 0 {
        let _ = writeln!(out, "Code map continuation from ranked offset {start_offset}:");
    }
    let mut out_files: Vec<OutlineFile> = Vec::new();
    let mut omitted_symbols = false;
    let mut files_scanned = 0usize;
    let mut next_offset = start_offset;

    for (index, (abs, rel)) in ranked.iter().enumerate().skip(start_offset) {
        if out_files.len() >= max_files || files_scanned >= MAX_FILES_SCANNED {
            next_offset = index;
            break;
        }
        // read_file_to_string enforces the size cap and rejects non-UTF-8, so a
        // separate metadata stat is redundant — skip on any read error.
        files_scanned += 1;
        next_offset = index + 1;
        let Ok(text) = read_file_to_string(abs, MAX_OUTLINE_FILE_SIZE) else { continue };
        let ext = ext_of(abs);
        let mut syms = match config_for(configs, &ext) {
            Some(config) => symbols::extract(context, config, &text),
            None => continue,
        };
        if syms.is_empty() {
            continue;
        }
        let total_symbols = syms.len();
        if syms.len() > REPO_MAX_SYMBOLS_PER_FILE {
            syms.truncate(REPO_MAX_SYMBOLS_PER_FILE);
            omitted_symbols = true;
        }

        // Fit the largest symbol prefix for this file into the combined text +
        // structured budget. Checking the candidate before committing avoids
        // the old one-dense-file overshoot.
        let low = fitting_repo_symbol_prefix(&out, &out_files, rel, &syms, item_budget);

        if low == 0 {
            omitted_symbols = true;
            // If the page already contains useful entries, leave this file for
            // the next cursor. A single pathological symbol on an empty page is
            // skipped so pagination can still make progress.
            if !out_files.is_empty() {
                next_offset = index;
                break;
            }
            continue;
        }

        let selected = &syms[..low];
        let _ = writeln!(out, "{rel}");
        render_symbols(&mut out, selected);
        out_files.push(OutlineFile { file: rel.clone(), symbols: to_output(selected.to_vec()) });
        if low < total_symbols {
            omitted_symbols = true;
        }

        if low < syms.len() {
            break;
        }
    }

    let next_cursor = if next_offset < ranked.len() {
        Some(encode_cursor(next_offset, &snapshot_hash)?)
    } else {
        None
    };
    let truncated = omitted_symbols || next_cursor.is_some();
    if out_files.is_empty() {
        out = format!("No code symbols found under {}.", root.display());
    } else if truncated {
        if let Some(cursor) = next_cursor.as_deref() {
            let _ =
                write!(out, "(...capped; continue with cursor `{cursor}`, or narrow path/query)");
        } else {
            let _ = write!(out, "(...dense files capped; outline a specific file for more)");
        }
    }

    let files_shown = out_files.len();
    let structured = OutlineOutput {
        mode: "repo".to_string(),
        files_shown,
        files: out_files,
        truncated,
        next_cursor,
        snapshot_hash: Some(snapshot_hash),
        files_scanned,
        payload_bytes: 0,
        file_extension: None,
        language_supported: None,
        fallback: None,
    };
    let value = finalize_output(&out, structured)?;
    Ok((out, value))
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

    fn args(path: &str) -> Outline {
        Outline {
            path: path.to_string(),
            max_results: 0,
            query: String::new(),
            cursor: String::new(),
            thread_id: String::new(),
        }
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
    async fn repo_map_query_focuses_the_first_page() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/alpha.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("src/billing.rs"), "fn invoice() {}\n").unwrap();
        let st = state_in(&dir);
        let mut focused = args("");
        focused.max_results = 1;
        focused.query = "billing invoice".to_string();

        let (_, structured) = handle_tool_call(&st, focused).await.unwrap();

        assert_eq!(structured["files"][0]["file"], "src/billing.rs");
        assert!(structured["next_cursor"].is_string());
    }

    #[tokio::test]
    async fn repo_map_cursor_continues_without_repeating_files() {
        let dir = TempDir::new().unwrap();
        for name in ["alpha", "beta", "gamma"] {
            std::fs::write(dir.path().join(format!("{name}.rs")), format!("fn {name}() {{}}\n"))
                .unwrap();
        }
        let st = state_in(&dir);
        let mut first_args = args("");
        first_args.max_results = 1;
        let (_, first) = handle_tool_call(&st, first_args).await.unwrap();
        let first_file = first["files"][0]["file"].as_str().unwrap().to_string();
        let cursor = first["next_cursor"].as_str().unwrap().to_string();

        let mut second_args = args("");
        second_args.max_results = 1;
        second_args.cursor = cursor;
        let (_, second) = handle_tool_call(&st, second_args).await.unwrap();

        assert_ne!(second["files"][0]["file"], first_file);
        assert_eq!(second["snapshot_hash"], first["snapshot_hash"]);
    }

    #[tokio::test]
    async fn repo_map_rejects_a_cursor_after_snapshot_change() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() {}\n").unwrap();
        let st = state_in(&dir);
        let mut first_args = args("");
        first_args.max_results = 1;
        let (_, first) = handle_tool_call(&st, first_args).await.unwrap();
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        std::fs::write(dir.path().join("c.rs"), "fn c() {}\n").unwrap();

        let mut stale = args("");
        stale.cursor = cursor;
        let error = handle_tool_call(&st, stale).await.unwrap_err();

        assert!(error.to_string().contains("cursor is stale"), "{error}");
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
    async fn unsupported_language_returns_actionable_exact_source_fallback() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("page.heex"), "<p><%= @name %></p>\n").unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("page.heex")).await.unwrap();
        assert!(out.contains("unsupported language"));
        assert!(out.contains("ReadFiles"));
        assert!(out.contains("do not transform source solely"));
        assert_eq!(structured["files_shown"], 0);
        assert_eq!(structured["file_extension"], "heex");
        assert_eq!(structured["language_supported"], false);
        assert_eq!(structured["fallback"]["tool"], "ReadFiles");
        assert_eq!(structured["fallback"]["reason"], "unsupported_language");
        assert_eq!(
            structured["fallback"]["file_paths"][0],
            dir.path().join("page.heex").canonicalize().unwrap().to_string_lossy().as_ref()
        );
        assert!(structured["fallback"]["temporary_artifact_dir"]
            .as_str()
            .is_some_and(|path| path.contains("/.winx/tmp/session-")));
    }

    #[tokio::test]
    async fn python_and_elixir_are_native_code_map_languages() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("service.py"),
            "class Service:\n    def run(self):\n        pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("worker.ex"),
            "defmodule Worker do\n  def run(), do: :ok\nend\n",
        )
        .unwrap();
        let st = state_in(&dir);

        let (python, python_data) = handle_tool_call(&st, args("service.py")).await.unwrap();
        assert!(python.contains("Service"), "{python}");
        assert!(python.contains("run"), "{python}");
        assert_eq!(python_data["language_supported"], true);

        let (elixir, elixir_data) = handle_tool_call(&st, args("worker.ex")).await.unwrap();
        assert!(elixir.contains("Worker"), "{elixir}");
        assert!(elixir.contains("run"), "{elixir}");
        assert_eq!(elixir_data["language_supported"], true);
    }

    #[tokio::test]
    async fn repo_map_prunes_winx_but_explicit_helper_outline_remains_available() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("main.py"), "def canonical():\n    pass\n").unwrap();
        let helper = crate::utils::agent_temp::session_info(&root, "helper-session")
            .directory
            .join("review_adapter.py");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, "def derived_review():\n    pass\n").unwrap();
        let st = state_in(&dir);

        let (repo, _) = handle_tool_call(&st, args("")).await.unwrap();
        assert!(repo.contains("canonical"), "{repo}");
        assert!(!repo.contains("derived_review"), "{repo}");

        let helper_relative = helper.strip_prefix(&root).unwrap().to_string_lossy();
        let (explicit, _) = handle_tool_call(&st, args(&helper_relative)).await.unwrap();
        assert!(explicit.contains("derived_review"), "{explicit}");
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
        assert!(syms.len() <= REPO_MAX_SYMBOLS_PER_FILE, "got {}", syms.len());
        assert_eq!(structured["truncated"], true);
        assert!(
            usize::try_from(structured["payload_bytes"].as_u64().unwrap())
                .is_ok_and(|bytes| bytes <= CODE_MAP_PAYLOAD_MAX_BYTES),
            "{structured}"
        );
    }
}
