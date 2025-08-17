use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use tracing::{debug, info};

use crate::utils::path_analyzer;

/// Gets a simple directory structure representation for the workspace path
pub fn get_repo_context(path: &Path) -> Result<(String, PathBuf), std::io::Error> {
    // Ensure path is absolute and exists
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    if !abs_path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Path does not exist: {:?}", abs_path),
        ));
    }

    let context_dir = if abs_path.is_file() {
        abs_path.parent().unwrap_or(Path::new("/")).to_path_buf()
    } else {
        abs_path.clone()
    };

    let mut output = Vec::new();
    writeln!(output, "# Workspace structure")?;
    writeln!(output, "{}", context_dir.display())?;

    // Get all files up to a max depth of 3 (adjust as needed)
    let max_depth = 3;
    let max_entries = 400; // Maximum number of entries to process to avoid excessive output
    let mut found_entries = 0;

    // Use BFS to traverse the directory structure
    let mut queue = VecDeque::new();
    queue.push_back((context_dir.clone(), 0, 0)); // (path, depth, indent)

    while let Some((dir_path, depth, indent)) = queue.pop_front() {
        if depth > max_depth || found_entries >= max_entries {
            break;
        }

        // Skip hidden directories and common large directories
        let dir_name = dir_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if dir_name.starts_with(".") && dir_name != "." && depth > 0 {
            continue;
        }

        if ["node_modules", "target", "venv", "dist", "__pycache__"].contains(&dir_name.as_str())
            && depth > 0
        {
            writeln!(output, "{}  {}", "  ".repeat(indent), dir_name)?;
            writeln!(output, "{}    ...", "  ".repeat(indent))?;
            continue;
        }

        // Print this directory
        if depth > 0 {
            writeln!(output, "{}  {}", "  ".repeat(indent), dir_name)?;
        }

        // List entries in this directory
        match fs::read_dir(&dir_path) {
            Ok(entries) => {
                // Collect and sort entries
                let mut files = Vec::new();
                let mut dirs = Vec::new();

                for entry in entries {
                    if found_entries >= max_entries {
                        break;
                    }

                    match entry {
                        Ok(entry) => {
                            let path = entry.path();
                            let file_name = path.file_name().unwrap_or_default().to_string_lossy();

                            // Skip hidden files except at root
                            if file_name.starts_with(".") && depth > 0 && file_name != ".gitignore"
                            {
                                continue;
                            }

                            if path.is_dir() {
                                dirs.push(path);
                            } else {
                                files.push(file_name.to_string());
                            }
                            found_entries += 1;
                        }
                        Err(_) => continue,
                    }
                }

                // Sort files alphabetically for consistent output
                files.sort();

                // Print files
                for file in files {
                    writeln!(output, "{}    {}", "  ".repeat(indent), file)?;
                }

                // Add subdirectories to the queue
                if depth < max_depth {
                    dirs.sort_by(|a, b| {
                        a.file_name()
                            .unwrap_or_default()
                            .cmp(b.file_name().unwrap_or_default())
                    });

                    for dir in dirs {
                        queue.push_back((dir, depth + 1, indent + 1));
                    }
                }
            }
            Err(_) => {
                writeln!(
                    output,
                    "{}    <error reading directory>",
                    "  ".repeat(indent)
                )?;
            }
        }
    }

    // If we hit the limit, indicate there are more files
    if found_entries >= max_entries {
        writeln!(output, "  ... (more files not shown)")?;
    }

    Ok((String::from_utf8_lossy(&output).to_string(), context_dir))
}

