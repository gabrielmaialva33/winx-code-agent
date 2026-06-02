//! Repository context, ported for parity with wcgw's `repo_context.py`.
//!
//! The flow mirrors wcgw exactly:
//! 1. Walk the workspace (gitignore-aware) collecting candidate files.
//! 2. For git repos, pull recently-changed files from history (topological).
//! 3. Rank every file with the embedded path-probability model.
//! 4. Build the shown set: active files first, then recent git files, then the
//!    statistically-ranked remainder, up to a size that scales with the repo.
//! 5. Render it as a partially-expanded directory tree.

use crate::errors::Result;
use crate::utils::display_tree::DirectoryTree;
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Stop scanning once we've seen this many filesystem entries (wcgw parity).
const MAX_ENTRIES_CHECK: usize = 100_000;
/// Roughly "10 directory levels deep" — walk depth counts files too, so +1.
const MAX_WALK_DEPTH: usize = 11;
/// How far back through git history to look for recently-touched files.
const MAX_COMMITS_WALK: usize = 500;

/// Build the workspace context string and the list of shown files.
///
/// The returned string is a partially-expanded directory tree, byte-for-byte in
/// the same spirit as wcgw's `DirectoryTree.display()`.
pub fn get_repo_context(path: &Path) -> Result<(String, Vec<String>)> {
    let context_dir = context_dir(path);
    let is_git_repo = find_git_root(&context_dir).is_some();

    let mut all_files = get_all_files_max_depth(&context_dir, is_git_repo);
    all_files.sort(); // deterministic order so score ties resolve stably

    let dynamic_max_files =
        if is_git_repo { calculate_dynamic_file_limit(all_files.len()) } else { 50 };

    let existing: HashSet<&str> = all_files.iter().map(String::as_str).collect();

    let recent_git_files = if is_git_repo {
        let count = std::cmp::max(10, (dynamic_max_files as f64 * 0.2) as usize);
        get_recent_git_files(&context_dir, count, &existing)
    } else {
        Vec::new()
    };

    let ranked = rank_files(&all_files);
    let active = crate::utils::workspace_stats::active_files_for_context(&context_dir);

    // Compose the shown set: active → recent → ranked remainder (no dups).
    let mut top_files: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |file: String, top: &mut Vec<String>, seen: &mut HashSet<String>| {
        if existing.contains(file.as_str()) && seen.insert(file.clone()) {
            top.push(file);
        }
    };

    for file in active {
        push(file, &mut top_files, &mut seen);
    }
    for file in recent_git_files {
        push(file, &mut top_files, &mut seen);
    }
    if top_files.len() < dynamic_max_files {
        for file in ranked {
            if top_files.len() >= dynamic_max_files {
                break;
            }
            if seen.insert(file.clone()) {
                top_files.push(file);
            }
        }
    }

    let mut tree = DirectoryTree::new(&context_dir);
    for file in top_files.iter().take(dynamic_max_files) {
        tree.expand(file);
    }

    Ok((tree.display(), top_files))
}

/// The directory wcgw would treat as the context root: the git toplevel if any,
/// otherwise the path itself (or its parent when a file is passed).
fn context_dir(path: &Path) -> PathBuf {
    if let Some(git_root) = find_git_root(path) {
        return git_root;
    }
    if path.is_file() {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

/// Walk up from `path` looking for a `.git` directory; returns the repo root.
fn find_git_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_file() { path.parent()? } else { path };
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Collect candidate files relative to `root`.
///
/// gitignore filtering is only applied inside a git repo (`require_git`), matching
/// wcgw which passes `repo=None` — and thus never ignores anything — for plain
/// folders. Hidden files are kept (wcgw shows dotfiles unless gitignored); only
/// the `.git` directory itself is always pruned.
fn get_all_files_max_depth(root: &Path, is_git_repo: bool) -> Vec<String> {
    let walker = WalkBuilder::new(root)
        .max_depth(Some(MAX_WALK_DEPTH))
        .hidden(false)
        .parents(true)
        .ignore(false)
        .git_ignore(is_git_repo)
        .git_global(is_git_repo)
        .git_exclude(is_git_repo)
        .require_git(true)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();

    let mut files = Vec::new();
    for entry in walker.flatten() {
        if files.len() >= MAX_ENTRIES_CHECK {
            break;
        }
        if entry.file_type().is_some_and(|file_type| file_type.is_file()) {
            if let Ok(relative) = entry.path().strip_prefix(root) {
                files.push(relative.to_string_lossy().to_string());
            }
        }
    }
    files
}

/// Recently-changed files from git history, newest first, topological order,
/// merges skipped — the CLI mirror of wcgw's pygit2 revwalk.
fn get_recent_git_files(root: &Path, count: usize, existing: &HashSet<&str>) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "--name-only",
            "--no-merges",
            "--topo-order",
            "--format=",
            "-n",
            &MAX_COMMITS_WALK.to_string(),
        ])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut recent = Vec::new();
    let mut seen = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines().map(str::trim) {
        if line.is_empty() || !existing.contains(line) {
            continue;
        }
        if seen.insert(line.to_string()) {
            recent.push(line.to_string());
            if recent.len() >= count {
                break;
            }
        }
    }
    recent
}

