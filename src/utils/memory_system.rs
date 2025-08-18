//! Memory and context saving system
//!
//! This module provides task memory management and context saving capabilities,
//! directly ported from WCGW's memory.py. It handles persistent storage of
//! task context, bash state, and relevant files for task resumption.

use anyhow::{Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::state::bash_state::BashState;

/// XDG data directory for storing application data
const APP_NAME: &str = "winx-code-agent";

/// Context save data structure for task resumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSave {
    /// Unique identifier for the task
    pub id: String,
    /// Human-readable description of the task
    pub description: String,
    /// Root path of the project being worked on
    pub project_root_path: Option<String>,
    /// File glob patterns that are relevant to this task
    pub relevant_file_globs: Vec<String>,
    /// Timestamp when the context was saved
    pub saved_at: String,
    /// Current working directory when saved
    pub working_directory: Option<String>,
    /// Current mode when saved
    pub mode: Option<String>,
}

/// Memory system for managing task context and resumption
#[derive(Debug)]
pub struct MemorySystem {
    app_dir: PathBuf,
    memory_dir: PathBuf,
}

impl Default for MemorySystem {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySystem {
    /// Create a new memory system with default paths
    pub fn new() -> Self {
        let app_dir = Self::get_app_dir_xdg();
        let memory_dir = app_dir.join("memory");

        Self {
            app_dir,
            memory_dir,
        }
    }

    /// Get the XDG-compliant application data directory
    fn get_app_dir_xdg() -> PathBuf {
        let xdg_data_dir = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            format!("{}/.local/share", home)
        });

