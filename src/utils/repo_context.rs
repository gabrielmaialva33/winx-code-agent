//! Repository context analysis module inspired by WCGW
//!
//! This module provides intelligent repository analysis capabilities including:
//! - Git repository detection and recent file tracking
//! - Project structure analysis with ignored files support
//! - File importance scoring and prioritization
//! - Workspace statistics and metadata

use anyhow::{Context as AnyhowContext, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Maximum entries to check during repository traversal
const MAX_ENTRIES_CHECK: usize = 100_000;

/// Maximum depth for directory traversal
const DEFAULT_MAX_DEPTH: usize = 10;

/// Repository context information
#[derive(Debug, Clone)]
pub struct RepoContext {
    pub workspace_root: PathBuf,
    pub is_git_repo: bool,
    pub project_files: Vec<String>,
    pub recent_files: Vec<String>,
    pub important_files: Vec<String>,
    pub ignored_patterns: Vec<String>,
    pub file_stats: HashMap<String, FileStats>,
    pub project_summary: String,
}

/// File statistics and metadata
#[derive(Debug, Clone)]
pub struct FileStats {
    pub path: String,
    pub size: u64,
    pub last_modified: std::time::SystemTime,
    pub importance_score: f64,
    pub file_type: String,
    pub is_ignored: bool,
}

/// Git repository analyzer
#[derive(Debug)]
pub struct GitAnalyzer {
    repo_path: PathBuf,
}

impl GitAnalyzer {
    /// Create a new git analyzer for the given path
    pub fn new(path: &Path) -> Option<Self> {
        let git_path = Self::find_git_root(path)?;
        Some(Self { repo_path: git_path })
    }

    /// Find the git root directory
    fn find_git_root(path: &Path) -> Option<PathBuf> {
        let mut current = path.to_path_buf();
        loop {
            if current.join(".git").exists() {
                return Some(current);
            }
            if !current.pop() {
                break;
            }
        }
        None
    }

    /// Get recently modified files from git history
    pub fn get_recent_files(&self, count: usize) -> Result<Vec<String>> {
        // Use git log to get recently modified files
        let output = std::process::Command::new("git")
            .args(&[
                "log",
                "--name-only",
                "--pretty=format:",
                "-n",
                &(count * 3).to_string(), // Get more than needed to filter
            ])
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run git log")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut seen = HashSet::new();
        let mut recent_files = Vec::new();

        for line in output_str.lines() {
            let line = line.trim();
            if !line.is_empty() && !seen.contains(line) {
                seen.insert(line.to_string());
                recent_files.push(line.to_string());
                if recent_files.len() >= count {
                    break;
                }
            }
        }

        Ok(recent_files)
    }

    /// Check if a path is ignored by git
    pub fn is_ignored(&self, path: &str) -> bool {
        std::process::Command::new("git")
            .args(&["check-ignore", path])
            .current_dir(&self.repo_path)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

/// Repository traversal and analysis
#[derive(Debug)]
pub struct RepoTraverser {
    workspace_root: PathBuf,
    git_analyzer: Option<GitAnalyzer>,
    ignored_patterns: Vec<String>,
    max_depth: usize,
}

impl RepoTraverser {
    /// Create a new repository traverser
    pub fn new(workspace_root: &Path) -> Self {
        let git_analyzer = GitAnalyzer::new(workspace_root);
        
        // Default ignore patterns (similar to WCGW)
        let ignored_patterns = vec![
            ".git".to_string(),
            "node_modules".to_string(),
            "target".to_string(),
            ".venv".to_string(),
            "__pycache__".to_string(),
            ".pytest_cache".to_string(),
            "dist".to_string(),
            "build".to_string(),
            ".cargo".to_string(),
            "*.tmp".to_string(),
            "*.log".to_string(),
        ];

        Self {
            workspace_root: workspace_root.to_path_buf(),
            git_analyzer,
            ignored_patterns,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Get all files with BFS traversal (WCGW-style)
    pub fn get_all_files(&self) -> Result<Vec<String>> {
        let mut all_files = Vec::new();
        let mut queue = VecDeque::new();
        let mut entries_check = 0;

        // Start with workspace root
        queue.push_back((self.workspace_root.clone(), 0, String::new()));

        while let Some((current_folder, depth, prefix)) = queue.pop_front() {
            if entries_check >= MAX_ENTRIES_CHECK || depth > self.max_depth {
                continue;
            }

            let entries = match fs::read_dir(&current_folder) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            let mut files = Vec::new();
            let mut folders = Vec::new();

            for entry in entries {
                entries_check += 1;
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };

                let name = entry.file_name().to_string_lossy().to_string();
                let rel_path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", prefix, name)
                };

                // Check if ignored
                if self.is_path_ignored(&rel_path) {
                    continue;
                }

                let file_type = entry.file_type();
                match file_type {
                    Ok(ft) if ft.is_file() => files.push(rel_path),
                    Ok(ft) if ft.is_dir() => folders.push((entry.path(), rel_path)),
                    _ => continue,
                }
            }

            // Add files to result
            all_files.extend(files);

            // Add folders to queue for BFS
            for (folder_path, folder_rel_path) in folders {
                queue.push_back((folder_path, depth + 1, folder_rel_path));
            }
        }

        Ok(all_files)
    }

    /// Check if a path should be ignored
    fn is_path_ignored(&self, path: &str) -> bool {
        // Check git ignore first
        if let Some(ref git) = self.git_analyzer {
            if git.is_ignored(path) {
                return true;
            }
        }

        // Check against our ignore patterns
        for pattern in &self.ignored_patterns {
            if pattern.contains('*') {
                if glob::Pattern::new(pattern)
                    .map(|p| p.matches(path))
                    .unwrap_or(false)
                {
                    return true;
                }
            } else if path.contains(pattern) {
                return true;
            }
        }

        false
    }

    /// Calculate importance score for a file (WCGW-style)
    fn calculate_importance_score(&self, path: &str, file_stats: &FileStats) -> f64 {
        let mut score = 0.0;

        // File type importance
        if let Some(ext) = Path::new(path).extension() {
            score += match ext.to_str() {
                Some("rs") => 10.0,
                Some("py") => 9.0,
                Some("js") | Some("ts") => 8.0,
                Some("json") | Some("toml") | Some("yaml") | Some("yml") => 7.0,
                Some("md") => 5.0,
                Some("txt") => 3.0,
                _ => 1.0,
            };
        }

        // Special files
        let filename = Path::new(path).file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        
        score += match filename.to_lowercase().as_str() {
            "cargo.toml" | "package.json" | "pyproject.toml" => 15.0,
            "readme.md" | "readme.txt" => 10.0,
            "license" | "license.txt" | "license.md" => 8.0,
            "makefile" | "dockerfile" => 7.0,
            _ => 0.0,
        };

        // Size factor (smaller files often more important for config)
        if file_stats.size < 10_000 {
            score += 2.0;
        } else if file_stats.size > 100_000 {
            score -= 1.0;
        }

        // Path depth (files closer to root more important)
        let depth = path.matches('/').count();
        score += (5.0 - depth as f64).max(0.0);

        score
    }

    /// Generate file statistics
    fn generate_file_stats(&self, files: &[String]) -> HashMap<String, FileStats> {
        let mut file_stats = HashMap::new();

        for file_path in files {
            let full_path = self.workspace_root.join(file_path);
            
            if let Ok(metadata) = fs::metadata(&full_path) {
                let stats = FileStats {
                    path: file_path.clone(),
                    size: metadata.len(),
                    last_modified: metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    importance_score: 0.0, // Will be calculated later
                    file_type: Path::new(file_path)
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    is_ignored: self.is_path_ignored(file_path),
                };

                file_stats.insert(file_path.clone(), stats);
            }
        }

        // Calculate importance scores
        for (path, stats) in file_stats.iter_mut() {
            stats.importance_score = self.calculate_importance_score(path, stats);
        }

        file_stats
    }
}

/// Main repository context analyzer
pub struct RepoContextAnalyzer;

impl RepoContextAnalyzer {
    /// Analyze repository and generate context (WCGW-style)
    pub fn analyze(workspace_path: &Path) -> Result<RepoContext> {
        debug!("Analyzing repository context for: {:?}", workspace_path);

        let traverser = RepoTraverser::new(workspace_path);
        let all_files = traverser.get_all_files()?;
        
        debug!("Found {} files in repository", all_files.len());

        // Generate file statistics
        let file_stats = traverser.generate_file_stats(&all_files);

        // Get recent files if git repo
        let recent_files = if let Some(ref git) = traverser.git_analyzer {
            git.get_recent_files(10).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Find important files
        let mut important_files: Vec<_> = file_stats
            .iter()
            .filter(|(_, stats)| stats.importance_score > 5.0)
            .map(|(path, _)| path.clone())
            .collect();
        
        important_files.sort_by(|a, b| {
            file_stats[b].importance_score
                .partial_cmp(&file_stats[a].importance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        important_files.truncate(20); // Top 20 important files

        // Generate project summary
        let project_summary = Self::generate_project_summary(&all_files, &file_stats, workspace_path);

        Ok(RepoContext {
            workspace_root: workspace_path.to_path_buf(),
            is_git_repo: traverser.git_analyzer.is_some(),
            project_files: all_files,
            recent_files,
            important_files,
            ignored_patterns: traverser.ignored_patterns,
            file_stats,
            project_summary,
        })
    }

    /// Generate a WCGW-style project summary
    fn generate_project_summary(
        files: &[String],
        file_stats: &HashMap<String, FileStats>,
        workspace_path: &Path,
    ) -> String {
        let mut summary = String::new();
        
        summary.push_str(&format!("# Project: {}\n\n", 
            workspace_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")));

        // File type distribution
        let mut type_counts = HashMap::new();
        for (_, stats) in file_stats {
            *type_counts.entry(&stats.file_type).or_insert(0) += 1;
        }

        summary.push_str("## File Types:\n");
        let mut types: Vec<_> = type_counts.into_iter().collect();
        types.sort_by(|a, b| b.1.cmp(&a.1));
        
        for (file_type, count) in types.into_iter().take(10) {
            summary.push_str(&format!("- {}: {} files\n", file_type, count));
        }

        // Project structure hint
        summary.push_str("\n## Project Structure:\n");
        if files.iter().any(|f| f.ends_with("Cargo.toml")) {
            summary.push_str("- Rust project (Cargo.toml found)\n");
        }
        if files.iter().any(|f| f.ends_with("package.json")) {
            summary.push_str("- Node.js project (package.json found)\n");
        }
        if files.iter().any(|f| f.ends_with("pyproject.toml") || f.ends_with("requirements.txt")) {
            summary.push_str("- Python project\n");
        }

        summary.push_str(&format!("\nTotal files: {}\n", files.len()));
        
        summary
    }
}