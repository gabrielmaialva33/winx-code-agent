//! Implementation of the `SearchFiles` tool — a structured, gitignore-aware grep.
//!
//! Reuses the `ignore` walker (so `.gitignore`/`target`/`node_modules` are
//! skipped inside a git repo) and the Rust `regex` engine. Read-only and
//! workspace-confined, so it works in every mode — including `architect` /
//! `code_writer`, where shelling out to `rg`/`grep` is blocked.

use std::fmt::Write as FmtWrite;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use regex::{Regex, RegexBuilder};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::SearchFiles;
use crate::utils::encoder::budget_from_env;
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{glob_matches, resolve_in_workspace};
use crate::utils::repo::walk_workspace_files;

/// Cap on matching lines returned when the caller passes 0.
const DEFAULT_MAX_RESULTS: usize = 200;
/// Skip files larger than this when scanning — a grep over multi-MB generated
/// blobs is rarely what you want and would blow the output budget anyway.
const MAX_SCAN_FILE_SIZE: u64 = 5_000_000;
/// Default token budget for the rendered result (overridable via env). Used as a
/// cheap byte proxy (~4 bytes/token) to stop before the output gets huge.
const SEARCH_TOKEN_BUDGET: usize = 24_000;
/// Clamp on `context_lines` — bounds per-file output and keeps the context index
/// arithmetic well away from overflow on an absurd request value.
const MAX_CONTEXT_LINES: usize = 1_000;
/// Stop after scanning this many (post-filter) files, so a no-match query on a
/// huge monorepo can't read the whole tree into memory.
const MAX_FILES_SCANNED: usize = 20_000;

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    search: SearchFiles,
) -> Result<String> {
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };

    // Canonicalize once so workspace-relative stripping is reliable even when the
    // stored root kept symlink components (e.g. a resumed session) — otherwise a
    // failed strip_prefix would leak the absolute path.
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);

    if search.pattern.trim().is_empty() {
        return Err(WinxError::ArgumentParseError("Search pattern must not be empty.".to_string()));
    }

    let root = resolve_in_workspace(&search.path, &cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&search.path), message: e.to_string() }
    })?;

    let regex = RegexBuilder::new(&search.pattern)
        .case_insensitive(search.ignore_case)
        .build()
        .map_err(|e| WinxError::ArgumentParseError(format!("Invalid regex: {e}")))?;

    let glob = match search.glob.trim() {
        "" => None,
        g => Some(
            glob::Pattern::new(g)
                .map_err(|e| WinxError::ArgumentParseError(format!("Invalid glob '{g}': {e}")))?,
        ),
    };

    let max_results =
        if search.max_results == 0 { DEFAULT_MAX_RESULTS } else { search.max_results };
    let max_bytes =
        budget_from_env("WINX_SEARCH_TOKEN_BUDGET", SEARCH_TOKEN_BUDGET).saturating_mul(4);
    let around = search.context_lines.min(MAX_CONTEXT_LINES);

    let mut body = String::new();
    let mut total = 0usize;
    let mut files_hit = 0usize;
    let mut scanned = 0usize;
    let mut stopped: Option<&str> = None;

    for file in walk_workspace_files(&root) {
        if total >= max_results {
            stopped = Some("max_results");
            break;
        }
        if body.len() >= max_bytes {
            stopped = Some("budget");
            break;
        }
        if scanned >= MAX_FILES_SCANNED {
            stopped = Some("scanned");
            break;
        }

        if let Some(glob) = &glob {
            let rel = file.strip_prefix(&workspace_root).unwrap_or(&file);
            if !glob_matches(glob, rel) {
                continue;
            }
        }

        let Ok(meta) = std::fs::metadata(&file) else { continue };
        if meta.len() > MAX_SCAN_FILE_SIZE {
            continue;
        }
        // read_file_to_string rejects non-UTF-8, so binaries are skipped here.
        let Ok(content) = read_file_to_string(&file, MAX_SCAN_FILE_SIZE) else { continue };
        scanned += 1;

        let rel = file.strip_prefix(&workspace_root).unwrap_or(&file).to_string_lossy().to_string();
        let added = scan_file(&content, &regex, &rel, around, max_results - total, &mut body);
        if added > 0 {
            files_hit += 1;
            total += added;
        }
    }

    if total == 0 {
        return Ok(format!("No matches for /{}/ under {}.", search.pattern, root.display()));
    }

    let mut out = format!("{total} match(es) in {files_hit} file(s) for /{}/:\n", search.pattern);
    out.push_str(&body);
    match stopped {
        Some("max_results") => {
            let _ = write!(
                out,
                "\n(...stopped at {max_results} matches; refine the pattern or raise max_results.)"
            );
        }
        Some("budget") => {
            let _ = write!(
                out,
                "\n(...output budget reached; narrow the search with `path` or `glob`.)"
            );
        }
        Some("scanned") => {
            let _ = write!(
                out,
                "\n(...stopped after scanning {MAX_FILES_SCANNED} files; narrow with `path` or `glob`.)"
            );
        }
        _ => {}
    }
    Ok(out)
}

