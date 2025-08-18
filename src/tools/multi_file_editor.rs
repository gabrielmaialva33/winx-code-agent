//! Multi-file editor tool for creating and editing multiple files simultaneously
//!
//! This tool provides advanced file manipulation capabilities including:
//! - Creating multiple files in a single operation
//! - Editing multiple files with different operations (replace, append, prepend, insert)
//! - Atomic operations with rollback capability
//! - Comprehensive error handling and validation

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::errors::{Result, WinxError};
use crate::state::BashState;
use crate::dashscope::{DashScopeClient, DashScopeConfig, ChatCompletionRequest, ChatMessage};
use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::gemini::{GeminiClient, GeminiConfig};

/// Represents different types of file operations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileOperation {
    /// Create a new file with content
    Create {
        file_path: String,
        content: String,
        create_dirs: Option<bool>,
    },
    /// Replace entire file content
    Replace { file_path: String, content: String },
    /// Append content to end of file
    Append { file_path: String, content: String },
    /// Prepend content to beginning of file
    Prepend { file_path: String, content: String },
    /// Insert content at specific line number
    InsertAtLine {
        file_path: String,
        content: String,
        line_number: usize,
    },
    /// Search and replace within file
    SearchReplace {
        file_path: String,
        search: String,
        replace: String,
        all_occurrences: Option<bool>,
    },
    /// AI-powered smart search and replace with context understanding
    SmartSearchReplace {
        file_paths: Vec<String>,
        search_pattern: String,
        replace_hint: String,
        context: Option<String>,
        use_ai_provider: Option<String>,
        confidence_threshold: Option<f32>,
        preview_mode: Option<bool>,
    },
}

/// Configuration for the multi-file editor operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiFileEditor {
    /// List of file operations to perform
    pub operations: Vec<FileOperation>,
    /// Whether to create backup files before modification
    pub create_backups: Option<bool>,
    /// Whether to perform operations atomically (all or nothing)
    pub atomic: Option<bool>,
    /// Whether to continue on errors or stop at first error
    pub continue_on_error: Option<bool>,
    /// Maximum file size to process (in bytes)
    pub max_file_size: Option<usize>,
    /// Dry run mode - validate operations without executing
    pub dry_run: Option<bool>,
}

/// Result of a single file operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation_index: usize,
    pub file_path: String,
    pub success: bool,
    pub message: String,
    pub backup_path: Option<String>,
    pub bytes_written: Option<usize>,
}

/// Complete result of multi-file editor operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiFileEditorResult {
    pub total_operations: usize,
    pub successful_operations: usize,
    pub failed_operations: usize,
    pub results: Vec<OperationResult>,
    pub rollback_performed: bool,
    pub dry_run: bool,
}

/// Backup information for rollback capability
#[derive(Debug, Clone)]
struct BackupInfo {
    original_path: PathBuf,
    backup_path: Option<PathBuf>,
    was_created: bool, // True if file was created (didn't exist before)
}

/// AI analysis result for smart search and replace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIAnalysisResult {
    pub file_path: String,
    pub matches: Vec<SmartMatch>,
    pub confidence: f32,
    pub explanation: String,
    pub suggested_replacement: String,
}

/// A smart match found by AI analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMatch {
    pub line_number: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub original_text: String,
    pub replacement_text: String,
    pub context_before: String,
    pub context_after: String,
    pub confidence_score: f32,
    pub reasoning: String,
}

/// AI provider trait for abstraction
#[async_trait::async_trait]
pub trait AIProvider: Send + Sync {
    async fn analyze_code(
        &self,
        content: &str,
        search_pattern: &str,
        replace_hint: &str,
        context: Option<&str>,
    ) -> Result<AIAnalysisResult>;
}

/// Multi-file editor implementation
pub struct MultiFileEditorTool {
    create_backups: bool,
    atomic: bool,
    continue_on_error: bool,
    max_file_size: usize,
    dry_run: bool,
    backups: Vec<BackupInfo>,
}