/// Attempts to find Git information for the repository
pub fn get_git_info(path: &Path) -> Option<String> {
    // Helper function to run a Git command
    fn run_git_command(dir: &Path, args: &[&str]) -> Option<String> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    }

    // Find the git root directory (if any)
    let git_root = run_git_command(path, &["rev-parse", "--show-toplevel"])?;
    let git_root_path = Path::new(&git_root);

    // Get basic git info
    let current_branch = run_git_command(git_root_path, &["branch", "--show-current"]);
    let last_commit_hash = run_git_command(git_root_path, &["rev-parse", "--short", "HEAD"]);
    let last_commit_msg = run_git_command(git_root_path, &["log", "-1", "--pretty=%s"]);

    // Format the git info
    let mut info = String::new();
    info.push_str("Git Repository Information:\n");
    info.push_str(&format!("  Root: {}\n", git_root));

    if let Some(branch) = current_branch {
        info.push_str(&format!("  Branch: {}\n", branch));
    }

    if let Some(hash) = last_commit_hash {
        info.push_str(&format!("  Last commit: {}", hash));

        if let Some(msg) = last_commit_msg {
            info.push_str(&format!(" - {}", msg));
        }

        info.push('\n');
    }

    Some(info)
}

/// Workspace statistics for managing file relevance and prioritization
#[derive(Debug, Clone)]
pub struct WorkspaceStats {
    /// Map of file paths to their last modified time
    pub file_modified_times: HashMap<String, SystemTime>,

    /// Map of file paths to their access count
    pub file_access_counts: HashMap<String, usize>,

    /// Map of file paths to their edit count
    pub file_edit_counts: HashMap<String, usize>,

    /// Set of recently edited files
    pub recently_edited_files: HashSet<String>,

    /// Set of recently viewed files
    pub recently_viewed_files: HashSet<String>,

    /// Last refresh time
    pub last_refresh: SystemTime,
}

impl Default for WorkspaceStats {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceStats {
    /// Create a new WorkspaceStats
    pub fn new() -> Self {
        Self {
            file_modified_times: HashMap::new(),
            file_access_counts: HashMap::new(),
            file_edit_counts: HashMap::new(),
            recently_edited_files: HashSet::new(),
            recently_viewed_files: HashSet::new(),
            last_refresh: SystemTime::now(),
        }
    }

    /// Record a file access
    pub fn record_file_access(&mut self, file_path: &str) {
        let count = self
            .file_access_counts
            .entry(file_path.to_string())
            .or_insert(0);
        *count += 1;

        self.recently_viewed_files.insert(file_path.to_string());

        // Limit the size of recently viewed files
        if self.recently_viewed_files.len() > 50 {
            // Remove the oldest entries - in this simplified version we just clear half
            let to_keep: Vec<_> = self
                .recently_viewed_files
                .iter()
                .take(25)
                .cloned()
                .collect();

            self.recently_viewed_files.clear();
            for item in to_keep {
                self.recently_viewed_files.insert(item);
            }
        }
    }

    /// Record a file edit
    pub fn record_file_edit(&mut self, file_path: &str) {
        let count = self
            .file_edit_counts
            .entry(file_path.to_string())
            .or_insert(0);
        *count += 1;

        self.recently_edited_files.insert(file_path.to_string());

        // Limit the size of recently edited files
        if self.recently_edited_files.len() > 30 {
            // Remove the oldest entries - in this simplified version we just clear half
            let to_keep: Vec<_> = self
                .recently_edited_files
                .iter()
                .take(15)
                .cloned()
                .collect();

            self.recently_edited_files.clear();
            for item in to_keep {
                self.recently_edited_files.insert(item);
            }
        }
    }

    /// Update file modification time
    pub fn update_file_modified_time(&mut self, file_path: &str, modified_time: SystemTime) {
        self.file_modified_times
            .insert(file_path.to_string(), modified_time);
    }

    /// Refresh workspace stats by scanning the filesystem
    pub fn refresh(&mut self, workspace_path: &Path) -> std::io::Result<()> {
        self.last_refresh = SystemTime::now();

        // Collect files using the parallel implementation
        let files = collect_files_par(workspace_path, None)?;

        // Process file metadata
        for file_path in files {
            if let Ok(metadata) = fs::metadata(&file_path) {
                if let Ok(modified) = metadata.modified() {
                    let path_str = file_path.to_string_lossy().to_string();
                    self.update_file_modified_time(&path_str, modified);
                }
            }
        }

        Ok(())
    }

