use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::core::state::SharedState;
use crate::core::types::{FileWriteOrEdit as FileWriteOrEditType, ReadFiles as ReadFilesType};
use crate::utils::fs as fs_utils;
use serde_json::from_str;

/// Read files and return their contents
pub async fn read_files_internal(
    state: &SharedState,
    file_paths: &[String],
) -> Result<Vec<(String, String)>> {
    debug!("Reading files: {:?}", file_paths);

    let mut results = Vec::new();

    for file_path in file_paths {
        // Check permissions and resolve path in a separate scope
        let path = {
            let state_guard = state.lock().unwrap();

            let resolved_path = if Path::new(file_path).is_absolute() {
                PathBuf::from(file_path)
            } else {
                state_guard.workspace_path.join(file_path)
            };

            if !state_guard.is_path_allowed(&resolved_path) {
                return Err(anyhow::anyhow!("Path not allowed: {}", resolved_path.display()));
            }

            resolved_path
        };

        // Now read the file without holding the mutex
        match fs_utils::read_file(&path).await {
            Ok(content) => {
                results.push((file_path.clone(), content));
            }
            Err(e) => {
                debug!("Failed to read file {}: {}", path.display(), e);
                results.push((file_path.clone(), format!("ERROR: {}", e)));
            }
        }
    }

    info!("Read {} files", results.len());
    Ok(results)
}

/// Write or edit a file
pub async fn write_or_edit_file_internal(
    state: &SharedState,
    file_path: &str,
    percentage_to_change: u8,
    content: &str,
) -> Result<String> {
    debug!("Writing/editing file: {}", file_path);

    // Check permissions and resolve path in a separate scope
    let path = {
        let state_guard = state.lock().unwrap();

        let resolved_path = if Path::new(file_path).is_absolute() {
            PathBuf::from(file_path)
        } else {
            state_guard.workspace_path.join(file_path)
        };

        if !state_guard.is_path_allowed(&resolved_path) {
            return Err(anyhow::anyhow!("Path not allowed: {}", resolved_path.display()));
        }

        resolved_path
    };

    // Determine if this is a full replacement or partial edit
    let mode = if percentage_to_change > 50 {
        // Full content replacement
        debug!("Replacing full file content: {}", path.display());
        fs::write(&path, content)?;
        "replaced"
    } else {
        // Parse search/replace blocks and apply them
        debug!(
            "Performing partial edit with search/replace blocks: {}",
            path.display()
        );

        // Read the current content if the file exists
        let _current_content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // If file doesn't exist, create it with the full content
                fs::write(&path, content)?;
                return Ok(format!("Created new file: {}", path.display()));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to read file {}: {}",
                    path.display(),
                    e
                ))
            }
        };

        // Here we would parse the search/replace blocks and apply them
        // For now, just do a full replacement since we haven't implemented the parser
        fs::write(&path, content)?;
        "edited"
    };

    info!("Successfully {} file: {}", mode, path.display());
    Ok(format!("Successfully {} file: {}", mode, path.display()))
}

/// Read files from a JSON request
pub async fn read_files(state: &SharedState, json_str: &str) -> Result<String> {
    debug!("Reading files from JSON: {}", json_str);

    // Parse the JSON request
    let request: ReadFilesType = from_str(json_str)?;

    // Read the files
    let results = read_files_internal(state, &request.file_paths).await?;

    // Format the results
    let mut output = String::new();
    for (path, content) in results {
        output.push_str(&format!("\n## File: {}\n```\n{}\n```\n", path, content));
    }

    Ok(output)
}

/// Write or edit a file from a JSON request
pub async fn write_or_edit_file(state: &SharedState, json_str: &str) -> Result<String> {
    debug!("Writing/editing file from JSON: {}", json_str);

    // Parse the JSON request
    let request: FileWriteOrEditType = from_str(json_str)?;

    // Write or edit the file
    write_or_edit_file_internal(
        state,
        &request.file_path,
        request.percentage_to_change,
        &request.file_content_or_search_replace_blocks,
    )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{state::create_shared_state, types::ModeType};
    use std::fs;
    use tempfile::tempdir;
    use tokio::runtime::Runtime;

    #[test]
    fn test_file_operations() {
        let rt = Runtime::new().unwrap();

        rt.block_on(async {
            let temp_dir = tempdir().unwrap();
            let state = create_shared_state(temp_dir.path(), ModeType::Wcgw, None, None).unwrap();

            // Create a test file
            let file_path = temp_dir.path().join("test.txt");
            fs::write(&file_path, "Hello, world!").unwrap();

            // Test reading
            let results = read_files(&state, &[file_path.to_string_lossy().to_string()])
                .await
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].1, "Hello, world!");

            // Test writing
            let result = write_or_edit_file(
                &state,
                &file_path.to_string_lossy().to_string(),
                100,
                "Hello, universe!",
            )
                .await
                .unwrap();

            assert!(result.contains("Successfully"));

            // Verify the file was updated
            let content = fs::read_to_string(&file_path).unwrap();
            assert_eq!(content, "Hello, universe!");
        });
    }
}
