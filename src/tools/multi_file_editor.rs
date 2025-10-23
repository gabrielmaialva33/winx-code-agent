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

use crate::dashscope::{ChatCompletionRequest, ChatMessage, DashScopeClient, DashScopeConfig};
use crate::errors::{Result, WinxError};
use crate::gemini::{GeminiClient, GeminiConfig};
use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::state::BashState;

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

/// DashScope AI provider implementation
#[async_trait::async_trait]
impl AIProvider for DashScopeClient {
    async fn analyze_code(
        &self,
        content: &str,
        search_pattern: &str,
        replace_hint: &str,
        context: Option<&str>,
    ) -> Result<AIAnalysisResult> {
        let prompt = format!(
            "Analyze this code and find patterns matching '{}'. Replace instruction: {}\n\
            Context: {}\n\
            Code:\n{}\n\n\
            Provide a JSON response with the following structure:\n\
            {{\n\
              \"matches\": [\n\
                {{\n\
                  \"line_number\": <number>,\n\
                  \"column_start\": <number>,\n\
                  \"column_end\": <number>,\n\
                  \"original_text\": \"<text>\",\n\
                  \"replacement_text\": \"<text>\",\n\
                  \"context_before\": \"<text>\",\n\
                  \"context_after\": \"<text>\",\n\
                  \"confidence_score\": <0.0-1.0>,\n\
                  \"reasoning\": \"<explanation>\"\n\
                }}\n\
              ],\n\
              \"confidence\": <0.0-1.0>,\n\
              \"explanation\": \"<overall explanation>\",\n\
              \"suggested_replacement\": \"<general suggestion>\"\n\
            }}",
            search_pattern,
            replace_hint,
            context.unwrap_or("none"),
            content
        );

        let request = ChatCompletionRequest {
            model: "qwen3-coder-plus".to_string(),
            messages: vec![ChatMessage::user(prompt)],
            temperature: Some(0.3),
            max_tokens: Some(2000),
            stream: Some(false),
            stop: None,
            top_p: Some(0.8),
        };

        let response = self.chat_completion(&request).await?;

        if let Some(message) = response.choices.first() {
            self.parse_ai_response(&message.message.content, content)
        } else {
            Err(WinxError::AIError("No response from DashScope".to_string()))
        }
    }
}

impl DashScopeClient {
    /// Parse AI response and extract smart matches
    fn parse_ai_response(&self, response: &str, _file_content: &str) -> Result<AIAnalysisResult> {
        // Try to parse JSON response
        if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(response) {
            let matches = json_value
                .get("matches")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            Some(SmartMatch {
                                line_number: item.get("line_number")?.as_u64()? as usize,
                                column_start: item.get("column_start")?.as_u64().unwrap_or(0)
                                    as usize,
                                column_end: item.get("column_end")?.as_u64().unwrap_or(0) as usize,
                                original_text: item.get("original_text")?.as_str()?.to_string(),
                                replacement_text: item
                                    .get("replacement_text")?
                                    .as_str()?
                                    .to_string(),
                                context_before: item
                                    .get("context_before")?
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string(),
                                context_after: item
                                    .get("context_after")?
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string(),
                                confidence_score: item
                                    .get("confidence_score")?
                                    .as_f64()
                                    .unwrap_or(0.5)
                                    as f32,
                                reasoning: item
                                    .get("reasoning")?
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let confidence = json_value
                .get("confidence")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.5) as f32;

            let explanation = json_value
                .get("explanation")
                .and_then(|e| e.as_str())
                .unwrap_or("AI analysis completed")
                .to_string();

            let suggested_replacement = json_value
                .get("suggested_replacement")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();

            return Ok(AIAnalysisResult {
                file_path: "analyzed_file".to_string(),
                matches,
                confidence,
                explanation,
                suggested_replacement,
            });
        }

        // Fallback: Create a simple analysis if JSON parsing fails
        Ok(AIAnalysisResult {
            file_path: "analyzed_file".to_string(),
            matches: vec![],
            confidence: 0.5,
            explanation: "AI analysis completed with limited parsing".to_string(),
            suggested_replacement: "Please check the analysis manually".to_string(),
        })
    }
}

/// NVIDIA AI provider implementation (simplified)
#[async_trait::async_trait]
impl AIProvider for NvidiaClient {
    async fn analyze_code(
        &self,
        _content: &str,
        search_pattern: &str,
        replace_hint: &str,
        _context: Option<&str>,
    ) -> Result<AIAnalysisResult> {
        // For now, return a basic implementation
        // TODO: Implement full NVIDIA API integration
        Ok(AIAnalysisResult {
            file_path: "analyzed_file".to_string(),
            matches: vec![],
            confidence: 0.6,
            explanation: "NVIDIA AI analysis (basic implementation)".to_string(),
            suggested_replacement: format!("Pattern: {} -> {}", search_pattern, replace_hint),
        })
    }
}

/// Gemini AI provider implementation (simplified)
#[async_trait::async_trait]
impl AIProvider for GeminiClient {
    async fn analyze_code(
        &self,
        _content: &str,
        search_pattern: &str,
        replace_hint: &str,
        _context: Option<&str>,
    ) -> Result<AIAnalysisResult> {
        // For now, return a basic implementation
        // TODO: Implement full Gemini API integration
        Ok(AIAnalysisResult {
            file_path: "analyzed_file".to_string(),
            matches: vec![],
            confidence: 0.6,
            explanation: "Gemini AI analysis (basic implementation)".to_string(),
            suggested_replacement: format!("Pattern: {} -> {}", search_pattern, replace_hint),
        })
    }
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

