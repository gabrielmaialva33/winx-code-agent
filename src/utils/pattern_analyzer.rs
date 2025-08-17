//! Pattern analyzer for command suggestions and intelligent predictions
//!
//! This module provides pattern recognition capabilities for analyzing command history,
//! predicting user intent, and making intelligent suggestions based on observed patterns.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::errors::Result;

/// Maximum number of commands to track in history
const MAX_HISTORY_SIZE: usize = 100;

/// Maximum age of commands in history (in seconds)
const MAX_COMMAND_AGE: u64 = 3600; // 1 hour

/// Maximum number of suggestions to provide
const MAX_SUGGESTIONS: usize = 5;

/// Pattern recognition confidence threshold (0.0-1.0)
const CONFIDENCE_THRESHOLD: f64 = 0.65;

/// A pattern recognition engine for command history analysis
#[derive(Debug, Clone)]
pub struct PatternAnalyzer {
    /// Command history with timestamps
    command_history: VecDeque<(String, Instant)>,

    /// Command frequency map
    command_frequency: HashMap<String, usize>,

    /// Command sequence patterns (command → likely next command)
    sequence_patterns: HashMap<String, HashMap<String, usize>>,

    /// Directory context mapping
    directory_context: HashMap<String, Vec<String>>,

    /// File operation patterns
    file_patterns: HashMap<String, Vec<String>>,
}

impl Default for PatternAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl PatternAnalyzer {
    /// Create a new pattern analyzer
    pub fn new() -> Self {
        Self {
            command_history: VecDeque::with_capacity(MAX_HISTORY_SIZE),
            command_frequency: HashMap::new(),
            sequence_patterns: HashMap::new(),
            directory_context: HashMap::new(),
            file_patterns: HashMap::new(),
        }
    }