    /// Get the most active files based on access and edit counts
    pub fn get_most_active_files(&self, limit: usize) -> Vec<String> {
        // Combine access and edit counts with higher weight for edits
        let mut activity_scores: HashMap<String, usize> = HashMap::new();

        for (file, count) in &self.file_access_counts {
            let score = activity_scores.entry(file.clone()).or_insert(0);
            *score += count;
        }

        for (file, count) in &self.file_edit_counts {
            let score = activity_scores.entry(file.clone()).or_insert(0);
            // Edits are weighted 3x more than views
            *score += count * 3;
        }

        // Add recently edited files with a boost
        for file in &self.recently_edited_files {
            let score = activity_scores.entry(file.clone()).or_insert(0);
            *score += 5; // Boost for recently edited
        }

        // Add recently viewed files with a smaller boost
        for file in &self.recently_viewed_files {
            let score = activity_scores.entry(file.clone()).or_insert(0);
            *score += 2; // Smaller boost for recently viewed
        }

        // Sort by score and return top files
        let mut files: Vec<(String, usize)> = activity_scores.into_iter().collect();
        files.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by score in descending order

        files
            .into_iter()
            .take(limit)
            .map(|(file, _)| file)
            .collect()
    }

    /// Get recently modified files within a specific timeframe
    pub fn get_recently_modified_files(&self, max_age: Duration, limit: usize) -> Vec<String> {
        let now = SystemTime::now();

        let mut recent_files: Vec<(String, SystemTime)> = self
            .file_modified_times
            .iter()
            .filter_map(|(file, time)| {
                if let Ok(duration) = now.duration_since(*time) {
                    if duration <= max_age {
                        return Some((file.clone(), *time));
                    }
                }
                None
            })
            .collect();

        // Sort by modification time (most recent first)
        recent_files.sort_by(|a, b| b.1.cmp(&a.1));

        recent_files
            .into_iter()
            .take(limit)
            .map(|(file, _)| file)
            .collect()
    }
}

/// Helper function to collect files recursively using parallel processing
/// Returns a vector of files
fn collect_files_par(
    dir: &Path,
    exclude_patterns: Option<&[&str]>,
) -> std::io::Result<Vec<PathBuf>> {
    // Handle non-directories
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    // Get directory entries
    let entries: Vec<_> = match fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(Result::ok).collect(),
        Err(e) => return Err(e),
    };

    // Collect files and directories
    let mut files = Vec::new();
    let mut dirs = Vec::new();

    for entry in entries {
        let path = entry.path();

        // Skip paths matching exclude patterns
        if let Some(patterns) = exclude_patterns {
            let path_str = path.to_string_lossy();
            if patterns.iter().any(|pattern| path_str.contains(*pattern)) {
                continue;
            }
        }

        if path.is_file() {
            files.push(path);
        } else if path.is_dir() {
            // Skip common directories to ignore
            let dir_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            if [
                "node_modules",
                "target",
                "venv",
                "dist",
                ".git",
                "__pycache__",
            ]
            .contains(&dir_name.as_str())
            {
                continue;
            }

            dirs.push(path);
        }
    }

    // If we have many subdirectories, process them in parallel
    if dirs.len() > 10 {
        debug!(
            "Using parallel processing for {} subdirectories in {}",
            dirs.len(),
            dir.display()
        );

        let exclude_patterns_owned = exclude_patterns
            .map(|patterns| patterns.iter().map(|&s| s.to_owned()).collect::<Vec<_>>());

        // Process subdirectories in parallel
        let subdir_files: Vec<PathBuf> = dirs
            .into_par_iter()
            .flat_map(|subdir| {
                let exclude_patterns_refs = exclude_patterns_owned
                    .as_ref()
                    .map(|patterns| patterns.iter().map(|s| s.as_str()).collect::<Vec<_>>());

                collect_files_par(&subdir, exclude_patterns_refs.as_deref()).unwrap_or_default()
            })
            .collect();

        // Add subdirectory files
        files.extend(subdir_files);
    } else {
        // Process directories sequentially
        for subdir in dirs {
            match collect_files_par(&subdir, exclude_patterns) {
                Ok(sub_files) => files.extend(sub_files),
                Err(e) => return Err(e),
            }
        }
    }