impl Default for MultiFileEditorTool {
    fn default() -> Self {
        Self {
            create_backups: true,
            atomic: true,
            continue_on_error: false,
            max_file_size: 10 * 1024 * 1024, // 10MB default
            dry_run: false,
            backups: Vec::new(),
        }
    }
}

impl MultiFileEditorTool {
    /// Create a new multi-file editor tool with configuration
    pub fn new(config: &MultiFileEditor) -> Self {
        Self {
            create_backups: config.create_backups.unwrap_or(true),
            atomic: config.atomic.unwrap_or(true),
            continue_on_error: config.continue_on_error.unwrap_or(false),
            max_file_size: config.max_file_size.unwrap_or(10 * 1024 * 1024),
            dry_run: config.dry_run.unwrap_or(false),
            backups: Vec::new(),
        }
    }

    /// Execute all file operations
    pub async fn execute(&mut self, operations: &[FileOperation]) -> Result<MultiFileEditorResult> {
        let mut results = Vec::new();
        let mut successful_operations = 0;
        let mut failed_operations = 0;
        let mut rollback_performed = false;

        info!(
            "Starting multi-file editor with {} operations (dry_run: {})",
            operations.len(),
            self.dry_run
        );

        // Validate all operations first
        for (index, operation) in operations.iter().enumerate() {
            if let Err(e) = self.validate_operation(operation).await {
                let result = OperationResult {
                    operation_index: index,
                    file_path: self.get_operation_file_path(operation),
                    success: false,
                    message: format!("Validation failed: {}", e),
                    backup_path: None,
                    bytes_written: None,
                };
                results.push(result);
                failed_operations += 1;

                if !self.continue_on_error {
                    return Ok(MultiFileEditorResult {
                        total_operations: operations.len(),
                        successful_operations,
                        failed_operations,
                        results,
                        rollback_performed,
                        dry_run: self.dry_run,
                    });
                }
            }
        }

        // Execute operations
        for (index, operation) in operations.iter().enumerate() {
            let result = match self.execute_operation(index, operation).await {
                Ok(result) => {
                    successful_operations += 1;
                    result
                }
                Err(e) => {
                    failed_operations += 1;
                    let result = OperationResult {
                        operation_index: index,
                        file_path: self.get_operation_file_path(operation),
                        success: false,
                        message: format!("Execution failed: {}", e),
                        backup_path: None,
                        bytes_written: None,
                    };

                    // If atomic mode and we have a failure, rollback
                    if self.atomic && !self.dry_run {
                        warn!("Operation failed in atomic mode, performing rollback");
                        if let Err(rollback_err) = self.rollback().await {
                            warn!("Rollback failed: {}", rollback_err);
                        } else {
                            rollback_performed = true;
                        }
                        results.push(result);
                        break;
                    }

                    if !self.continue_on_error {
                        results.push(result);
                        break;
                    }

                    result
                }
            };

            results.push(result);
        }

        Ok(MultiFileEditorResult {
            total_operations: operations.len(),
            successful_operations,
            failed_operations,
            results,
            rollback_performed,
            dry_run: self.dry_run,
        })
    }