    /// Add a command to the history and update patterns
    pub fn record_command(&mut self, command: &str, current_dir: &str) -> Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }

        // Cleanup old commands
        self.cleanup_history();

        // Get normalized command for analysis
        let normalized = self.normalize_command(command);

        // Update command history
        self.command_history
            .push_back((normalized.clone(), Instant::now()));
        if self.command_history.len() > MAX_HISTORY_SIZE {
            self.command_history.pop_front();
        }

        // Update frequency map
        *self
            .command_frequency
            .entry(normalized.clone())
            .or_insert(0) += 1;

        // Update sequence patterns if there's a previous command
        if let Some((prev_cmd, _)) = self.command_history.iter().rev().nth(1) {
            let next_commands = self.sequence_patterns.entry(prev_cmd.clone()).or_default();
            *next_commands.entry(normalized.clone()).or_insert(0) += 1;
        }

        // Update directory context
        let dir_commands = self
            .directory_context
            .entry(current_dir.to_string())
            .or_default();

        // Only add if it's not already in the list
        if !dir_commands.contains(&normalized) {
            dir_commands.push(normalized.clone());

            // Limit the number of commands per directory
            if dir_commands.len() > MAX_SUGGESTIONS * 2 {
                dir_commands.remove(0);
            }
        }

        // Update file patterns if this is a file operation
        if let Some(file_path) = self.extract_file_path(command) {
            if let Some(file_ext) = Path::new(&file_path).extension() {
                if let Some(ext_str) = file_ext.to_str() {
                    let file_type = format!(".{}", ext_str);
                    let file_commands = self.file_patterns.entry(file_type).or_default();

                    if !file_commands.contains(&normalized) {
                        file_commands.push(normalized);

                        // Limit the number of commands per file type
                        if file_commands.len() > MAX_SUGGESTIONS * 2 {
                            file_commands.remove(0);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get command suggestions based on current context
    pub fn suggest_commands(
        &self,
        partial_command: &str,
        current_dir: &str,
        last_command: Option<&str>,
    ) -> Vec<String> {
        let mut suggestions = Vec::new();
        let mut scores: HashMap<String, f64> = HashMap::new();

        // Suggestions based on command history frequency
        for (cmd, freq) in &self.command_frequency {
            if cmd.starts_with(partial_command) {
                let score = (*freq as f64) / (self.command_history.len() as f64).max(1.0);
                *scores.entry(cmd.clone()).or_insert(0.0) += score * 0.3;
            }
        }

        // Suggestions based on sequence patterns (what comes after the last command)
        if let Some(last) = last_command {
            if let Some(next_commands) = self.sequence_patterns.get(last) {
                let total: usize = next_commands.values().sum();
                for (cmd, count) in next_commands {
                    if cmd.starts_with(partial_command) {
                        let score = (*count as f64) / (total as f64).max(1.0);
                        *scores.entry(cmd.clone()).or_insert(0.0) += score * 0.4;
                    }
                }
            }
        }

        // Suggestions based on directory context
        if let Some(dir_commands) = self.directory_context.get(current_dir) {
            for cmd in dir_commands {
                if cmd.starts_with(partial_command) {
                    *scores.entry(cmd.clone()).or_insert(0.0) += 0.2;
                }
            }
        }

        // Sort by score and return top suggestions
        let mut scored_commands: Vec<(String, f64)> = scores.into_iter().collect();
        scored_commands.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        for (cmd, score) in scored_commands {
            if score >= CONFIDENCE_THRESHOLD && suggestions.len() < MAX_SUGGESTIONS {
                suggestions.push(cmd);
            }
        }

        suggestions
    }

    /// Get suggestions for error recovery
    pub fn suggest_error_recovery(&self, error_message: &str) -> Option<String> {
        // Extract keywords from error message
        let _error_keywords = self.extract_keywords(error_message);

        // Find previously successful commands after similar errors
        for (i, (cmd, _)) in self.command_history.iter().rev().enumerate() {
            // Skip very recent commands (likely part of the problem)
            if i < 2 {
                continue;
            }

            // Look for common error recovery patterns
            if (error_message.contains("No such file or directory")
                || error_message.contains("not found"))
                && (cmd.contains("mkdir") || cmd.contains("touch") || cmd.contains("git clone"))
            {
                return Some(cmd.clone());
            }

            if error_message.contains("Permission denied")
                && (cmd.contains("chmod") || cmd.contains("sudo"))
            {
                return Some(cmd.clone());
            }

            if error_message.contains("command not found")
                && (cmd.contains("apt") || cmd.contains("brew") || cmd.contains("npm install"))
            {
                return Some(format!("Install the required command first with: {}", cmd));
            }
        }

        None
    }

    /// Clean up old commands from history
    fn cleanup_history(&mut self) {
        let now = Instant::now();
        let cutoff = Duration::from_secs(MAX_COMMAND_AGE);

        while let Some((_, timestamp)) = self.command_history.front() {
            if now.duration_since(*timestamp) > cutoff {
                self.command_history.pop_front();
            } else {
                break;
            }
        }
    }

    /// Normalize a command for pattern analysis
    fn normalize_command(&self, command: &str) -> String {
        // Remove timestamps, PIDs, and other varying elements
        // This is a simplified normalization for demonstration
        command.trim().to_string()
    }

    /// Extract file path from a command
    fn extract_file_path(&self, command: &str) -> Option<String> {
        // Simple file path extraction, can be improved
        let parts: Vec<&str> = command.split_whitespace().collect();
        for part in parts {
            if part.contains('/') && !part.starts_with('-') {
                return Some(part.to_string());
            }
        }
        None
    }

    /// Extract keywords from a string
    fn extract_keywords(&self, text: &str) -> Vec<String> {
        text.split_whitespace()
            .filter(|word| word.len() > 3)
            .map(|word| word.to_lowercase())
            .collect()
    }

    /// Get command history
    pub fn get_history(&self) -> Vec<String> {
        self.command_history
            .iter()
            .map(|(cmd, _)| cmd.clone())
            .collect()
    }

    /// Clear the pattern analyzer history and patterns
    pub fn clear(&mut self) {
        self.command_history.clear();
        self.command_frequency.clear();
        self.sequence_patterns.clear();
        self.directory_context.clear();
        self.file_patterns.clear();
    }
}

/// A suggestion with confidence score and explanation
#[derive(Debug, Clone)]
pub struct CommandSuggestion {
    /// The suggested command
    pub command: String,

    /// Confidence score (0.0-1.0)
    pub confidence: f64,

    /// Explanation for why this command was suggested
    pub explanation: String,
}

/// Shared pattern analyzer that can be used from multiple tools
#[derive(Debug, Clone)]
pub struct SharedPatternAnalyzer {
    inner: Arc<tokio::sync::Mutex<PatternAnalyzer>>,
}

impl Default for SharedPatternAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedPatternAnalyzer {
    /// Create a new shared pattern analyzer
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(PatternAnalyzer::new())),
        }
    }

    /// Record a command in the shared analyzer
    pub async fn record_command(&self, command: &str, current_dir: &str) -> Result<()> {
        let mut analyzer = self.inner.lock().await;
        analyzer.record_command(command, current_dir)
    }

    /// Get command suggestions from the shared analyzer
    pub async fn suggest_commands(
        &self,
        partial_command: &str,
        current_dir: &str,
        last_command: Option<&str>,
    ) -> Vec<String> {
        let analyzer = self.inner.lock().await;
        analyzer.suggest_commands(partial_command, current_dir, last_command)
    }

    /// Get error recovery suggestions from the shared analyzer
    pub async fn suggest_error_recovery(&self, error_message: &str) -> Option<String> {
        let analyzer = self.inner.lock().await;
        analyzer.suggest_error_recovery(error_message)
    }

    /// Clear the shared analyzer
    pub async fn clear(&self) {
        let mut analyzer = self.inner.lock().await;
        analyzer.clear();
    }
}