        PathBuf::from(xdg_data_dir).join(APP_NAME)
    }

    /// Ensure the memory directory exists
    fn ensure_memory_dir(&self) -> Result<()> {
        if !self.memory_dir.exists() {
            debug!("Creating memory directory: {:?}", self.memory_dir);
            fs::create_dir_all(&self.memory_dir).with_context(|| {
                format!("Failed to create memory directory: {:?}", self.memory_dir)
            })?;
        }
        Ok(())
    }

    /// Format memory data for storage
    ///
    /// # Arguments
    /// * `task_memory` - The context save data
    /// * `relevant_files` - Content of relevant files
    ///
    /// # Returns
    /// Formatted memory string ready for storage
    pub fn format_memory(&self, task_memory: &ContextSave, relevant_files: &str) -> String {
        let mut memory_data = String::new();

        // Add project root if available
        if let Some(ref project_root) = task_memory.project_root_path {
            memory_data.push_str(&format!("# PROJECT ROOT = {}\n", shell_quote(project_root)));
        }

        // Add task description
        memory_data.push_str(&task_memory.description);

        // Add relevant file patterns
        memory_data.push_str("\n\n# Relevant file paths\n");
        let quoted_globs: Vec<String> = task_memory
            .relevant_file_globs
            .iter()
            .map(|glob| shell_quote(glob))
            .collect();
        memory_data.push_str(&quoted_globs.join(", "));

        // Add relevant files content
        memory_data.push_str("\n\n# Relevant Files:\n");
        memory_data.push_str(relevant_files);

        memory_data
    }

    /// Save task memory and context to persistent storage
    ///
    /// # Arguments
    /// * `task_memory` - The context save data
    /// * `relevant_files` - Content of relevant files
    /// * `bash_state` - Optional bash state to save
    ///
    /// # Returns
    /// Path to the saved memory file
    pub fn save_memory(
        &self,
        task_memory: &ContextSave,
        relevant_files: &str,
        bash_state: Option<&BashState>,
    ) -> Result<PathBuf> {
        self.ensure_memory_dir()?;

        let task_id = &task_memory.id;
        if task_id.is_empty() {
            return Err(anyhow::anyhow!("Task ID cannot be empty"));
        }

        info!("Saving memory for task: {}", task_id);

        // Format and save the main memory data
        let memory_data = self.format_memory(task_memory, relevant_files);
        let memory_file_path = self.memory_dir.join(format!("{}.txt", task_id));

        fs::write(&memory_file_path, memory_data)
            .with_context(|| format!("Failed to write memory file: {:?}", memory_file_path))?;

        debug!("Saved memory data to: {:?}", memory_file_path);

        // Save bash state if provided (simplified serialization)
        if let Some(bash_state) = bash_state {
            let state_file_path = self.memory_dir.join(format!("{}_bash_state.json", task_id));

            // Create a simplified representation of the bash state
            let state_data = serde_json::json!({
                "cwd": bash_state.cwd.to_string_lossy().to_string(),
                "workspace_root": bash_state.workspace_root.to_string_lossy().to_string(),
                "current_chat_id": bash_state.current_chat_id,
                "mode": format!("{:?}", bash_state.mode),
                "initialized": bash_state.initialized,
            });

            let state_json = serde_json::to_string_pretty(&state_data)
                .context("Failed to serialize bash state")?;

            fs::write(&state_file_path, state_json).with_context(|| {
                format!("Failed to write bash state file: {:?}", state_file_path)
            })?;

            debug!("Saved bash state to: {:?}", state_file_path);
        }

        Ok(memory_file_path)
    }

    /// Load task memory and context from persistent storage
    ///
    /// # Arguments
    /// * `task_id` - The unique task identifier
    /// * `_coding_max_tokens` - Maximum tokens for source code files (unused)
    /// * `noncoding_max_tokens` - Maximum tokens for non-source code files
    ///
    /// # Returns
    /// Tuple of (project_root_path, memory_data, bash_state_json)
    pub fn load_memory(
        &self,
        task_id: &str,
        _coding_max_tokens: Option<usize>,
        noncoding_max_tokens: Option<usize>,
    ) -> Result<(String, String, Option<serde_json::Value>)> {
        let memory_file_path = self.memory_dir.join(format!("{}.txt", task_id));

        if !memory_file_path.exists() {
            return Err(anyhow::anyhow!(
                "Memory file not found for task: {}",
                task_id
            ));
        }

        info!("Loading memory for task: {}", task_id);

        // Read memory data
        let mut data = fs::read_to_string(&memory_file_path)
            .with_context(|| format!("Failed to read memory file: {:?}", memory_file_path))?;

        // Apply token limits if specified (memory files are considered non-coding)
        if let Some(max_tokens) = noncoding_max_tokens {
            data = self.truncate_to_token_limit(&data, max_tokens);
        }

        // Extract project root path using regex
        let project_root_path = self.extract_project_root(&data);

        // Try to load bash state if it exists
        let state_file_path = self.memory_dir.join(format!("{}_bash_state.json", task_id));
        let bash_state_json = if state_file_path.exists() {
            match fs::read_to_string(&state_file_path) {
                Ok(state_json) => match serde_json::from_str::<serde_json::Value>(&state_json) {
                    Ok(state) => {
                        debug!("Successfully loaded bash state from: {:?}", state_file_path);
                        Some(state)
                    }
                    Err(e) => {
                        warn!("Failed to deserialize bash state: {}", e);
                        None
                    }
                },
                Err(e) => {
                    warn!("Failed to read bash state file: {}", e);
                    None
                }
            }
        } else {
            debug!("No bash state file found for task: {}", task_id);
            None
        };

        Ok((project_root_path, data, bash_state_json))
    }

    /// List all saved tasks
    pub fn list_saved_tasks(&self) -> Result<Vec<String>> {
        if !self.memory_dir.exists() {
            return Ok(Vec::new());
        }

        let mut task_ids = Vec::new();

        for entry in fs::read_dir(&self.memory_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    // Look for .txt files (main memory files)
                    if file_name.ends_with(".txt") && !file_name.ends_with("_bash_state.json") {
                        let task_id = file_name.strip_suffix(".txt").unwrap_or(file_name);
                        task_ids.push(task_id.to_string());
                    }
                }
            }
        }

        task_ids.sort();
        Ok(task_ids)
    }

    /// Delete saved memory for a task
    pub fn delete_memory(&self, task_id: &str) -> Result<()> {
        let memory_file_path = self.memory_dir.join(format!("{}.txt", task_id));
        let state_file_path = self.memory_dir.join(format!("{}_bash_state.json", task_id));

        info!("Deleting memory for task: {}", task_id);

        // Remove memory file
        if memory_file_path.exists() {
            fs::remove_file(&memory_file_path)
                .with_context(|| format!("Failed to remove memory file: {:?}", memory_file_path))?;
            debug!("Removed memory file: {:?}", memory_file_path);
        }

        // Remove bash state file if it exists
        if state_file_path.exists() {
            fs::remove_file(&state_file_path).with_context(|| {
                format!("Failed to remove bash state file: {:?}", state_file_path)
            })?;
            debug!("Removed bash state file: {:?}", state_file_path);
        }

        Ok(())
    }

    /// Get memory statistics
    pub fn get_memory_stats(&self) -> Result<MemoryStats> {
        if !self.memory_dir.exists() {
            return Ok(MemoryStats::default());
        }

        let mut stats = MemoryStats::default();

        for entry in fs::read_dir(&self.memory_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Ok(metadata) = entry.metadata() {
                    stats.total_files += 1;
                    stats.total_size += metadata.len();

                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if file_name.ends_with(".txt") {
                            stats.memory_files += 1;
                        } else if file_name.ends_with("_bash_state.json") {
                            stats.state_files += 1;
                        }
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Truncate text to approximate token limit
    fn truncate_to_token_limit(&self, text: &str, max_tokens: usize) -> String {
        // Rough approximation: 1 token ≈ 4 characters
        let max_chars = max_tokens * 4;

        if text.len() <= max_chars {
            return text.to_string();
        }

        // Reserve space for truncation message
        let reserve_chars = 20; // "... (truncated)"
        let available_chars = max_chars.saturating_sub(reserve_chars);

        let mut truncated = text.chars().take(available_chars).collect::<String>();
        truncated.push_str("\n(... truncated)");

        truncated
    }

    /// Extract project root path from memory data
    fn extract_project_root(&self, data: &str) -> String {
        // Look for "# PROJECT ROOT = path" pattern
        for line in data.lines() {
            if let Some(stripped) = line.strip_prefix("# PROJECT ROOT = ") {
                // Handle shell-quoted paths
                return shell_unquote(stripped.trim()).unwrap_or_default();
            }
        }

        String::new()
    }
}

/// Memory system statistics
#[derive(Debug, Default)]
pub struct MemoryStats {
    pub total_files: usize,
    pub memory_files: usize,
    pub state_files: usize,
    pub total_size: u64,
}

/// Simple shell quoting function
fn shell_quote(s: &str) -> String {
    if s.contains(' ')
        || s.contains('\t')
        || s.contains('\n')
        || s.contains('\'')
        || s.contains('"')
    {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Simple shell unquoting function
fn shell_unquote(s: &str) -> Option<String> {
    let trimmed = s.trim();

    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        Some(inner.replace("\\\"", "\""))
    } else if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        Some(inner.to_string())
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_memory_system() -> (MemorySystem, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let app_dir = temp_dir.path().join("winx-test");
        let memory_dir = app_dir.join("memory");

        let memory_system = MemorySystem {
            app_dir,
            memory_dir,
        };

        (memory_system, temp_dir)
    }

    #[test]
    fn test_format_memory() {
        let (memory_system, _temp_dir) = create_test_memory_system();

        let context_save = ContextSave {
            id: "test-task".to_string(),
            description: "Test task description".to_string(),
            project_root_path: Some("/path/to/project".to_string()),
            relevant_file_globs: vec!["*.rs".to_string(), "Cargo.toml".to_string()],
            saved_at: "2024-01-01T00:00:00Z".to_string(),
            working_directory: None,
            mode: None,
        };

        let relevant_files = "# file contents here";
        let formatted = memory_system.format_memory(&context_save, relevant_files);

        // The shell_quote function only adds quotes if the path contains special characters
        // Since "/path/to/project" doesn't contain spaces, it won't be quoted
        assert!(formatted.contains("# PROJECT ROOT = /path/to/project"));
        assert!(formatted.contains("Test task description"));
        assert!(formatted.contains("*.rs"));
        assert!(formatted.contains("# file contents here"));
    }

    #[test]
    fn test_shell_quoting() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("path with spaces"), "\"path with spaces\"");
        assert_eq!(
            shell_quote("path\"with\"quotes"),
            "\"path\\\"with\\\"quotes\""
        );
    }

    #[test]
    fn test_shell_unquoting() {
        assert_eq!(shell_unquote("simple"), Some("simple".to_string()));
        assert_eq!(
            shell_unquote("\"quoted path\""),
            Some("quoted path".to_string())
        );
        assert_eq!(
            shell_unquote("\"path\\\"with\\\"quotes\""),
            Some("path\"with\"quotes".to_string())
        );
        assert_eq!(
            shell_unquote("'single quoted'"),
            Some("single quoted".to_string())
        );
    }

    #[test]
    fn test_save_and_load_memory() {
        let (memory_system, _temp_dir) = create_test_memory_system();

        let context_save = ContextSave {
            id: "test-task".to_string(),
            description: "Test task".to_string(),
            project_root_path: Some("/test/path".to_string()),
            relevant_file_globs: vec!["*.rs".to_string()],
            saved_at: "2024-01-01T00:00:00Z".to_string(),
            working_directory: None,
            mode: None,
        };

        let relevant_files = "Test file content";

        // Save memory
        let saved_path = memory_system
            .save_memory(&context_save, relevant_files, None)
            .unwrap();
        assert!(saved_path.exists());

        // Load memory
        let (project_root, loaded_data, bash_state_json) =
            memory_system.load_memory("test-task", None, None).unwrap();

        assert_eq!(project_root, "/test/path");
        assert!(loaded_data.contains("Test task"));
        assert!(loaded_data.contains("Test file content"));
        assert!(bash_state_json.is_none());
    }

    #[test]
    fn test_list_saved_tasks() {
        let (memory_system, _temp_dir) = create_test_memory_system();

        // Should be empty initially
        let tasks = memory_system.list_saved_tasks().unwrap();
        assert!(tasks.is_empty());

        // Save some tasks
        for i in 1..=3 {
            let context_save = ContextSave {
                id: format!("task-{}", i),
                description: format!("Task {}", i),
                project_root_path: None,
                relevant_file_globs: Vec::new(),
                saved_at: "2024-01-01T00:00:00Z".to_string(),
                working_directory: None,
                mode: None,
            };
            memory_system.save_memory(&context_save, "", None).unwrap();
        }

        // Should list all tasks
        let tasks = memory_system.list_saved_tasks().unwrap();
        assert_eq!(tasks.len(), 3);
        assert!(tasks.contains(&"task-1".to_string()));
        assert!(tasks.contains(&"task-2".to_string()));
        assert!(tasks.contains(&"task-3".to_string()));
    }

    #[test]
    fn test_delete_memory() {
        let (memory_system, _temp_dir) = create_test_memory_system();

        let context_save = ContextSave {
            id: "test-delete".to_string(),
            description: "Test deletion".to_string(),
            project_root_path: None,
            relevant_file_globs: Vec::new(),
            saved_at: "2024-01-01T00:00:00Z".to_string(),
            working_directory: None,
            mode: None,
        };

        // Save and verify
        memory_system.save_memory(&context_save, "", None).unwrap();
        let tasks = memory_system.list_saved_tasks().unwrap();
        assert!(tasks.contains(&"test-delete".to_string()));

        // Delete and verify
        memory_system.delete_memory("test-delete").unwrap();
        let tasks = memory_system.list_saved_tasks().unwrap();
        assert!(!tasks.contains(&"test-delete".to_string()));
    }
}