    /// Validate a single operation
    async fn validate_operation(&self, operation: &FileOperation) -> Result<()> {
        match operation {
            FileOperation::Create {
                file_path, content, ..
            } => {
                let path = Path::new(file_path);

                // Check if file already exists
                if path.exists() {
                    return Err(WinxError::FileAccessError {
                        path: path.to_path_buf(),
                        message: "File already exists".to_string(),
                    });
                }

                // Check content size
                if content.len() > self.max_file_size {
                    return Err(WinxError::FileTooLarge {
                        path: PathBuf::from(file_path),
                        size: content.len() as u64,
                        max_size: self.max_file_size as u64,
                    });
                }

                // Check if parent directory exists or can be created
                if let Some(parent) = path.parent() {
                    if !parent.exists() {
                        // This is okay if create_dirs is true
                        debug!("Parent directory {:?} does not exist", parent);
                    }
                }
            }
            FileOperation::Replace { file_path, content }
            | FileOperation::Append { file_path, content }
            | FileOperation::Prepend { file_path, content } => {
                let path = Path::new(file_path);

                // Check if file exists
                if !path.exists() {
                    return Err(WinxError::FileAccessError {
                        path: path.to_path_buf(),
                        message: "File not found".to_string(),
                    });
                }

                // Check content size
                if content.len() > self.max_file_size {
                    return Err(WinxError::FileTooLarge {
                        path: PathBuf::from(file_path),
                        size: content.len() as u64,
                        max_size: self.max_file_size as u64,
                    });
                }

                // Check file size
                let metadata = fs::metadata(path).await.map_err(|e| {
                    WinxError::FileError(format!("Failed to get file metadata: {}", e))
                })?;

                if metadata.len() as usize > self.max_file_size {
                    return Err(WinxError::FileTooLarge {
                        path: path.to_path_buf(),
                        size: metadata.len(),
                        max_size: self.max_file_size as u64,
                    });
                }
            }
            FileOperation::InsertAtLine {
                file_path,
                content,
                line_number,
            } => {
                let path = Path::new(file_path);

                if !path.exists() {
                    return Err(WinxError::FileAccessError {
                        path: path.to_path_buf(),
                        message: "File not found".to_string(),
                    });
                }

                if content.len() > self.max_file_size {
                    return Err(WinxError::FileTooLarge {
                        path: PathBuf::from(file_path),
                        size: content.len() as u64,
                        max_size: self.max_file_size as u64,
                    });
                }

                // Validate line number
                if *line_number == 0 {
                    return Err(WinxError::InvalidInput(
                        "Line number must be >= 1".to_string(),
                    ));
                }
            }
            FileOperation::SearchReplace {
                file_path,
                search,
                replace,
                ..
            } => {
                let path = Path::new(file_path);

                if !path.exists() {
                    return Err(WinxError::FileAccessError {
                        path: path.to_path_buf(),
                        message: "File not found".to_string(),
                    });
                }

                if search.is_empty() {
                    return Err(WinxError::InvalidInput(
                        "Search string cannot be empty".to_string(),
                    ));
                }

                if replace.len() > self.max_file_size {
                    return Err(WinxError::FileTooLarge {
                        path: PathBuf::from(file_path),
                        size: replace.len() as u64,
                        max_size: self.max_file_size as u64,
                    });
                }
            }
        }

        Ok(())
    }

    /// Execute a single operation
    async fn execute_operation(
        &mut self,
        index: usize,
        operation: &FileOperation,
    ) -> Result<OperationResult> {
        let file_path = self.get_operation_file_path(operation);

        if self.dry_run {
            return Ok(OperationResult {
                operation_index: index,
                file_path,
                success: true,
                message: "Dry run - operation would succeed".to_string(),
                backup_path: None,
                bytes_written: None,
            });
        }

        match operation {
            FileOperation::Create {
                file_path,
                content,
                create_dirs,
            } => {
                self.execute_create(index, file_path, content, create_dirs.unwrap_or(true))
                    .await
            }
            FileOperation::Replace { file_path, content } => {
                self.execute_replace(index, file_path, content).await
            }
            FileOperation::Append { file_path, content } => {
                self.execute_append(index, file_path, content).await
            }
            FileOperation::Prepend { file_path, content } => {
                self.execute_prepend(index, file_path, content).await
            }
            FileOperation::InsertAtLine {
                file_path,
                content,
                line_number,
            } => {
                self.execute_insert_at_line(index, file_path, content, *line_number)
                    .await
            }
            FileOperation::SearchReplace {
                file_path,
                search,
                replace,
                all_occurrences,
            } => {
                self.execute_search_replace(
                    index,
                    file_path,
                    search,
                    replace,
                    all_occurrences.unwrap_or(false),
                )
                .await
            }
        }
    }

