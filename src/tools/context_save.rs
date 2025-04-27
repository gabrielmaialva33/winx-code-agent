use glob::glob;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ContextSave;
use crate::utils::path::expand_user;

/// Handle a call to the ContextSave tool
///
/// This function processes a ContextSave request, saves context information about a task,
/// including file contents from specified globs, to a single file.
///
/// # Arguments
///
/// * `bash_state` - Shared reference to the bash state
/// * `args` - Parameters for the ContextSave operation
///
/// # Returns
///
/// A Result with the path where the context file was saved, or an error
pub async fn handle_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    args: ContextSave,
) -> Result<String> {
    // Ensure bash state is initialized
    let bash_state_guard = bash_state
        .lock()
        .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e)))?;

    let bash_state = bash_state_guard
        .as_ref()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Process the ContextSave request
    let result = save_context(bash_state, args)?;

    // Try to open the file with the default application if possible
    if let Err(e) = try_open_file(&result) {
        debug!("Failed to open the context file: {}", e);
        // This is non-fatal, just log it
    }

    Ok(result)
}

/// Save the context information to a file
///
/// # Arguments
///
/// * `bash_state` - Reference to the bash state
/// * `context` - The ContextSave parameters
///
/// # Returns
///
/// A Result with the path where the context file was saved, or an error
fn save_context(bash_state: &BashState, mut context: ContextSave) -> Result<String> {
    // Expand the project root path if provided
    if !context.project_root_path.is_empty() {
        context.project_root_path = expand_user(&context.project_root_path);
    }

    // Find all files matching the globs
    let mut relevant_files = Vec::new();
    let mut warnings = Vec::new();

    for glob_pattern in &context.relevant_file_globs {
        // Expand the glob pattern if it contains a tilde
        let expanded_glob = expand_user(glob_pattern);

        // If the glob is not absolute and we have a project root, make it relative to the project root
        let final_glob =
            if !Path::new(&expanded_glob).is_absolute() && !context.project_root_path.is_empty() {
                PathBuf::from(&context.project_root_path)
                    .join(expanded_glob)
                    .to_string_lossy()
                    .to_string()
            } else {
                expanded_glob
            };

        debug!("Processing glob pattern: {}", final_glob);

        // Use the glob crate to find matching files
        let matches = glob(&final_glob).map_err(|e| {
            WinxError::ArgumentParseError(format!("Invalid glob pattern '{}': {}", final_glob, e))
        })?;

        let mut found_files = false;
        for entry in matches {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        relevant_files.push(path);
                        found_files = true;
                        // Limit to 1000 files per glob to avoid excessive processing
                        if relevant_files.len() >= 1000 {
                            warn!("Reached limit of 1000 files for glob '{}'", final_glob);
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("Error matching glob '{}': {}", final_glob, e);
                }
            }
        }

        if !found_files {
            warnings.push(format!(
                "Warning: No files found for the glob: {}",
                glob_pattern
            ));
        }
    }

    debug!("Found {} relevant files", relevant_files.len());

    // Get the app directory for storing memory files
    let app_dir = get_app_dir_xdg()?;
    let memory_dir = app_dir.join("memory");
    fs::create_dir_all(&memory_dir).map_err(|e| WinxError::FileAccessError {
        path: memory_dir.clone(),
        message: format!("Failed to create memory directory: {}", e),
    })?;

    // Validate the task ID
    if context.id.is_empty() {
        return Err(WinxError::ArgumentParseError(
            "Task ID cannot be empty".to_string(),
        ));
    }

    // Read the content of the relevant files
    let relevant_files_data = read_files_content(&relevant_files, 10_000)?;

    // Format the memory data
    let memory_data = format_memory(&context, &relevant_files_data);

    // Save the memory file
    let memory_file_path = memory_dir.join(format!("{}.txt", context.id));
    let mut file = File::create(&memory_file_path).map_err(|e| WinxError::FileAccessError {
        path: memory_file_path.clone(),
        message: format!("Failed to create memory file: {}", e),
    })?;

    file.write_all(memory_data.as_bytes())
        .map_err(|e| WinxError::FileAccessError {
            path: memory_file_path.clone(),
            message: format!("Failed to write to memory file: {}", e),
        })?;

    // Save the bash state if available
    let state_file_path = memory_dir.join(format!("{}_bash_state.json", context.id));

    // Serialize the bash state (simplified for now)
    let bash_state_dict = serde_json::json!({
        "cwd": bash_state.cwd.to_string_lossy().to_string(),
        "workspace_root": bash_state.workspace_root.to_string_lossy().to_string(),
        "mode": match bash_state.mode {
            crate::types::Modes::Wcgw => "wcgw",
            crate::types::Modes::Architect => "architect",
            crate::types::Modes::CodeWriter => "code_writer",
        }
    });

    let state_json = serde_json::to_string_pretty(&bash_state_dict).map_err(|e| {
        WinxError::SerializationError(format!("Failed to serialize bash state: {}", e))
    })?;

    let mut state_file =
        File::create(&state_file_path).map_err(|e| WinxError::FileAccessError {
            path: state_file_path.clone(),
            message: format!("Failed to create bash state file: {}", e),
        })?;

    state_file
        .write_all(state_json.as_bytes())
        .map_err(|e| WinxError::FileAccessError {
            path: state_file_path.clone(),
            message: format!("Failed to write to bash state file: {}", e),
        })?;

    // Prepare the response message
    let memory_file_path_str = memory_file_path.to_string_lossy().to_string();
    let response = if !relevant_files.is_empty() || context.relevant_file_globs.is_empty() {
        if warnings.is_empty() {
            memory_file_path_str
        } else {
            format!(
                "{}\n\nContext file successfully saved at {}",
                warnings.join("\n"),
                memory_file_path_str
            )
        }
    } else {
        format!(
            "Error: No files found for the given globs. Context file successfully saved at \"{}\", but please fix the error.",
            memory_file_path_str
        )
    };

    Ok(response)
}

