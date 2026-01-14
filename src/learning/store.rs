//! Learning Store - Learning persistence.
//!
//! Saves and loads learning data in `~/.winx/learning/`.

use std::fs;
use std::path::PathBuf;

use serde_json;
use tracing::{info, warn};

use crate::errors::WinxError;

use super::LearningReport;

/// Store for learning persistence.
pub struct LearningStore {
    /// Base directory
    base_dir: PathBuf,
}

impl LearningStore {
    /// Creates a new store.
    pub fn new(base_dir: PathBuf) -> Result<Self, WinxError> {
        // Create necessary directories
        let dirs = [
            base_dir.join("sessions").join("messages"),
            base_dir.join("communication"),
            base_dir.join("repetitions"),
            base_dir.join("thinking"),
            base_dir.join("models"),
        ];

        for dir in &dirs {
            fs::create_dir_all(dir)?;
        }

        info!("Learning store initialized at {:?}", base_dir);

        Ok(Self { base_dir })
    }

    /// Saves learning report.
    pub fn save_report(&self, report: &LearningReport) -> Result<(), WinxError> {
        // Save vocabulary
        let vocab_path = self.base_dir.join("communication").join("vocabulary.json");
        self.save_json(&vocab_path, &report.vocabulary)?;

        // Save frequent requests
        let freq_path = self.base_dir.join("repetitions").join("frequent_requests.json");
        self.save_json(&freq_path, &report.frequent_requests)?;

        // Save automation candidates
        let auto_path = self.base_dir.join("repetitions").join("automation_candidates.json");
        self.save_json(&auto_path, &report.automation_candidates)?;

        // Save thinking patterns
        let think_path = self.base_dir.join("thinking").join("patterns.json");
        self.save_json(&think_path, &report.thinking_patterns)?;

        // Save full report
        let report_path = self.base_dir.join("learning_report.json");
        self.save_json(&report_path, report)?;

        info!(
            "Saved learning report: {} sessions, {} messages",
            report.total_sessions, report.total_messages
        );

        Ok(())
    }

    /// Loads existing report.
    pub fn load_report(&self) -> Option<LearningReport> {
        let report_path = self.base_dir.join("learning_report.json");

        if !report_path.exists() {
            return None;
        }

        match fs::read_to_string(&report_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(report) => Some(report),
                Err(e) => {
                    warn!("Failed to parse learning report: {}", e);
                    None
                }
            },
            Err(e) => {
                warn!("Failed to read learning report: {}", e);
                None
            }
        }
    }

    /// Checks if learning data exists.
    pub fn has_learning(&self) -> bool {
        self.base_dir.join("learning_report.json").exists()
    }

    /// Returns base directory path.
    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// Saves JSON to file.
    fn save_json<T: serde::Serialize>(&self, path: &PathBuf, data: &T) -> Result<(), WinxError> {
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| WinxError::SerializationError(e.to_string()))?;

        fs::write(path, json)?;

        Ok(())
    }

    /// Loads JSON from file.
    pub fn load_json<T: serde::de::DeserializeOwned>(&self, path: &PathBuf) -> Option<T> {
        if !path.exists() {
            return None;
        }

        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(data) => Some(data),
                Err(e) => {
                    warn!("Failed to parse {:?}: {}", path, e);
                    None
                }
            },
            Err(e) => {
                warn!("Failed to read {:?}: {}", path, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_store() {
        let temp = TempDir::new().unwrap();
        let store = LearningStore::new(temp.path().to_path_buf()).unwrap();

        assert!(store.base_dir().join("communication").exists());
        assert!(store.base_dir().join("repetitions").exists());
        assert!(store.base_dir().join("thinking").exists());
    }

    #[test]
    fn test_save_and_load_report() {
        let temp = TempDir::new().unwrap();
        let store = LearningStore::new(temp.path().to_path_buf()).unwrap();

        let report = LearningReport {
            total_sessions: 10,
            total_messages: 100,
            user_messages: 50,
            vocabulary: vec![("mano".to_string(), 20)],
            frequent_requests: vec![],
            automation_candidates: vec![],
            thinking_patterns: vec![],
        };

        store.save_report(&report).unwrap();
        assert!(store.has_learning());

        let loaded = store.load_report().unwrap();
        assert_eq!(loaded.total_sessions, 10);
        assert_eq!(loaded.vocabulary.len(), 1);
    }
}
