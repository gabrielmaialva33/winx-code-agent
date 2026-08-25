//! Implementation of the `FindReferences` tool — tree-sitter symbol occurrences.
//!
//! Finds where an identifier is defined and referenced (called / used) across
//! the workspace, by name. It is a *name-based* semantic lookup (it counts only
//! real identifier occurrences, never matches inside strings or comments like a
//! raw grep would), built on the same `tree-sitter-tags` engine as `Outline`.
//! Read-only, gitignore-aware, workspace-confined; works in every mode.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{FindReferences, ReferenceHit, ReferencesOutput};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::resolve_in_workspace;
use crate::utils::repo::walk_workspace_files;
use crate::utils::symbols;

/// Cap on occurrences returned when the caller passes 0.
const DEFAULT_MAX_HITS: usize = 200;
/// Skip files larger than this when scanning.
const MAX_FILE_SIZE: u64 = 2_000_000;
/// Stop after reading+parsing this many files (a scan-budget cap).
const MAX_FILES_SCANNED: usize = 20_000;
/// Leave room for the shared `CodeMap` envelope and source-scope metadata.
const PAYLOAD_FIXED_RESERVE: usize = 1_024;

type Configs = HashMap<String, Option<TagsConfiguration>>;

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: FindReferences,
) -> Result<(String, serde_json::Value)> {
    handle_tool_call_with_budget(
        bash_state_arc,
        args,
        crate::tools::outline::CODE_MAP_PAYLOAD_MAX_BYTES,
    )
    .await
}

pub(crate) async fn handle_tool_call_with_budget(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: FindReferences,
    payload_max_bytes: usize,
) -> Result<(String, serde_json::Value)> {
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };

    tokio::task::spawn_blocking(move || {
        references_with_paths(&args, &cwd, workspace_root, payload_max_bytes)
    })
    .await
    .map_err(|error| {
        WinxError::CommandExecutionError(format!("CodeMap references worker failed: {error}"))
    })?
}

fn references_with_paths(
    args: &FindReferences,
    cwd: &Path,
    workspace_root: PathBuf,
    payload_max_bytes: usize,
) -> Result<(String, serde_json::Value)> {
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);

    if args.name.trim().is_empty() {
        return Err(WinxError::ArgumentParseError("Symbol name must not be empty.".to_string()));
    }

    let root = resolve_in_workspace(&args.path, cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&args.path), message: e.to_string() }
    })?;
    let max = if args.max_results == 0 { DEFAULT_MAX_HITS } else { args.max_results };

    // Don't silently degrade a typo'd file path into an empty whole-workspace
    // scan (mirrors Outline).
    let candidates: Vec<PathBuf> = if root.is_file() {
        vec![root.clone()]
    } else if root.is_dir() {
        walk_workspace_files(&root)
    } else {
        return Err(WinxError::FileNotFound { path: root.clone() });
    };

    let mut context = TagsContext::new();
    let mut configs: Configs = HashMap::new();
    // Two buckets: definitions are collected UNCAPPED (there are few, and losing
    // one is the worst failure), only references are capped at `max`. The walk is
    // stopped only by the file budget — never by the hit count — so a definition
    // in a late-walked file is never missed (which the naive top-of-loop cap did).
    let mut defs: Vec<ReferenceHit> = Vec::new();
    let mut refs: Vec<ReferenceHit> = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;

    for abs in candidates {
        if scanned >= MAX_FILES_SCANNED {
            truncated = true;
            break;
        }
        let ext = ext_of(&abs);
        if !symbols::supports(&ext) {
            continue;
        }
        let Ok(text) = read_file_to_string(&abs, MAX_FILE_SIZE) else { continue };
        scanned += 1;
        let rel = rel_of(&abs, &workspace_root);
        let syms = match config_for(&mut configs, &ext) {
            Some(config) => symbols::extract_all(&mut context, config, &text),
            None => continue,
        };
        for s in syms {
            if s.name != args.name {
                continue;
            }
            let hit = ReferenceHit {
                file: rel.clone(),
                line: s.line,
                kind: s.kind,
                is_definition: s.is_definition,
            };
            if s.is_definition {
                defs.push(hit);
            } else if refs.len() < max {
                refs.push(hit);
            } else {
                truncated = true; // more references than the cap allows
            }
        }
    }

    let by_loc =
        |a: &ReferenceHit, b: &ReferenceHit| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line));
    defs.sort_by(by_loc);
    refs.sort_by(by_loc);
    let mut hits = defs;
    hits.extend(refs); // definitions first, then references

    bounded_output(&args.name, &hits, truncated, &root, payload_max_bytes)
}