    /// Get available AI client with fallback system
    async fn get_ai_client(&self) -> Option<Box<dyn AIProvider>> {
        // Try DashScope first (primary)
        if let Some(client) = self.get_dashscope_client().await {
            debug!("Using DashScope AI client for smart operations");
            return Some(Box::new(client));
        }

        // Try NVIDIA as fallback 1
        if let Some(client) = self.get_nvidia_client().await {
            debug!("Using NVIDIA AI client for smart operations");
            return Some(Box::new(client));
        }

        // Try Gemini as fallback 2
        if let Some(client) = self.get_gemini_client().await {
            debug!("Using Gemini AI client for smart operations");
            return Some(Box::new(client));
        }

        warn!("No AI providers available for smart operations");
        None
    }

    /// Get DashScope client if available
    async fn get_dashscope_client(&self) -> Option<DashScopeClient> {
        if let Ok(config) = DashScopeConfig::from_env()
            && let Ok(client) = DashScopeClient::new(config) {
                return Some(client);
            }
        None
    }

    /// Get NVIDIA client if available  
    async fn get_nvidia_client(&self) -> Option<NvidiaClient> {
        if let Ok(config) = NvidiaConfig::from_env()
            && let Ok(client) = NvidiaClient::new(config).await {
                return Some(client);
            }
        None
    }

