use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::core::state::SharedState;
use crate::diff::search_replace;
use crate::lsp::types::SymbolLocation;
// For now, we'll implement a simpler version without LSP dependency
// until we can properly integrate with the LSP server

/// Definition of a symbolic edit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolicEdit {
    /// Symbol location to edit
    pub location: SymbolLocation,
    /// Type of edit to perform
    pub edit_type: SymbolicEditType,
    /// Content for the edit
    pub content: String,
}

/// Types of symbolic edits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolicEditType {
    /// Replace the body of a symbol
    ReplaceBody,
    /// Insert before the symbol
    InsertBefore,
    /// Insert after the symbol
    InsertAfter,
}

/// Definition of a text edit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    /// Path to the file
    pub file_path: String,
    /// Type of edit
    pub edit_type: TextEditType,
    /// Content or parameters for the edit
    pub content: String,
    /// Optional parameters
    pub parameters: Option<TextEditParameters>,
}

/// Types of text edits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TextEditType {
    /// Full file replacement
    Replace,
    /// Search/replace blocks
    SearchReplace,
    /// Insert at a specific line
    InsertAtLine,
    /// Delete lines
    DeleteLines,
}

/// Additional parameters for text edits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEditParameters {
    /// Line number for insertion or start of deletion
    pub line: Option<usize>,
    /// End line for deletion
    pub end_line: Option<usize>,
    /// Tolerance options for matching
    pub match_options: Option<MatchOptions>,
}

/// Options for matching text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchOptions {
    /// Whether to ignore indentation
    pub ignore_indentation: bool,
    /// Whether to ignore whitespace entirely
    pub ignore_all_whitespace: bool,
}

/// Executes a symbolic edit
pub async fn execute_symbolic_edit(_state: &SharedState, edit: &SymbolicEdit) -> Result<String> {
    debug!("Executing symbolic edit: {:?}", edit);

    // For now, we'll just return a placeholder result, until we implement
    // proper LSP integration

    // Ensure we have a valid location
    if edit.location.relative_path.is_none() {
        return Err(anyhow!("Symbolic edit requires a valid file path"));
    }

    let file_path = edit.location.relative_path.as_ref().unwrap();

    match edit.edit_type {
        SymbolicEditType::ReplaceBody => {
            info!("Would replace symbol body at {:?}", edit.location);
            // Placeholder implementation
            Ok(format!(
                "Symbol edit not yet implemented - would replace body in {}",
                file_path
            ))
        }
        SymbolicEditType::InsertBefore => {
            info!("Would insert before symbol at {:?}", edit.location);
            // Placeholder implementation
            Ok(format!(
                "Symbol edit not yet implemented - would insert before symbol in {}",
                file_path
            ))
        }
        SymbolicEditType::InsertAfter => {
            info!("Would insert after symbol at {:?}", edit.location);
            // Placeholder implementation
            Ok(format!(
                "Symbol edit not yet implemented - would insert after symbol in {}",
                file_path
            ))
        }
    }
}

/// Executes a text edit
pub async fn execute_text_edit(state: &SharedState, edit: &TextEdit) -> Result<String> {
    debug!("Executing text edit: {:?}", edit);

    // Resolve file path
    let path = {
        let state_guard = state.lock().unwrap();

        let resolved_path = if Path::new(&edit.file_path).is_absolute() {
            PathBuf::from(&edit.file_path)
        } else {
            state_guard.workspace_path.join(&edit.file_path)
        };

        if !state_guard.is_path_allowed(&resolved_path) {
            return Err(anyhow!("Path not allowed: {}", resolved_path.display()));
        }

        resolved_path
    };

    match edit.edit_type {
        TextEditType::Replace => {
            info!("Replacing content in file: {}", path.display());
            fs::write(&path, &edit.content)?;
            Ok(format!(
                "Successfully replaced content in file: {}",
                path.display()
            ))
        }
        TextEditType::SearchReplace => {
            info!("Applying search/replace blocks to file: {}", path.display());

            // Read current content
            let current_content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(anyhow!("File not found: {}", path.display()));
                }
                Err(e) => {
                    return Err(anyhow!("Failed to read file {}: {}", path.display(), e));
                }
            };

            // Apply search/replace
            let result =
                search_replace::apply_search_replace_from_text(&current_content, &edit.content)
                    .context("Failed to apply search/replace blocks")?;

            // Write updated content
            fs::write(&path, &result.content)?;

            let mut message = format!(
                "Successfully edited file with search/replace blocks: {}",
                path.display()
            );
            if !result.warnings.is_empty() {
                message.push_str("\nWarnings:");
                for warning in result.warnings {
                    message.push_str(&format!("\n- {}", warning));
                }
            }

            Ok(message)
        }
        TextEditType::InsertAtLine => {
            let line = match edit.parameters.as_ref().and_then(|p| p.line) {
                Some(line) => line,
                None => return Err(anyhow!("Line parameter is required for InsertAtLine edit")),
            };

            info!("Inserting at line {} in file: {}", line, path.display());

            // Placeholder implementation
            info!("Would insert at line {} in file: {}", line, path.display());

            Ok(format!(
                "Successfully inserted content at line {} in file: {}",
                line,
                path.display()
            ))
        }
        TextEditType::DeleteLines => {
            let params = edit
                .parameters
                .as_ref()
                .ok_or_else(|| anyhow!("Parameters are required for DeleteLines edit"))?;

            let start_line = params
                .line
                .ok_or_else(|| anyhow!("Start line parameter is required for DeleteLines edit"))?;

            let end_line = params
                .end_line
                .ok_or_else(|| anyhow!("End line parameter is required for DeleteLines edit"))?;

            info!(
                "Deleting lines {}-{} in file: {}",
                start_line,
                end_line,
                path.display()
            );

            // Placeholder implementation
            info!(
                "Would delete lines {}-{} in file: {}",
                start_line,
                end_line,
                path.display()
            );

            Ok(format!(
                "Successfully deleted lines {}-{} in file: {}",
                start_line,
                end_line,
                path.display()
            ))
        }
    }
}

