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
/// Stop after reading+parsing this many files (mirrors `SearchFiles`).
const MAX_FILES_SCANNED: usize = 20_000;

type Configs = HashMap<String, Option<TagsConfiguration>>;

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    args: FindReferences,
) -> Result<(String, serde_json::Value)> {
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let bash_state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);

    if args.name.trim().is_empty() {
        return Err(WinxError::ArgumentParseError("Symbol name must not be empty.".to_string()));
    }

    let root = resolve_in_workspace(&args.path, &cwd, &workspace_root).map_err(|e| {
        WinxError::PathSecurityError { path: PathBuf::from(&args.path), message: e.to_string() }
    })?;
    let max = if args.max_results == 0 { DEFAULT_MAX_HITS } else { args.max_results };

    let candidates: Vec<PathBuf> =
        if root.is_file() { vec![root.clone()] } else { walk_workspace_files(&root) };

    let mut context = TagsContext::new();
    let mut configs: Configs = HashMap::new();
    let mut hits: Vec<ReferenceHit> = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;

    for abs in candidates {
        if hits.len() >= max || scanned >= MAX_FILES_SCANNED {
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
            hits.push(ReferenceHit {
                file: rel.clone(),
                line: s.line,
                kind: s.kind,
                is_definition: s.is_definition,
            });
        }
    }

    // Definitions first (most useful), then by file/line. Cap keeps definitions.
    hits.sort_by(|a, b| {
        b.is_definition
            .cmp(&a.is_definition)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    if hits.len() > max {
        hits.truncate(max);
        truncated = true;
    }

    let definitions = hits.iter().filter(|h| h.is_definition).count();
    let references = hits.len() - definitions;

    let out = render(&args.name, &hits, definitions, references, truncated, &root);
    let structured = ReferencesOutput { name: args.name, definitions, references, truncated, hits };
    Ok((out, crate::tools::structured_json(&structured)?))
}

fn render(
    name: &str,
    hits: &[ReferenceHit],
    definitions: usize,
    references: usize,
    truncated: bool,
    root: &Path,
) -> String {
    if hits.is_empty() {
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

    // ADVERSARIAL: does the top-of-loop hits.len()>=max cap drop the definition
    // when many reference files are walked before the def file?
    #[tokio::test]
    async fn adversarial_def_dropped_by_early_cap() {
        let dir = TempDir::new().unwrap();
        // 5000 ref-only files. Walk order is filesystem readdir order (NOT
        // sorted by walk_workspace_files), so the single def file is very
        // unlikely to land in the first 200 visited.
        for i in 0..5000 {
            std::fs::write(
                dir.path().join(format!("ref_{i:05}.rs")),
                "fn caller() { target(); }\n",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("the_def.rs"), "fn target() {}\n").unwrap();
        let st = state_in(&dir);
        let (out, structured) = handle_tool_call(&st, args("target")).await.unwrap();
        eprintln!("DEFS={} REFS={} TRUNC={}", structured["definitions"], structured["references"], structured["truncated"]);
        // The claim under review: "definitions are never dropped before references".
        // This asserts the BUG: if the def is dropped, definitions==0.
        eprintln!("DEFINITION SURVIVED? {}", structured["definitions"] == 1);
        assert_eq!(structured["definitions"], 1, "BUG CONFIRMED: definition dropped by early cap before def file was scanned. out head:\n{}", &out[..out.len().min(200)]);
    }

    // ADVERSARIAL: over-matching across types — A::run and B::run both named run.
    #[tokio::test]
    async fn adversarial_method_overmatch_across_types() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "struct A;\nstruct B;\nimpl A { fn run(&self) {} }\nimpl B { fn run(&self) {} }\n",
        )
        .unwrap();
        let st = state_in(&dir);
        let (_out, structured) = handle_tool_call(&st, args("run")).await.unwrap();
        eprintln!("run DEFS={} REFS={}", structured["definitions"], structured["references"]);
        // Just observe: bare-name match cannot distinguish A::run from B::run.
    }
}