    Ok(files)
}

/// Helper function to collect files recursively from a directory
fn collect_files_recursively(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    exclude_patterns: Option<&[&str]>,
) -> std::io::Result<()> {
    // Simply use our parallel implementation and extend the files vector
    let collected = collect_files_par(dir, exclude_patterns)?;
    files.extend(collected);
    Ok(())
}

/// Process files in parallel using a provided operation function
///
/// # Arguments
///
/// * `files` - List of files to process
/// * `threshold` - Minimum number of files to use parallel processing
/// * `operation` - Function that processes each file and returns a result
///
/// # Returns
///
/// Vector of results from processing each file
pub fn process_files_parallel<T, F>(files: &[PathBuf], threshold: usize, operation: F) -> Vec<T>
where
    T: Send + 'static,
    F: Fn(&Path) -> T + Send + Sync + 'static,
{
    if files.len() >= threshold {
        debug!("Using parallel processing for {} files", files.len());
        files.par_iter().map(|path| operation(path)).collect()
    } else {
        // Use sequential processing for small batches
        files.iter().map(|path| operation(path)).collect()
    }
}

/// Calculate dynamic file limit based on repository size
pub fn calculate_dynamic_file_limit(total_files: usize) -> usize {
    let min_files = 50;
    let max_files = 400;

    if total_files < 100 {
        // For very small repos, include most files
        min_files.min(total_files)
    } else if total_files < 1000 {
        // For small repos, scale linearly from 50 to 150
        min_files + (total_files - 100) * 100 / 900
    } else if total_files < 10000 {
        // For medium repos, scale from 150 to 300
        150 + (total_files - 1000) * 150 / 9000
    } else {
        // For large repos, cap at max_files
        max_files
    }
}

/// Get active files from workspace statistics
pub fn get_active_files_from_stats(
    workspace_stats: &WorkspaceStats,
    limit: Option<usize>,
) -> Vec<String> {
    let limit = limit.unwrap_or_else(|| {
        let total_files = workspace_stats.file_modified_times.len();
        calculate_dynamic_file_limit(total_files)
    });

    // Get recently modified files (last 3 days)
    let three_days = Duration::from_secs(3 * 24 * 60 * 60);
    let mut recent_files = workspace_stats.get_recently_modified_files(three_days, limit);

    // If we have less than the limit, add most active files
    if recent_files.len() < limit {
        let active_files = workspace_stats.get_most_active_files(limit - recent_files.len());

        // Avoid duplicates
        for file in active_files {
            if !recent_files.contains(&file) {
                recent_files.push(file);
            }

            if recent_files.len() >= limit {
                break;
            }
        }
    }

    recent_files
}