    /// Get file path from operation
    fn get_operation_file_path(&self, operation: &FileOperation) -> String {
        match operation {
            FileOperation::Create { file_path, .. }
            | FileOperation::Replace { file_path, .. }
            | FileOperation::Append { file_path, .. }
            | FileOperation::Prepend { file_path, .. }
            | FileOperation::InsertAtLine { file_path, .. }
            | FileOperation::SearchReplace { file_path, .. } => file_path.clone(),
        }
    }

    /// Create backup of file if needed
    async fn create_backup(&mut self, file_path: &str) -> Result<Option<String>> {
        if !self.create_backups {
            return Ok(None);
        }

        let path = Path::new(file_path);
        if !path.exists() {
            // File doesn't exist, record that it was created
            self.backups.push(BackupInfo {
                original_path: path.to_path_buf(),
                backup_path: None,
                was_created: true,
            });
            return Ok(None);
        }

        // Create backup file
        let backup_path = format!("{}.backup.{}", file_path, chrono::Utc::now().timestamp());
        fs::copy(file_path, &backup_path)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to create backup: {}", e)))?;

        self.backups.push(BackupInfo {
            original_path: path.to_path_buf(),
            backup_path: Some(PathBuf::from(&backup_path)),
            was_created: false,
        });

        Ok(Some(backup_path))
    }

    /// Rollback all operations
    async fn rollback(&mut self) -> Result<()> {
        info!("Starting rollback of {} operations", self.backups.len());

        for backup in self.backups.iter().rev() {
            if backup.was_created {
                // File was created, so delete it
                if backup.original_path.exists() {
                    fs::remove_file(&backup.original_path).await.map_err(|e| {
                        WinxError::FileError(format!(
                            "Failed to remove created file during rollback: {}",
                            e
                        ))
                    })?;
                    debug!("Removed created file: {:?}", backup.original_path);
                }
            } else if let Some(backup_path) = &backup.backup_path {
                // File was modified, restore from backup
                fs::copy(backup_path, &backup.original_path)
                    .await
                    .map_err(|e| {
                        WinxError::FileError(format!(
                            "Failed to restore file from backup during rollback: {}",
                            e
                        ))
                    })?;
                debug!(
                    "Restored file: {:?} from {:?}",
                    backup.original_path, backup_path
                );

                // Clean up backup file
                let _ = fs::remove_file(backup_path).await;
            }
        }

        self.backups.clear();
        info!("Rollback completed successfully");
        Ok(())
    }

    /// Execute create operation
    async fn execute_create(
        &mut self,
        index: usize,
        file_path: &str,
        content: &str,
        create_dirs: bool,
    ) -> Result<OperationResult> {
        let path = Path::new(file_path);

        // Create parent directories if needed
        if create_dirs {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.map_err(|e| {
                    WinxError::FileError(format!("Failed to create directories: {}", e))
                })?;
            }
        }

        // Create backup (will record that file was created)
        let backup_path = self.create_backup(file_path).await?;