fn bounded_output(
    name: &str,
    hits: &[ReferenceHit],
    already_truncated: bool,
    root: &Path,
    payload_max_bytes: usize,
) -> Result<(String, serde_json::Value)> {
    let budget = payload_max_bytes.saturating_sub(PAYLOAD_FIXED_RESERVE);
    let mut low = 0usize;
    let mut high = hits.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let selected = &hits[..middle];
        let candidate = reference_output(name, selected, already_truncated || middle < hits.len());
        let text = render(
            name,
            selected,
            candidate.definitions,
            candidate.references,
            candidate.truncated,
            root,
            !hits.is_empty(),
        );
        let bytes = text.len().saturating_add(
            serde_json::to_vec(&candidate).map_or(usize::MAX, |serialized| serialized.len()),
        );
        if bytes <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }

    let selected = &hits[..low];
    let mut output =
        reference_output(name, selected, already_truncated || selected.len() < hits.len());
    let text = render(
        name,
        selected,
        output.definitions,
        output.references,
        output.truncated,
        root,
        !hits.is_empty(),
    );
    for _ in 0..3 {
        let serialized = serde_json::to_vec(&output)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let measured = text.len().saturating_add(serialized.len());
        if measured == output.payload_bytes {
            break;
        }
        output.payload_bytes = measured;
    }
    Ok((text, crate::tools::structured_json(&output)?))
}

fn reference_output(name: &str, hits: &[ReferenceHit], truncated: bool) -> ReferencesOutput {
    ReferencesOutput {
        name: name.to_string(),
        definitions: hits.iter().filter(|hit| hit.is_definition).count(),
        references: hits.iter().filter(|hit| !hit.is_definition).count(),
        truncated,
        payload_bytes: 0,
        hits: hits.to_vec(),
    }
}