/// Scale the number of shown files with repo size (wcgw: 50..=400 linearly).
fn calculate_dynamic_file_limit(total_files: usize) -> usize {
    const MIN_FILES: usize = 50;
    const MAX_FILES: usize = 400;
    if total_files <= MIN_FILES {
        return MIN_FILES;
    }
    let scale = (MAX_FILES - MIN_FILES) as f64 / (30_000.0 - MIN_FILES as f64);
    let dynamic = MIN_FILES + ((total_files - MIN_FILES) as f64 * scale) as usize;
    dynamic.min(MAX_FILES)
}

/// Order files best-first. Uses the embedded path-probability model; if it
/// can't be loaded, falls back to a simple importance/depth heuristic.
fn rank_files(all_files: &[String]) -> Vec<String> {
    if let Some(scores) = crate::utils::path_prob::score_paths(all_files) {
        let mut indexed: Vec<(usize, f64)> = scores.into_iter().enumerate().collect();
        // Higher log-prob first; stable sort keeps the alphabetical order on ties.
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        return indexed.into_iter().map(|(index, _)| all_files[index].clone()).collect();
    }

    let mut files = all_files.to_vec();
    files.sort_by_key(|path| (heuristic_score(path), path.clone()));
    files
}

const IMPORTANT_NAMES: &[&str] = &[
    "Cargo.toml",
    "README.md",
    "AGENTS.md",
    "package.json",
    "pnpm-workspace.yaml",
    "pyproject.toml",
    "go.mod",
    "Dockerfile",
    "docker-compose.yml",
];

/// Fallback ranking when the ML model is unavailable. Lower is better.
fn heuristic_score(path: &str) -> usize {
    let not_important = usize::from(!IMPORTANT_NAMES.contains(&path));
    let depth = path.matches('/').count();
    let test_penalty = usize::from(path.contains("test") || path.contains("spec"));
    not_important * 10 + depth + test_penalty
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn builds_repo_context_from_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        std::fs::write(temp_dir.path().join("Cargo.toml"), "[package]\nname='x'\n")?;
        std::fs::create_dir(temp_dir.path().join("src"))?;
        std::fs::write(temp_dir.path().join("src/lib.rs"), "pub fn x() {}\n")?;

        let (context, files) = get_repo_context(temp_dir.path())?;
        assert!(context.contains("Cargo.toml"));
        assert!(files.iter().any(|file| file == "src/lib.rs"));
        Ok(())
    }

    #[test]
    fn dynamic_limit_scales_between_bounds() {
        assert_eq!(calculate_dynamic_file_limit(10), 50);
        assert_eq!(calculate_dynamic_file_limit(50), 50);
        assert!(calculate_dynamic_file_limit(1000) > 50);
        assert_eq!(calculate_dynamic_file_limit(1_000_000), 400);
    }

    #[test]
    fn respects_gitignore_in_git_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();
        std::fs::create_dir(root.join(".git"))?; // mark as git repo
        std::fs::write(root.join(".gitignore"), "ignored.txt\n")?;
        std::fs::write(root.join("ignored.txt"), "secret\n")?;
        std::fs::write(root.join("kept.rs"), "fn x() {}\n")?;

        let files = get_all_files_max_depth(root, true);
        assert!(files.iter().any(|file| file == "kept.rs"));
        assert!(!files.iter().any(|file| file == "ignored.txt"), "gitignore must hide ignored.txt");
        Ok(())
    }
}