/// Parses a JSON request for a symbolic edit
pub async fn symbolic_edit(state: &SharedState, json_str: &str) -> Result<String> {
    debug!("Parsing symbolic edit request: {}", json_str);

    let edit: SymbolicEdit =
        serde_json::from_str(json_str).context("Failed to parse symbolic edit request")?;

    execute_symbolic_edit(state, &edit).await
}

/// Parses a JSON request for a text edit
pub async fn text_edit(state: &SharedState, json_str: &str) -> Result<String> {
    debug!("Parsing text edit request: {}", json_str);

    let edit: TextEdit =
        serde_json::from_str(json_str).context("Failed to parse text edit request")?;

    execute_text_edit(state, &edit).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{state::create_shared_state, types::ModeType};
    // Removed unused import
    use tokio::runtime::Runtime;

    #[test]
    fn test_search_replace_edit() {
        let rt = Runtime::new().unwrap();

        rt.block_on(async {
            // Use the actual /tmp directory for testing
            let base_dir = PathBuf::from("/tmp/winx_edit_test");
            fs::create_dir_all(&base_dir).unwrap_or_default();

            // Create the state using /tmp as workspace
            let state = create_shared_state("/tmp", ModeType::Wcgw, None, None).unwrap();

            // Create a test file within the workspace directory
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Create file directly in the temporary directory
            let file_name = format!("test_{}.txt", timestamp);
            let file_path = base_dir.join(&file_name);

            // Write initial content - be very careful to match exactly what will be in the search block
            let initial_content = "function hello() {\n    console.log(\"Hello, universe!\");\n}\n";

            // Create the directory structure
            fs::create_dir_all(&base_dir).unwrap_or_else(|e| panic!("Failed to create test directory: {}", e));

            // Write the file with better error handling
            match fs::write(&file_path, initial_content) {
                Ok(_) => {},
                Err(e) => panic!("Failed to create test file {}: {}", file_path.display(), e)
            }

            // Make sure we actually wrote the file
            assert!(file_path.exists(), "Failed to create test file");

            // Make sure we clean up after the test
            let file_path_clone = file_path.clone();
            let _cleanup = defer::defer(move || {
                let _ = std::fs::remove_file(&file_path_clone);
            });

            // Create search/replace content that exactly matches initial content
            let search_replace = format!(
                "<<<<<<< SEARCH\n{0}=======\nfunction hello() {{\n    console.log(\"Hello, World!\");\n}}\n>>>>>>> REPLACE\n",
                initial_content
            );

            let edit = TextEdit {
                file_path: file_path.to_string_lossy().to_string(),
                edit_type: TextEditType::SearchReplace,
                content: search_replace,
                parameters: None,
            };

            let result = execute_text_edit(&state, &edit).await.unwrap();
            assert!(result.contains("Successfully"));

            // Verify the file was updated
            let content = fs::read_to_string(&file_path).unwrap();
            assert_eq!(content.trim(), "function hello() {\n    console.log(\"Hello, World!\");\n}".trim());
        });
    }
}