/// Scan one file's text, appending up to `remaining` matches (with `around`
/// lines of surrounding context) to `out`. Match lines render as `line:text`,
/// context as `line-text` (the grep `-A/-B` convention). Returns matches written.
///
/// Each source line is emitted at most once, in ascending order: `last_emitted`
/// tracks the highest line already written so overlapping context windows from
/// adjacent matches merge instead of duplicating lines. All index math is
/// saturating, so even a pathological `around` can't underflow a slice.
fn scan_file(
    content: &str,
    regex: &Regex,
    rel: &str,
    around: usize,
    remaining: usize,
    out: &mut String,
) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let mut added = 0usize;
    let mut header = false;
    let mut last_emitted = 0usize; // highest 1-based line number already written
    for (i, line) in lines.iter().enumerate() {
        if added >= remaining {
            break;
        }
        if !regex.is_match(line) {
            continue;
        }
        if !header {
            let _ = writeln!(out, "\n{rel}");
            header = true;
        }
        let lineno = i + 1;
        let start = lineno.saturating_sub(around).max(last_emitted + 1);
        let end = lineno.saturating_add(around).min(lines.len());
        // `start > end` yields an empty range (no panic) when the window was
        // already covered by a previous match's context.
        for ln in start..=end {
            let marker = if ln == lineno { ':' } else { '-' };
            let _ = writeln!(out, "{ln}{marker}{}", lines[ln - 1]);
        }
        if end > last_emitted {
            last_emitted = end;
        }
        added += 1;
    }
    added
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

    fn args(pattern: &str) -> SearchFiles {
        SearchFiles {
            pattern: pattern.to_string(),
            path: String::new(),
            glob: String::new(),
            ignore_case: false,
            context_lines: 0,
            max_results: 0,
            thread_id: String::new(),
        }
    }

    #[tokio::test]
    async fn finds_matches_with_line_numbers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\nlet x = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here\n").unwrap();
        let st = state_in(&dir);
        let out = handle_tool_call(&st, args("fn foo")).await.unwrap();
        assert!(out.contains("a.rs"));
        assert!(out.contains("1:fn foo() {}"));
        assert!(!out.contains("b.txt"));
    }

    #[tokio::test]
    async fn respects_glob_filter() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "target\n").unwrap();
        std::fs::write(dir.path().join("a.py"), "target\n").unwrap();
        let st = state_in(&dir);
        let mut a = args("target");
        a.glob = "*.rs".to_string();
        let out = handle_tool_call(&st, a).await.unwrap();
        assert!(out.contains("a.rs"));
        assert!(!out.contains("a.py"));
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        let st = state_in(&dir);
        let out = handle_tool_call(&st, args("zzz_definitely_absent")).await.unwrap();
        assert!(out.to_lowercase().contains("no matches"));
    }

    #[tokio::test]
    async fn invalid_regex_errors() {
        let dir = TempDir::new().unwrap();
        let st = state_in(&dir);
        assert!(handle_tool_call(&st, args("(unclosed")).await.is_err());
    }

    #[tokio::test]
    async fn huge_context_lines_does_not_panic() {
        // Regression: context_lines = usize::MAX used to overflow the trailing
        // context index and panic on a match — which aborts the whole server
        // under panic="abort". It must clamp and return Ok instead.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "match me\nx\ny\n").unwrap();
        let st = state_in(&dir);
        let mut a = args("match");
        a.context_lines = usize::MAX;
        let out = handle_tool_call(&st, a).await.unwrap();
        assert!(out.contains("a.rs"));
    }

    #[tokio::test]
    async fn merges_overlapping_context_without_duplicates() {
        // Two adjacent matches with context must emit each line once, ascending.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hit1\nmid\nhit2\n").unwrap();
        let st = state_in(&dir);
        let mut a = args("hit");
        a.context_lines = 1;
        let out = handle_tool_call(&st, a).await.unwrap();
        assert_eq!(out.matches("mid").count(), 1, "shared context line must not duplicate");
    }
}