fn render(
    name: &str,
    hits: &[ReferenceHit],
    definitions: usize,
    references: usize,
    truncated: bool,
    root: &Path,
    had_hits: bool,
) -> String {
    if hits.is_empty() {
        if had_hits {
            return format!(
                "Occurrences of `{name}` were found under {}, but the first result exceeds the \
                 CodeMap response budget. Narrow `path` or use ReadFiles for exact source.",
                root.display()
            );
        }
        return format!("No occurrences of `{name}` found under {}.", root.display());
    }
    let mut out = format!("`{name}` — {definitions} definition(s), {references} reference(s):\n");
    for h in hits {
        let tag = if h.is_definition { "def" } else { "ref" };
        let loc = format!("{}:{}", h.file, h.line);
        let _ = writeln!(out, "  {tag}  {loc:<40} {:<9} {name}", h.kind);
    }
    if truncated {
        let _ = write!(out, "(...capped; narrow `path` or raise max_results)");
    }
    out
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

    fn args(name: &str) -> FindReferences {
        FindReferences {
            name: name.to_string(),
            path: String::new(),
            max_results: 0,
            thread_id: String::new(),
        }
    }

    #[tokio::test]
    async fn finds_definition_and_references() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn target() {}\nfn caller() { target(); target(); }\n",
        )
        .unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("target")).await.unwrap();
        assert!(out.contains("target"));
        assert_eq!(structured["definitions"], 1);
        assert!(structured["references"].as_u64().unwrap() >= 1);
        // definition is listed first
        assert_eq!(structured["hits"][0]["is_definition"], true);
    }

    #[tokio::test]
    async fn finds_python_and_elixir_definitions_and_calls() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("service.py"),
            "def python_target():\n    return 1\n\npython_target()\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("worker.ex"),
            "defmodule Worker do\n  def elixir_target(), do: :ok\n  def run(), do: elixir_target()\nend\n",
        )
        .unwrap();
        let st = state_in(&dir);

        let (_, python) = handle_tool_call(&st, args("python_target")).await.unwrap();
        assert_eq!(python["definitions"], 1);
        assert!(python["references"].as_u64().unwrap() >= 1, "{python}");

        let (_, elixir) = handle_tool_call(&st, args("elixir_target")).await.unwrap();
        assert_eq!(elixir["definitions"], 1);
        assert!(elixir["references"].as_u64().unwrap() >= 1, "{elixir}");
    }

    #[tokio::test]
    async fn empty_name_errors() {
        let dir = TempDir::new().unwrap();
        let st = state_in(&dir);
        assert!(handle_tool_call(&st, args("  ")).await.is_err());
    }

    #[tokio::test]
    async fn reports_no_occurrences() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn something() {}\n").unwrap();
        let st = state_in(&dir);
        let (out, _) = handle_tool_call(&st, args("nonexistent_symbol")).await.unwrap();
        assert!(out.to_lowercase().contains("no occurrences"));
    }

    #[tokio::test]
    async fn definition_survives_when_references_exceed_cap() {
        // Regression (B1): definitions are uncapped, so the def is found and kept
        // even when far more reference hits than `max` are collected, regardless
        // of filesystem walk order.
        let dir = TempDir::new().unwrap();
        for i in 0..20 {
            std::fs::write(
                dir.path().join(format!("ref_{i:02}.rs")),
                "fn caller() { target(); }\n",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("zzz_def.rs"), "fn target() {}\n").unwrap();
        let st = state_in(&dir);
        let mut a = args("target");
        a.max_results = 3; // far fewer than the 20 reference hits
        let (_, structured) = handle_tool_call(&st, a).await.unwrap();
        assert_eq!(
            structured["definitions"], 1,
            "definition must never be dropped by the reference cap"
        );
        assert!(structured["references"].as_u64().unwrap() <= 3);
        assert_eq!(structured["truncated"], true);
        assert_eq!(structured["hits"][0]["is_definition"], true); // definitions listed first
    }

    #[tokio::test]
    async fn bare_name_matches_same_name_across_types() {
        // Documented limitation: a name-based lookup can't distinguish A::run from
        // B::run — both count. Useful as a superset for rename/impact analysis.
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "struct A;\nstruct B;\nimpl A { fn run(&self) {} }\nimpl B { fn run(&self) {} }\n",
        )
        .unwrap();
        let st = state_in(&dir);
        let (_, structured) = handle_tool_call(&st, args("run")).await.unwrap();
        assert_eq!(structured["definitions"], 2);
    }

    #[tokio::test]
    async fn dense_references_respect_the_combined_payload_budget() {
        let dir = TempDir::new().unwrap();
        let mut source = "fn target() {}\nfn caller() {\n".to_string();
        for _ in 0..800 {
            source.push_str("    target();\n");
        }
        source.push_str("}\n");
        std::fs::write(dir.path().join("dense.rs"), source).unwrap();
        let st = state_in(&dir);
        let mut request = args("target");
        request.max_results = 1_000;

        let (text, structured) =
            handle_tool_call_with_budget(&st, request, 8 * 1024).await.unwrap();
        let bytes = text.len() + serde_json::to_vec(&structured).unwrap().len();
        assert!(bytes <= 8 * 1024, "payload used {bytes} bytes");
        assert_eq!(structured["truncated"], true);
        assert_eq!(structured["hits"][0]["is_definition"], true);
    }

    #[tokio::test]
    async fn nonexistent_path_errors() {
        // B3: a typo'd path must error, not silently scan the whole workspace.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("real.rs"), "fn x() {}\n").unwrap();
        let st = state_in(&dir);
        let mut a = args("x");
        a.path = "nope_typo.rs".to_string();
        assert!(handle_tool_call(&st, a).await.is_err());
    }
}
