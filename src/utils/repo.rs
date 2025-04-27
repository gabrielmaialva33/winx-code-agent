use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
