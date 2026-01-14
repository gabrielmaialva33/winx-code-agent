//! Session Parser - Parses Claude Code sessions.
//!
//! Parses JSONL files in `~/.claude/projects/**/*.jsonl`.
//! Extracts user/assistant messages for analysis.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::errors::WinxError;

/// Claude Code session parser.
pub struct SessionParser {
    /// Sessions directory
    sessions_dir: PathBuf,
    /// Processed session count
    session_count: usize,
}

/// Message extracted from a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    /// Session ID
    pub session_id: String,
    /// Role: "user" or "assistant"
    pub role: String,
    /// Message content
    pub content: String,
    /// Timestamp
    pub timestamp: String,
    /// Working directory
    pub cwd: Option<String>,
    /// Associated project
    pub project: Option<String>,
}

/// JSONL entry of a session.
#[derive(Debug, Deserialize)]
struct SessionEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<MessageContent>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
}

/// Message content (can be string or array).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Simple { role: String, content: String },
    Complex { role: String, content: Vec<ContentPart> },
}

/// Content part (for complex messages).
#[derive(Debug, Deserialize)]
struct ContentPart {
    #[serde(rename = "type")]
    part_type: String,
    text: Option<String>,
}

impl SessionParser {
    /// Creates a new parser.
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self {
            sessions_dir,
            session_count: 0,
        }
    }

    /// Returns session count.
    pub fn session_count(&self) -> usize {
        self.session_count
    }

    /// Finds all session files.
    pub async fn find_session_files(&self) -> Result<Vec<PathBuf>, WinxError> {
        let mut files = Vec::new();

        if !self.sessions_dir.exists() {
            warn!("Sessions directory not found: {:?}", self.sessions_dir);
            return Ok(files);
        }

        // Recursively find .jsonl files
        let mut stack = vec![self.sessions_dir.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = fs::read_dir(&dir).await?;

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Ok(file_type) = entry.file_type().await {
                    if file_type.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                        files.push(path);
                    }
                }
            }
        }

        info!("Found {} session files", files.len());
        Ok(files)
    }

    /// Parses a single session file.
    pub async fn parse_session_file(
        &self,
        path: &PathBuf,
    ) -> Result<Vec<SessionMessage>, WinxError> {
        let mut messages = Vec::new();

        let file = fs::File::open(path).await?;

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // Extracts session_id from filename
        let default_session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<SessionEntry>(&line) {
                Ok(entry) => {
                    // Only process entries with messages
                    if let Some(msg) = entry.message {
                        let session_id = entry
                            .session_id
                            .clone()
                            .unwrap_or_else(|| default_session_id.clone());

                        let (role, content) = match msg {
                            MessageContent::Simple { role, content } => (role, content),
                            MessageContent::Complex { role, content } => {
                                // Concatenate content parts
                                let text = content
                                    .iter()
                                    .filter_map(|p| {
                                        if p.part_type == "text" {
                                            p.text.clone()
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                (role, text)
                            }
                        };

                        // Ignore empty or system messages
                        if !content.trim().is_empty() && !content.contains("<command-message>") {
                            messages.push(SessionMessage {
                                session_id,
                                role,
                                content,
                                timestamp: entry.timestamp.unwrap_or_default(),
                                cwd: entry.cwd,
                                project: None,
                            });
                        }
                    }
                }
                Err(e) => {
                    // Log but continue - some lines might have different format
                    debug!("Failed to parse line in {:?}: {}", path, e);
                }
            }
        }

        Ok(messages)
    }

    /// Parses all sessions.
    pub async fn parse_all_sessions(&mut self) -> Result<Vec<SessionMessage>, WinxError> {
        let files = self.find_session_files().await?;
        self.session_count = files.len();

        let mut all_messages = Vec::new();

        for (i, file) in files.iter().enumerate() {
            if i % 100 == 0 {
                info!("Processing session {}/{}", i + 1, files.len());
            }

            match self.parse_session_file(file).await {
                Ok(messages) => {
                    // Adds project based on path
                    let project = file
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(std::string::ToString::to_string);

                    for mut msg in messages {
                        msg.project = project.clone();
                        all_messages.push(msg);
                    }
                }
                Err(e) => {
                    warn!("Failed to parse {:?}: {}", file, e);
                }
            }
        }

        info!(
            "Parsed {} messages from {} sessions",
            all_messages.len(),
            self.session_count
        );

        Ok(all_messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_parser() {
        let parser = SessionParser::new(PathBuf::from("/tmp/test"));
        assert_eq!(parser.session_count(), 0);
    }

    #[tokio::test]
    async fn test_find_nonexistent_dir() {
        let parser = SessionParser::new(PathBuf::from("/nonexistent/path"));
        let files = parser.find_session_files().await.unwrap();
        assert!(files.is_empty());
    }
}
