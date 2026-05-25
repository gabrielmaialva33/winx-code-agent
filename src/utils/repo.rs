use crate::errors::Result;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_DEPTH: usize = 10;
const MAX_CONTEXT_FILES: usize = 160;
const MAX_RECENT_FILES: usize = 30;

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
    ".github/workflows/ci.yml",
];

const SKIP_DIRS: &[&str] =
    &[".git", ".winx", "target", "node_modules", ".next", "dist", "build", ".venv", "__pycache__"];

#[derive(Debug, Clone)]
pub struct RepoContext {
    pub root: PathBuf,
    pub is_git_repo: bool,
    pub project_summary: String,
    pub recent_files: Vec<String>,
    pub active_files: Vec<String>,
    pub important_files: Vec<String>,
    pub project_files: Vec<String>,
}

pub struct RepoContextAnalyzer;

impl RepoContextAnalyzer {
    pub fn analyze(path: &Path) -> Result<RepoContext> {
        let root = workspace_root(path);
        let is_git_repo = root.join(".git").exists();
        let mut project_files = collect_project_files(&root)?;
        let active_files = crate::utils::workspace_stats::active_files(&root);
        project_files.sort_by_key(|path| (path_score(path, &active_files), path.clone()));
        project_files.truncate(MAX_CONTEXT_FILES);

        let recent_files = if is_git_repo { recent_git_files(&root) } else { Vec::new() };
        let important_files = important_files(&project_files);
        let project_summary = project_summary(&root, is_git_repo, project_files.len());

        Ok(RepoContext {
            root,
            is_git_repo,
            project_summary,
            recent_files,
            active_files,
            important_files,
            project_files,
        })
    }
}

pub fn get_repo_context(path: &Path) -> Result<(String, Vec<String>)> {
    let context = RepoContextAnalyzer::analyze(path)?;
    let mut output = String::new();

    let _ = writeln!(output, "Project root: {}", context.root.display());
    let _ = writeln!(output, "Git repository: {}", if context.is_git_repo { "yes" } else { "no" });
    let _ = writeln!(output, "{}", context.project_summary);

    if !context.important_files.is_empty() {
        output.push_str("\nImportant files:\n");
        for file in &context.important_files {
            let _ = writeln!(output, "- {file}");
        }
    }

    if !context.recent_files.is_empty() {
        output.push_str("\nRecent git files:\n");
        for file in &context.recent_files {
            let _ = writeln!(output, "- {file}");
        }
    }

    if !context.active_files.is_empty() {
        output.push_str("\nActive winx files:\n");
        for file in &context.active_files {
            let _ = writeln!(output, "- {file}");
        }
    }

    output.push_str("\nWorkspace files:\n");
    for file in &context.project_files {
        let _ = writeln!(output, "- {file}");
    }

    Ok((output, context.project_files))
}

fn workspace_root(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn collect_project_files(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    collect_files(root, root, 0, &mut files)?;
    Ok(files)
}

fn collect_files(root: &Path, current: &Path, depth: usize, files: &mut Vec<String>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }

    let mut entries = fs::read_dir(current)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                collect_files(root, &path, depth + 1, files)?;
            }
        } else if path.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_string_lossy().to_string());
            }
        }
    }

    Ok(())
}

fn important_files(files: &[String]) -> Vec<String> {
    files.iter().filter(|file| IMPORTANT_NAMES.contains(&file.as_str())).cloned().collect()
}

fn recent_git_files(root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["log", "--name-only", "--pretty=format:", "-n", "50"])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut recent = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines().map(str::trim) {
        if !line.is_empty() && !recent.iter().any(|existing| existing == line) {
            recent.push(line.to_string());
        }
        if recent.len() >= MAX_RECENT_FILES {
            break;
        }
    }
    recent
}

fn project_summary(root: &Path, is_git_repo: bool, file_count: usize) -> String {
    let manifest = if root.join("Cargo.toml").exists() {
        "Rust/Cargo"
    } else if root.join("package.json").exists() {
        "Node.js"
    } else if root.join("pyproject.toml").exists() {
        "Python"
    } else {
        "generic"
    };
    format!("Detected {manifest} workspace with {file_count} indexed files; git={is_git_repo}.")
}

fn path_score(path: &str, active_files: &[String]) -> usize {
    let important = usize::from(!IMPORTANT_NAMES.contains(&path));
    let active_bonus = if active_files.iter().any(|active| active == path) { 0 } else { 8 };
    let depth = path.matches('/').count();
    let test_penalty = usize::from(path.contains("test") || path.contains("spec"));
    active_bonus + important * 10 + depth + test_penalty
}

#[cfg(test)]
mod tests {
    use super::get_repo_context;
    use crate::errors::Result;
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
}
