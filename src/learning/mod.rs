//! Learning module - Learns user communication patterns.
//!
//! This module reads Claude Code sessions and extracts:
//! - Communication patterns (vocabulary, style)
//! - Repeated requests (automation candidates)
//! - Thinking patterns (mental shortcuts)
//!
//! Focuses on HOW the user thinks and communicates, not on the code itself.

pub mod communication;
pub mod embedding_engine;
pub mod embeddings;
pub mod repetitions;
pub mod session_parser;
pub mod store;
pub mod thinking;

use std::path::PathBuf;

use directories::BaseDirs;

use crate::errors::WinxError;

pub use communication::CommunicationLearner;
pub use embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine, EmbeddingStats};
pub use repetitions::RepetitionDetector;
pub use session_parser::{SessionMessage, SessionParser};
pub use store::LearningStore;
pub use thinking::ThinkingPatterns;

/// Default directory for storing learning data.
pub fn default_learning_dir() -> PathBuf {
    BaseDirs::new().map_or_else(|| PathBuf::from("."), |d| d.home_dir().to_path_buf())
        .join(".winx")
        .join("learning")
}

/// Directory for Claude Code sessions.
pub fn claude_sessions_dir() -> PathBuf {
    BaseDirs::new().map_or_else(|| PathBuf::from("."), |d| d.home_dir().to_path_buf())
        .join(".claude")
        .join("projects")
}

/// Complete learning system.
pub struct LearningSystem {
    /// Session parser
    pub parser: SessionParser,
    /// Communication learner
    pub communication: CommunicationLearner,
    /// Repetition detector
    pub repetitions: RepetitionDetector,
    /// Thinking patterns
    pub thinking: ThinkingPatterns,
    /// Persistent storage
    pub store: LearningStore,
}

impl LearningSystem {
    /// Creates a new learning system.
    pub fn new() -> Result<Self, WinxError> {
        let sessions_dir = claude_sessions_dir();
        let learning_dir = default_learning_dir();

        Ok(Self {
            parser: SessionParser::new(sessions_dir),
            communication: CommunicationLearner::new(),
            repetitions: RepetitionDetector::new(),
            thinking: ThinkingPatterns::new(),
            store: LearningStore::new(learning_dir)?,
        })
    }

    /// Processes all sessions and extracts learning.
    pub async fn process_all_sessions(&mut self) -> Result<LearningReport, WinxError> {
        // 1. Parse all sessions
        let messages = self.parser.parse_all_sessions().await?;

        // 2. Extract only user messages
        let user_messages: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "user")
            .collect();

        // 3. Analyze communication
        for msg in &user_messages {
            self.communication.analyze(&msg.content);
        }

        // 4. Detect repetitions
        for msg in &user_messages {
            self.repetitions.add_message(&msg.content, &msg.session_id);
        }

        // 5. Analyze thinking patterns
        self.thinking.analyze_sequences(&messages);

        // 6. Generate report
        let report = LearningReport {
            total_sessions: self.parser.session_count(),
            total_messages: messages.len(),
            user_messages: user_messages.len(),
            vocabulary: self.communication.get_vocabulary(),
            frequent_requests: self.repetitions.get_frequent_requests(2),
            automation_candidates: self.repetitions.get_automation_candidates(),
            thinking_patterns: self.thinking.get_patterns(),
        };

        // 7. Persist learning
        self.store.save_report(&report)?;

        Ok(report)
    }
}

impl Default for LearningSystem {
    fn default() -> Self {
        Self::new().expect("Failed to create LearningSystem")
    }
}

/// Learning report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearningReport {
    /// Total processed sessions
    pub total_sessions: usize,
    /// Total messages
    pub total_messages: usize,
    /// User messages
    pub user_messages: usize,
    /// Learned vocabulary
    pub vocabulary: Vec<(String, usize)>,
    /// Frequent requests
    pub frequent_requests: Vec<FrequentRequest>,
    /// Automation candidates
    pub automation_candidates: Vec<AutomationCandidate>,
    /// Thinking patterns
    pub thinking_patterns: Vec<ThinkingPattern>,
}

/// Detected frequent request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrequentRequest {
    /// Request text (normalized)
    pub text: String,
    /// Occurrence count
    pub count: usize,
    /// Sessions where it appeared
    pub sessions: Vec<String>,
}

/// Automation candidate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationCandidate {
    /// Pattern description
    pub pattern: String,
    /// Suggested command/skill
    pub suggested_command: String,
    /// Frequency
    pub frequency: usize,
    /// Usage examples
    pub examples: Vec<String>,
}

/// Detected thinking pattern.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThinkingPattern {
    /// Pattern name
    pub name: String,
    /// Description
    pub description: String,
    /// Frequency
    pub frequency: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_dirs() {
        let learning_dir = default_learning_dir();
        assert!(learning_dir.to_string_lossy().contains(".winx/learning"));

        let sessions_dir = claude_sessions_dir();
        assert!(sessions_dir.to_string_lossy().contains(".claude/projects"));
    }
}