/// Format the memory data for saving
///
/// # Arguments
///
/// * `context` - The ContextSave parameters
/// * `relevant_files_data` - The content of the relevant files
///
/// # Returns
///
/// A formatted string containing the memory data
fn format_memory(context: &ContextSave, relevant_files_data: &str) -> String {
    let mut memory_data = String::new();

    // Add project root path if provided
    if !context.project_root_path.is_empty() {
        memory_data.push_str(&format!(
            "Project root path: {}\n\n",
            context.project_root_path
        ));
    }

    // Add the description
    memory_data.push_str(&context.description);
    memory_data.push_str("\n\n");

    // Add the relevant file globs
    memory_data.push_str(&format!(
        "Relevant file globs: {}\n\n",
        context.relevant_file_globs.join(", ")
    ));

    // Add the content of the relevant files
    memory_data.push_str("File contents:\n\n");
    memory_data.push_str(relevant_files_data);

    memory_data
}

/// Get the XDG data directory for the application
///
/// # Returns
///
/// A Result with the path to the app directory
fn get_app_dir_xdg() -> Result<PathBuf> {
    // Get the XDG data directory
    let xdg_data_dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("~"));
            home.join(".local/share")
        });

    // Create the app directory if it doesn't exist
    let app_dir = xdg_data_dir.join("winx");
    fs::create_dir_all(&app_dir).map_err(|e| WinxError::FileAccessError {
        path: app_dir.clone(),
        message: format!("Failed to create app directory: {}", e),
    })?;

    Ok(app_dir)
}

/// Read the content of multiple files
///
/// # Arguments
///
/// * `file_paths` - List of paths to the files to read
/// * `max_files` - Maximum number of files to read
///
/// # Returns
///
/// A Result with the content of the files, or an error
fn read_files_content(file_paths: &[PathBuf], max_files: usize) -> Result<String> {
    let mut result = String::new();

    for (i, path) in file_paths.iter().take(max_files).enumerate() {
        let file_content = fs::read_to_string(path).map_err(|e| WinxError::FileAccessError {
            path: path.clone(),
            message: format!("Failed to read file: {}", e),
        })?;

        result.push_str(&format!("--- File {}: {} ---\n", i + 1, path.display()));
        result.push_str(&file_content);
        result.push_str("\n\n");
    }

    if file_paths.len() > max_files {
        result.push_str(&format!(
            "Note: Only showing the first {} files out of {}.\n",
            max_files,
            file_paths.len()
        ));
    }

    Ok(result)
}

/// Try to open a file with the default application
///
/// # Arguments
///
/// * `file_path` - Path to the file to open
///
/// # Returns
///
/// A Result indicating success or failure
fn try_open_file(file_path: &str) -> Result<()> {
    if std::env::consts::OS != "macos" && std::env::consts::OS != "linux" {
        // Skip on unsupported platforms
        return Ok(());
    }

    // Get the command to use based on the OS
    let cmd = if std::env::consts::OS == "macos" {
        "open"
    } else {
        // Try to find which command is available on Linux
        for cmd in &["xdg-open", "gnome-open", "kde-open"] {
            let status = std::process::Command::new("which")
                .arg(cmd)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            if let Ok(status) = status {
                if status.success() {
                    // Found an available command, use it
                    let _ = std::process::Command::new(cmd)
                        .arg(file_path)
                        .spawn()
                        .map_err(|e| {
                            WinxError::CommandExecutionError(format!(
                                "Failed to spawn open command: {}",
                                e
                            ))
                        })?;

                    // We don't wait for the command to complete
                    return Ok(());
                }
            }
        }

        // If no command is available, just return success
        return Ok(());
    };

    // Try to open the file
    let _ = std::process::Command::new(cmd)
        .arg(file_path)
        .spawn()
        .map_err(|e| {
            WinxError::CommandExecutionError(format!("Failed to spawn open command: {}", e))
        })?;

    // We don't actually need to wait for the command to complete
    // Just let it run in the background
    // (This mimics the Python implementation)

    Ok(())
}