/// Get most relevant files from the repository using path scoring
/// with parallel processing for improved performance
pub fn get_relevant_files(
    workspace_path: &Path,
    limit: Option<usize>,
    workspace_stats: Option<&WorkspaceStats>,
) -> std::io::Result<Vec<String>> {
    let exclude_patterns = &[
        "node_modules",
        ".git",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".next",
        ".vscode",
        ".idea",
    ];

    // Use our parallel file collection function
    let files = collect_files_par(workspace_path, Some(exclude_patterns))?;

    // Convert PathBufs to relative path Strings in parallel for large repositories
    let file_paths: Vec<String> = if files.len() > 1000 {
        debug!(
            "Using parallel processing for path conversion: {} files",
            files.len()
        );
        files
            .par_iter()
            .filter_map(|path| {
                path.strip_prefix(workspace_path)
                    .ok()
                    .map(|rel_path| rel_path.to_string_lossy().to_string())
            })
            .collect()
    } else {
        files
            .iter()
            .filter_map(|path| {
                path.strip_prefix(workspace_path)
                    .ok()
                    .map(|rel_path| rel_path.to_string_lossy().to_string())
            })
            .collect()
    };

    // Calculate dynamic limit based on repository size
    let limit = limit.unwrap_or_else(|| calculate_dynamic_file_limit(file_paths.len()));

    // Log information for large repositories
    if file_paths.len() > 1000 {
        info!(
            "Analyzing {} files in workspace: {}",
            file_paths.len(),
            workspace_path.display()
        );
    }

    // If workspace stats are available, use them to enhance path scoring
    if let Some(stats) = workspace_stats {
        // Create a context-aware path scorer using recent activity from workspace stats
        let recent_files = stats.get_most_active_files(50);

        // Get recently modified files (last 3 days) to include in context
        let three_days = Duration::from_secs(3 * 24 * 60 * 60);
        let recent_modified = stats.get_recently_modified_files(three_days, 50);

        // Combine all recent files for context
        let mut context_files = recent_files.clone();
        for file in recent_modified {
            if !context_files.contains(&file) {
                context_files.push(file);
            }
        }

        // Create context-aware scorer
        if let Ok(mut path_scorer) = path_analyzer::create_default_path_scorer() {
            // Extract context tokens from recent files
            path_scorer.extract_context_from_files(&context_files, None);

            // Create custom extension weights (optional - using defaults)
            let scored_paths = path_scorer.calculate_path_probabilities_batch(&file_paths);

            let relevant_paths: Vec<String> = scored_paths
                .into_iter()
                .take(limit)
                .map(|(_, path)| path)
                .collect();

            return Ok(relevant_paths);
        }
    }

    // Fallback to regular path scorer if workspace stats are not available
    if let Ok(path_scorer) = path_analyzer::create_default_path_scorer() {
        // The calculate_path_probabilities_batch already uses parallel processing internally
        let scored_paths = path_scorer.calculate_path_probabilities_batch(&file_paths);

        let relevant_paths: Vec<String> = scored_paths
            .into_iter()
            .take(limit)
            .map(|(_, path)| path)
            .collect();

        return Ok(relevant_paths);
    }

    // Fallback to using workspace stats if available
    if let Some(stats) = workspace_stats {
        return Ok(get_active_files_from_stats(stats, Some(limit)));
    }

    // If all else fails, just return the files sorted alphabetically
    // Sort in parallel for large collections
    let mut sorted_paths = file_paths;
    if sorted_paths.len() > 10000 {
        // Use parallel sort for very large collections
        sorted_paths.par_sort();
    } else {
        sorted_paths.sort();
    }

    Ok(sorted_paths.into_iter().take(limit).collect())
}