        // Write file
        fs::write(file_path, content)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to create file: {}", e)))?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: "File created successfully".to_string(),
            backup_path,
            bytes_written: Some(content.len()),
        })
    }

    /// Execute replace operation
    async fn execute_replace(
        &mut self,
        index: usize,
        file_path: &str,
        content: &str,
    ) -> Result<OperationResult> {
        let backup_path = self.create_backup(file_path).await?;

        fs::write(file_path, content)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to replace file content: {}", e)))?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: "File content replaced successfully".to_string(),
            backup_path,
            bytes_written: Some(content.len()),
        })
    }

    /// Execute append operation
    async fn execute_append(
        &mut self,
        index: usize,
        file_path: &str,
        content: &str,
    ) -> Result<OperationResult> {
        let backup_path = self.create_backup(file_path).await?;

        let mut existing_content = fs::read_to_string(file_path)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to read existing file: {}", e)))?;

        existing_content.push_str(content);

        fs::write(file_path, &existing_content)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to append to file: {}", e)))?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: "Content appended successfully".to_string(),
            backup_path,
            bytes_written: Some(content.len()),
        })
    }

    /// Execute prepend operation
    async fn execute_prepend(
        &mut self,
        index: usize,
        file_path: &str,
        content: &str,
    ) -> Result<OperationResult> {
        let backup_path = self.create_backup(file_path).await?;

        let existing_content = fs::read_to_string(file_path)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to read existing file: {}", e)))?;

        let new_content = format!("{}{}", content, existing_content);

        fs::write(file_path, &new_content)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to prepend to file: {}", e)))?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: "Content prepended successfully".to_string(),
            backup_path,
            bytes_written: Some(content.len()),
        })
    }

    /// Execute insert at line operation
    async fn execute_insert_at_line(
        &mut self,
        index: usize,
        file_path: &str,
        content: &str,
        line_number: usize,
    ) -> Result<OperationResult> {
        let backup_path = self.create_backup(file_path).await?;

        let existing_content = fs::read_to_string(file_path)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to read existing file: {}", e)))?;

        let mut lines: Vec<&str> = existing_content.lines().collect();

        // Insert at specified line (1-based indexing)
        let insert_index = if line_number > lines.len() {
            lines.len()
        } else {
            line_number - 1
        };

        lines.insert(insert_index, content);
        let new_content = lines.join("\n");

        fs::write(file_path, &new_content).await.map_err(|e| {
            WinxError::FileError(format!("Failed to insert content at line: {}", e))
        })?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: format!("Content inserted at line {} successfully", line_number),
            backup_path,
            bytes_written: Some(content.len()),
        })
    }

    /// Execute search and replace operation
    async fn execute_search_replace(
        &mut self,
        index: usize,
        file_path: &str,
        search: &str,
        replace: &str,
        all_occurrences: bool,
    ) -> Result<OperationResult> {
        let backup_path = self.create_backup(file_path).await?;

        let existing_content = fs::read_to_string(file_path)
            .await
            .map_err(|e| WinxError::FileError(format!("Failed to read existing file: {}", e)))?;

        let new_content = if all_occurrences {
            existing_content.replace(search, replace)
        } else {
            existing_content.replacen(search, replace, 1)
        };

        let replacements = if all_occurrences {
            existing_content.matches(search).count()
        } else if existing_content.contains(search) {
            1
        } else {
            0
        };

        fs::write(file_path, &new_content).await.map_err(|e| {
            WinxError::FileError(format!("Failed to write search/replace result: {}", e))
        })?;

        Ok(OperationResult {
            operation_index: index,
            file_path: file_path.to_string(),
            success: true,
            message: format!(
                "Search/replace completed: {} replacements made",
                replacements
            ),
            backup_path,
            bytes_written: Some(new_content.len()),
        })
    }
}