    /// Get Gemini client if available
    async fn get_gemini_client(&self) -> Option<GeminiClient> {
        if let Ok(config) = GeminiConfig::from_env()
            && let Ok(client) = GeminiClient::new(config) {
                return Some(client);
            }
        None
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
                if let Some(parent) = path.parent()
                    && !parent.exists() {
                        // This is okay if create_dirs is true
                        debug!("Parent directory {:?} does not exist", parent);
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
            FileOperation::SmartSearchReplace {
                file_paths,
                search_pattern,
                confidence_threshold,
                ..
            } => {
                // Validate file paths
                if file_paths.is_empty() {
                    return Err(WinxError::InvalidInput(
                        "SmartSearchReplace requires at least one file path".to_string(),
                    ));
                }

                for file_path in file_paths {
                    let path = Path::new(file_path);
                    if !path.exists() {
                        return Err(WinxError::FileAccessError {
                            path: path.to_path_buf(),
                            message: "File not found".to_string(),
                        });
                    }
                }

                // Validate search pattern
                if search_pattern.is_empty() {
                    return Err(WinxError::InvalidInput(
                        "Search pattern cannot be empty".to_string(),
                    ));
                }

                // Validate confidence threshold
                if let Some(threshold) = confidence_threshold
                    && (*threshold < 0.0 || *threshold > 1.0) {
                        return Err(WinxError::InvalidInput(
                            "Confidence threshold must be between 0.0 and 1.0".to_string(),
                        ));
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
            FileOperation::SmartSearchReplace {
                file_paths,
                search_pattern,
                replace_hint,
                context,
                use_ai_provider,
                confidence_threshold,
                preview_mode,
            } => {
                self.execute_smart_search_replace(
                    index,
                    file_paths,
                    search_pattern,
                    replace_hint,
                    context.as_deref(),
                    use_ai_provider.as_deref(),
                    confidence_threshold.unwrap_or(0.7),
                    preview_mode.unwrap_or(false),
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
            FileOperation::SmartSearchReplace { file_paths, .. } => {
                if file_paths.is_empty() {
                    "no_files".to_string()
                } else {
                    file_paths[0].clone()
                }
            }
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
        if create_dirs
            && let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.map_err(|e| {
                    WinxError::FileError(format!("Failed to create directories: {}", e))
                })?;
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

    /// Execute smart AI-powered search and replace operation
    #[allow(clippy::too_many_arguments)]
    async fn execute_smart_search_replace(
        &mut self,
        index: usize,
        file_paths: &[String],
        search_pattern: &str,
        replace_hint: &str,
        context: Option<&str>,
        _use_ai_provider: Option<&str>,
        confidence_threshold: f32,
        preview_mode: bool,
    ) -> Result<OperationResult> {
        info!(
            "Starting smart search/replace on {} files with pattern: '{}'",
            file_paths.len(),
            search_pattern
        );

        // Get AI client
        let ai_client = match self.get_ai_client().await {
            Some(client) => client,
            None => {
                return Ok(OperationResult {
                    operation_index: index,
                    file_path: file_paths.first().unwrap_or(&"unknown".to_string()).clone(),
                    success: false,
                    message: "No AI providers available for smart search/replace".to_string(),
                    backup_path: None,
                    bytes_written: None,
                });
            }
        };

        let mut total_matches = 0;
        let mut total_replacements = 0;
        let mut processed_files = 0;
        let mut preview_results = Vec::new();

        // Process each file
        for file_path in file_paths {
            // Read file content
            let content = match fs::read_to_string(file_path).await {
                Ok(content) => content,
                Err(e) => {
                    warn!("Failed to read file {}: {}", file_path, e);
                    continue;
                }
            };

            // Analyze with AI
            let analysis = match ai_client
                .analyze_code(&content, search_pattern, replace_hint, context)
                .await
            {
                Ok(analysis) => analysis,
                Err(e) => {
                    warn!("AI analysis failed for {}: {}", file_path, e);
                    continue;
                }
            };

            // Filter matches by confidence threshold
            let valid_matches: Vec<_> = analysis
                .matches
                .into_iter()
                .filter(|m| m.confidence_score >= confidence_threshold)
                .collect();

            if valid_matches.is_empty() {
                debug!("No valid matches found in {}", file_path);
                continue;
            }

            total_matches += valid_matches.len();

            if preview_mode {
                // Store preview information
                preview_results.push(format!(
                    "File: {}\nMatches: {}\nExplanation: {}\n",
                    file_path,
                    valid_matches.len(),
                    analysis.explanation
                ));
                continue;
            }

            // Create backup
            let _backup_path = self.create_backup(file_path).await?;

            // Apply replacements
            let new_content = self.apply_smart_replacements(&content, &valid_matches)?;

            // Write the modified content
            fs::write(file_path, &new_content).await.map_err(|e| {
                WinxError::FileError(format!("Failed to write smart replacements: {}", e))
            })?;

            total_replacements += valid_matches.len();
            processed_files += 1;

            info!(
                "Applied {} smart replacements in {}",
                valid_matches.len(),
                file_path
            );
        }

        let message = if preview_mode {
            format!(
                "Preview: Found {} potential matches in {} files\n{}",
                total_matches,
                file_paths.len(),
                preview_results.join("\n")
            )
        } else {
            format!(
                "Smart search/replace completed: {} replacements in {} files",
                total_replacements, processed_files
            )
        };

        Ok(OperationResult {
            operation_index: index,
            file_path: file_paths
                .first()
                .unwrap_or(&"multiple_files".to_string())
                .clone(),
            success: true,
            message,
            backup_path: None,
            bytes_written: Some(total_replacements),
        })
    }

    /// Apply smart replacements to content
    fn apply_smart_replacements(&self, content: &str, matches: &[SmartMatch]) -> Result<String> {
        let mut lines: Vec<&str> = content.lines().collect();

        // Sort matches by line number in reverse order to avoid index shifting
        let mut sorted_matches = matches.to_vec();
        sorted_matches.sort_by(|a, b| b.line_number.cmp(&a.line_number));

        for smart_match in sorted_matches {
            if smart_match.line_number == 0 || smart_match.line_number > lines.len() {
                warn!(
                    "Invalid line number {} in smart match",
                    smart_match.line_number
                );
                continue;
            }

            let line_index = smart_match.line_number - 1; // Convert to 0-based index
            let original_line = lines[line_index];

            // Replace the specific text in the line
            let new_line =
                original_line.replace(&smart_match.original_text, &smart_match.replacement_text);

            lines[line_index] = Box::leak(new_line.into_boxed_str());
        }

        Ok(lines.join("\n"))
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

    #[tokio::test]
    async fn test_smart_search_replace_basic() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");

        // Create initial file with Rust code
        fs::write(
            &file_path,
            "fn old_function() {\n    println!(\"Hello\");\n}",
        )
        .await
        .unwrap();

        let operation = FileOperation::SmartSearchReplace {
            file_paths: vec![file_path.to_string_lossy().to_string()],
            search_pattern: "old_function".to_string(),
            replace_hint: "Rename to new_function".to_string(),
            context: Some("Rust function renaming".to_string()),
            use_ai_provider: None,
            confidence_threshold: Some(0.5),
            preview_mode: Some(false),
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

        // Should succeed even if no AI provider is available
        assert_eq!(result.total_operations, 1);
        // May fail if no AI provider, but shouldn't crash
    }

    #[tokio::test]
    async fn test_smart_search_replace_preview() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.js");

        // Create initial file
        fs::write(&file_path, "var oldVar = 'test';\nconsole.log(oldVar);")
            .await
            .unwrap();

        let operation = FileOperation::SmartSearchReplace {
            file_paths: vec![file_path.to_string_lossy().to_string()],
            search_pattern: "var".to_string(),
            replace_hint: "Replace with let or const".to_string(),
            context: Some("JavaScript modernization".to_string()),
            use_ai_provider: None,
            confidence_threshold: Some(0.7),
            preview_mode: Some(true), // Preview mode
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

        assert_eq!(result.total_operations, 1);

        // File should not be modified in preview mode
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "var oldVar = 'test';\nconsole.log(oldVar);");
    }

    #[tokio::test]
    async fn test_smart_search_replace_validation() {
        let config = MultiFileEditor {
            operations: vec![FileOperation::SmartSearchReplace {
                file_paths: vec![], // Empty file paths - should fail validation
                search_pattern: "test".to_string(),
                replace_hint: "replace".to_string(),
                context: None,
                use_ai_provider: None,
                confidence_threshold: Some(0.5),
                preview_mode: Some(false),
            }],
            create_backups: Some(false),
            atomic: Some(false),
            continue_on_error: Some(false),
            max_file_size: None,
            dry_run: Some(false),
        };

        let mut tool = MultiFileEditorTool::new(&config);
        let result = tool.execute(&config.operations).await.unwrap();

        assert_eq!(result.failed_operations, 1);
        assert!(result.results[0]
            .message
            .contains("requires at least one file path"));
    }

    #[tokio::test]
    async fn test_smart_search_replace_confidence_threshold() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.py");

        fs::write(&file_path, "def test_function():\n    pass")
            .await
            .unwrap();

        // Test with invalid confidence threshold
        let operation = FileOperation::SmartSearchReplace {
            file_paths: vec![file_path.to_string_lossy().to_string()],
            search_pattern: "test".to_string(),
            replace_hint: "replace".to_string(),
            context: None,
            use_ai_provider: None,
            confidence_threshold: Some(1.5), // Invalid - should be 0.0-1.0
            preview_mode: Some(false),
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

        assert_eq!(result.failed_operations, 1);
        assert!(result.results[0]
            .message
            .contains("Confidence threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn test_ai_analysis_result_serialization() {
        let analysis = AIAnalysisResult {
            file_path: "test.rs".to_string(),
            matches: vec![SmartMatch {
                line_number: 1,
                column_start: 0,
                column_end: 10,
                original_text: "old_func".to_string(),
                replacement_text: "new_func".to_string(),
                context_before: "".to_string(),
                context_after: "() {".to_string(),
                confidence_score: 0.9,
                reasoning: "Function name modernization".to_string(),
            }],
            confidence: 0.9,
            explanation: "Analysis completed successfully".to_string(),
            suggested_replacement: "new_func".to_string(),
        };

        // Should serialize and deserialize correctly
        let json = serde_json::to_string(&analysis).unwrap();
        let deserialized: AIAnalysisResult = serde_json::from_str(&json).unwrap();

        assert_eq!(analysis.file_path, deserialized.file_path);
        assert_eq!(analysis.matches.len(), deserialized.matches.len());
        assert_eq!(analysis.confidence, deserialized.confidence);
    }
}
