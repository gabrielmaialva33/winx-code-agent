//! Alignment file handling for WCGW-style initialization
//!
//! This module provides functions for reading alignment files (CLAUDE.md, AGENTS.md)
//! from both global (~/.wcgw/) and workspace directories, matching wcgw Python behavior.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::utils::path::expand_user;

/// Check if ripgrep (rg) is available in the system
/// Matches wcgw Python: subprocess.run(["which", "rg"], ...)
pub fn check_ripgrep_available() -> bool {
    Command::new("which").arg("rg").output().map(|output| output.status.success()).unwrap_or(false)
}

/// Read alignment file content (CLAUDE.md or AGENTS.md)
/// Returns the content if file exists, None otherwise
/// Matches wcgw Python logic for reading alignment files
pub fn read_alignment_file(base_dir: &PathBuf) -> Option<(String, String)> {
    for fname in ["CLAUDE.md", "AGENTS.md"] {
        let file_path = base_dir.join(fname);
        if file_path.exists() {
            if let Ok(content) = fs::read_to_string(&file_path) {
                return Some((fname.to_string(), content));
            }
        }
    }
    None
}

/// Read global alignment file from ~/.wcgw/
/// Matches wcgw Python: checking `global_dir` = os.path.join(expanduser("~"), ".wcgw")
pub fn read_global_alignment_file() -> Option<String> {
    let home_dir = home::home_dir()?;
    let wcgw_dir = home_dir.join(".wcgw");

    if let Some((_, content)) = read_alignment_file(&wcgw_dir) {
        Some(content)
    } else {
        None
    }
}

/// Read workspace alignment file from workspace directory
/// Matches wcgw Python: checking workspace CLAUDE.md or AGENTS.md
pub fn read_workspace_alignment_file(workspace_path: &PathBuf) -> Option<(String, String)> {
    read_alignment_file(workspace_path)
}

/// Read initial files and return their content
/// Matches wcgw Python behavior for `initial_files_to_read`
pub fn read_initial_files(
    file_paths: &[String],
    folder_to_start: &PathBuf,
) -> (String, Vec<(String, (usize, usize))>) {
    let mut content = String::new();
    let mut file_ranges: Vec<(String, (usize, usize))> = Vec::new();

    if file_paths.is_empty() {
        return (content, file_ranges);
    }

    for file_path in file_paths {
        // Expand the path
        let expanded_path = expand_user(file_path);

        // Make path absolute if relative
        let abs_path = if PathBuf::from(&expanded_path).is_absolute() {
            PathBuf::from(&expanded_path)
        } else {
            folder_to_start.join(&expanded_path)
        };

        // Try to read the file
        match fs::read_to_string(&abs_path) {
            Ok(file_content) => {
                let lines: Vec<&str> = file_content.lines().collect();
                let total_lines = lines.len();

                // Add numbered lines like wcgw Python
                let mut numbered_content = String::new();
                for (i, line) in lines.iter().enumerate() {
                    numbered_content.push_str(&format!("{} {}\n", i + 1, line));
                }

                content.push_str(&format!(
                    "<file-contents-numbered path=\"{}\">\n{}</file-contents-numbered>\n",
                    abs_path.display(),
                    numbered_content
                ));

                file_ranges.push((abs_path.to_string_lossy().to_string(), (1, total_lines)));
            }
            Err(e) => {
                content.push_str(&format!("{}: Error reading file: {}\n", abs_path.display(), e));
            }
        }
    }

    (content, file_ranges)
}

/// Load saved task context for resumption
/// Matches wcgw Python: `load_memory()`
pub fn load_task_context(task_id: &str) -> Option<(PathBuf, String, Option<String>)> {
    // Get the memory directory
    let app_dir = get_app_dir()?;
    let memory_dir = app_dir.join("memory");

    // Sanitize task_id for filename
    let safe_id = sanitize_filename(task_id);
    let memory_file = memory_dir.join(format!("{safe_id}.txt"));
    let state_file = memory_dir.join(format!("{safe_id}_bash_state.json"));

    // Check if memory file exists
    if !memory_file.exists() {
        return None;
    }

    // Read memory content
    let memory_content = fs::read_to_string(&memory_file).ok()?;

    // Try to extract project root path from memory content
    // Matches wcgw Python format: "# PROJECT ROOT = path"
    let project_root = memory_content
        .lines()
        .find(|line| line.starts_with("# PROJECT ROOT = "))
        .map(|line| {
            let path_str = line.trim_start_matches("# PROJECT ROOT = ").trim();
            // Handle shell-quoted paths
            shell_unquote(path_str)
        })
        .map_or_else(|| PathBuf::from("."), PathBuf::from);

    // Try to read bash state
    let bash_state = fs::read_to_string(&state_file).ok();

    Some((project_root, memory_content, bash_state))
}

/// Application name for data directory (must match `memory_system.rs`)
const APP_NAME: &str = "winx-code-agent";

/// Get application directory for storing data
pub fn get_app_dir() -> Option<PathBuf> {
    // Try XDG_DATA_HOME first (matches wcgw Python behavior)
    if let Ok(xdg_path) = std::env::var("XDG_DATA_HOME") {
        let app_dir = PathBuf::from(xdg_path).join(APP_NAME);
        if app_dir.exists() || fs::create_dir_all(&app_dir).is_ok() {
            return Some(app_dir);
        }
    }

    // Try HOME/.local/share (XDG default)
    if let Some(home) = home::home_dir() {
        let app_dir = home.join(".local/share").join(APP_NAME);
        if app_dir.exists() || fs::create_dir_all(&app_dir).is_ok() {
            return Some(app_dir);
        }
    }

    // Fallback to temp directory
    let temp_dir = std::env::temp_dir().join(format!("{APP_NAME}-data"));
    if temp_dir.exists() || fs::create_dir_all(&temp_dir).is_ok() {
        return Some(temp_dir);
    }

    None
}

/// Sanitize a string for use as a filename
pub fn sanitize_filename(input: &str) -> String {
    let invalid_chars = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    let mut result = input.to_string();
    for c in invalid_chars {
        result = result.replace(c, "_");
    }
    if result.len() > 50 {
        result = result[0..50].to_string();
    }
    result
}

/// Simple shell unquoting function (matches wcgw Python behavior)
fn shell_unquote(s: &str) -> String {
    let trimmed = s.trim();

    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        inner.replace("\\\"", "\"")
    } else if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        inner.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_ripgrep() {
        // Just verify the function doesn't panic
        let _ = check_ripgrep_available();
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("test"), "test");
        assert_eq!(sanitize_filename("test/file"), "test_file");
        assert_eq!(sanitize_filename("test:file"), "test_file");
        assert_eq!(sanitize_filename("test*file?name"), "test_file_name");
    }

    #[test]
    fn test_read_initial_files_empty() {
        let (content, ranges) = read_initial_files(&[], &PathBuf::from("/tmp"));
        assert!(content.is_empty());
        assert!(ranges.is_empty());
    }
}