/// Create a repository context summary with most relevant files
pub fn get_repo_summary(
    workspace_path: &Path,
    workspace_stats: Option<&WorkspaceStats>,
) -> Result<String, std::io::Error> {
    let mut output = Vec::new();

    // Add basic repository information
    writeln!(output, "# Repository Summary")?;
    writeln!(output, "Workspace path: {}", workspace_path.display())?;

    // Try to get Git information
    if let Some(git_info) = get_git_info(workspace_path) {
        writeln!(output)?;
        writeln!(output, "## Git Information")?;
        writeln!(output, "{}", git_info)?;
    }

    // Get most relevant files
    writeln!(output)?;
    writeln!(output, "## Most Relevant Files")?;

    let relevant_files = get_relevant_files(workspace_path, Some(30), workspace_stats)?;

    if relevant_files.is_empty() {
        writeln!(output, "No relevant files found.")?;
    } else {
        // If we have a path scorer, try to group by relevance
        if let Ok(mut path_scorer) = path_analyzer::create_default_path_scorer() {
            // Use workspace stats if available
            if let Some(stats) = workspace_stats {
                let recent_files = stats.get_most_active_files(20);
                path_scorer.extract_context_from_files(&recent_files, None);
            }

            // Group files by relevance level
            let grouped = path_scorer.group_by_relevance(&relevant_files);

            // Display high relevance files
            if !grouped.high.is_empty() {
                writeln!(output, "### High Relevance")?;
                for (i, file) in grouped.high.iter().enumerate() {
                    writeln!(output, "{}. {}", i + 1, file)?;
                }
            }

            // Display medium relevance files
            if !grouped.medium.is_empty() {
                writeln!(output, "### Medium Relevance")?;
                for (i, file) in grouped.medium.iter().enumerate() {
                    writeln!(output, "{}. {}", i + 1, file)?;
                }
            }

            // Display low relevance files
            if !grouped.low.is_empty() {
                writeln!(output, "### Low Relevance")?;
                for (i, file) in grouped.low.iter().enumerate() {
                    writeln!(output, "{}. {}", i + 1, file)?;
                }
            }
        } else {
            // Fall back to flat list if grouping isn't available
            for (i, file) in relevant_files.iter().enumerate() {
                writeln!(output, "{}. {}", i + 1, file)?;
            }
        }
    }

    // Add file type statistics
    writeln!(output)?;
    writeln!(output, "## File Type Statistics")?;

    let file_types = calculate_file_type_stats(workspace_path)?;
    let total_files: usize = file_types.values().sum();

    writeln!(output, "Total files: {}", total_files)?;

    let mut file_type_entries: Vec<_> = file_types.into_iter().collect();
    file_type_entries.sort_by(|a, b| b.1.cmp(&a.1));

    for (ext, count) in file_type_entries.iter().take(10) {
        let percentage = (*count as f64 / total_files as f64) * 100.0;
        writeln!(output, "{}: {} files ({:.1}%)", ext, count, percentage)?;
    }

    Ok(String::from_utf8_lossy(&output).to_string())
}

/// Calculate statistics on file types in the repository using parallel processing
fn calculate_file_type_stats(workspace_path: &Path) -> std::io::Result<HashMap<String, usize>> {
    let exclude_patterns = &[
        "node_modules",
        ".git",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".next",
        ".vscode",
        ".idea",
    ];

    // Use our parallel file collection function
    let files = collect_files_par(workspace_path, Some(exclude_patterns))?;

    // Process files based on count
    if files.len() > 1000 {
        debug!(
            "Using parallel processing for file type analysis: {} files",
            files.len()
        );

        // Process in parallel using rayon's fold/reduce pattern for lock-free parallelism
        let file_types = files
            .par_iter()
            .fold(
                HashMap::new, // Create a HashMap for each thread
                |mut thread_map, path| {
                    // Process each file in the thread's chunk
                    let extension = path
                        .extension()
                        .map(|ext| ext.to_string_lossy().to_string())
                        .unwrap_or_else(|| "no_extension".to_string());

                    *thread_map.entry(extension).or_insert(0) += 1;
                    thread_map
                },
            )
            .reduce(
                HashMap::new, // Identity value
                |mut result_map, thread_map| {
                    // Merge thread results
                    for (ext, count) in thread_map.into_iter() {
                        *result_map.entry(ext).or_insert(0) += count;
                    }
                    result_map
                },
            );

        Ok(file_types)
    } else {
        // Sequential processing for smaller repositories
        let mut file_types = HashMap::new();

        for path in files {
            let extension = path
                .extension()
                .map(|ext| ext.to_string_lossy().to_string())
                .unwrap_or_else(|| "no_extension".to_string());

            *file_types.entry(extension).or_insert(0) += 1;
        }

        Ok(file_types)
    }
}