/// Handle multi-file editor tool call
pub async fn handle_tool_call(
    _bash_state: &Arc<Mutex<Option<BashState>>>,
    multi_file_editor: MultiFileEditor,
) -> Result<String> {
    let mut tool = MultiFileEditorTool::new(&multi_file_editor);
    let result = tool.execute(&multi_file_editor.operations).await?;

    // Format result as JSON for better readability
    let result_json = serde_json::to_string_pretty(&result)
        .map_err(|e| WinxError::SerializationError(format!("Failed to serialize result: {}", e)))?;

    Ok(result_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        let operation = FileOperation::Create {
            file_path: file_path.to_string_lossy().to_string(),
            content: "Hello, World!".to_string(),
            create_dirs: Some(true),
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);
        assert_eq!(result.failed_operations, 0);
        assert!(file_path.exists());

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_dry_run() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        let operation = FileOperation::Create {
            file_path: file_path.to_string_lossy().to_string(),
            content: "Hello, World!".to_string(),
            create_dirs: Some(true),
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(true),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);
        assert_eq!(result.failed_operations, 0);
        assert!(result.dry_run);
        assert!(!file_path.exists()); // File should not be created in dry run
    }

    #[tokio::test]
    async fn test_atomic_rollback() {
        let temp_dir = TempDir::new().unwrap();
        let file1_path = temp_dir.path().join("test1.txt");
        let file2_path = temp_dir.path().join("nonexistent/test2.txt"); // This will fail

        let operations = vec![
            FileOperation::Create {
                file_path: file1_path.to_string_lossy().to_string(),
                content: "Hello, World!".to_string(),
                create_dirs: Some(true),
            },
            FileOperation::Create {
                file_path: file2_path.to_string_lossy().to_string(),
                content: "This will fail".to_string(),
                create_dirs: Some(false), // This will cause failure
            },
        ];

        let config = MultiFileEditor {
            operations,
            create_backups: Some(true),
            atomic: Some(true),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert!(result.rollback_performed);
        assert!(!file1_path.exists()); // Should be rolled back
    }

    #[tokio::test]
    async fn test_append_operation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        fs::write(&file_path, "Initial content").await.unwrap();

        let operation = FileOperation::Append {
            file_path: file_path.to_string_lossy().to_string(),
            content: "\nAppended content".to_string(),
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Initial content\nAppended content");
    }

    #[tokio::test]
    async fn test_prepend_operation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        fs::write(&file_path, "Original content").await.unwrap();

        let operation = FileOperation::Prepend {
            file_path: file_path.to_string_lossy().to_string(),
            content: "Prepended content\n".to_string(),
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Prepended content\nOriginal content");
    }

    #[tokio::test]
    async fn test_search_replace_operation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        fs::write(&file_path, "Hello world! Hello universe!")
            .await
            .unwrap();

        let operation = FileOperation::SearchReplace {
            file_path: file_path.to_string_lossy().to_string(),
            search: "Hello".to_string(),
            replace: "Hi".to_string(),
            all_occurrences: Some(true),
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Hi world! Hi universe!");
    }

    #[tokio::test]
    async fn test_insert_at_line_operation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file with multiple lines
        fs::write(&file_path, "Line 1\nLine 2\nLine 3")
            .await
            .unwrap();

        let operation = FileOperation::InsertAtLine {
            file_path: file_path.to_string_lossy().to_string(),
            content: "Inserted line".to_string(),
            line_number: 2,
        };

        let config = MultiFileEditor {
            operations: vec![operation],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 1);

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Line 1\nInserted line\nLine 2\nLine 3");
    }

    #[tokio::test]
    async fn test_multiple_operations() {
        let temp_dir = TempDir::new().unwrap();
        let file1_path = temp_dir.path().join("test1.txt");
        let file2_path = temp_dir.path().join("test2.txt");

        let operations = vec![
            FileOperation::Create {
                file_path: file1_path.to_string_lossy().to_string(),
                content: "File 1 content".to_string(),
                create_dirs: Some(true),
            },
            FileOperation::Create {
                file_path: file2_path.to_string_lossy().to_string(),
                content: "File 2 content".to_string(),
                create_dirs: Some(true),
            },
        ];

        let config = MultiFileEditor {
            operations,
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.successful_operations, 2);
        assert_eq!(result.failed_operations, 0);
        assert!(file1_path.exists());
        assert!(file2_path.exists());

        let content1 = fs::read_to_string(&file1_path).await.unwrap();
        let content2 = fs::read_to_string(&file2_path).await.unwrap();
        assert_eq!(content1, "File 1 content");
        assert_eq!(content2, "File 2 content");
    }
}
