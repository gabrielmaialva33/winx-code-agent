//! Path probability analysis module.
//!
//! This module provides functionality for analyzing and scoring file paths
//! based on their relevance, using a pre-trained model similar to WCGW's
//! FastPathAnalyzer implementation.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokenizers::tokenizer::{Encoding, Tokenizer};
use tracing::{error, info};

use crate::errors::WinxError;

/// Maximum number of tokens to consider when scoring a path
const MAX_PATH_TOKENS: usize = 50;

/// Default score for paths with unknown tokens
const DEFAULT_UNKNOWN_SCORE: f32 = -5.0;

/// Default minimum score threshold for relevance
pub const DEFAULT_RELEVANCE_THRESHOLD: f32 = -30.0;

/// PathScorer provides functionality to score file paths based on
/// their relevance using a pre-trained model.
pub struct PathScorer {
    /// Tokenizer for processing file paths
    tokenizer: Arc<Tokenizer>,

    /// Token probability map
    vocab_probs: HashMap<String, f32>,

    /// Default score for unknown tokens
    unknown_score: f32,
}

impl PathScorer {
    /// Create a new PathScorer with the specified model and vocabulary files
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to the tokenizer model file
    /// * `vocab_path` - Path to the vocabulary probabilities file
    ///
    /// # Returns
    ///
    /// A Result containing the new PathScorer or an error
    pub fn new(model_path: &Path, vocab_path: &Path) -> Result<Self> {
        let tokenizer =
            Tokenizer::from_file(model_path.to_string_lossy().as_ref()).map_err(|e| {
                WinxError::DataLoadingError(format!("Failed to load tokenizer model: {}", e))
            })?;

        let vocab_probs = Self::load_vocab_probs(vocab_path)?;

        Ok(Self {
            tokenizer: Arc::new(tokenizer),
            vocab_probs,
            unknown_score: DEFAULT_UNKNOWN_SCORE,
        })
    }

    /// Load vocabulary probabilities from a file
    ///
    /// # Arguments
    ///
    /// * `vocab_path` - Path to the vocabulary probabilities file
    ///
    /// # Returns
    ///
    /// A Result containing the vocabulary probabilities map or an error
    fn load_vocab_probs(vocab_path: &Path) -> Result<HashMap<String, f32>> {
        let file = File::open(vocab_path)
            .with_context(|| format!("Failed to open vocabulary file: {}", vocab_path.display()))?;

        let reader = BufReader::new(file);
        let mut vocab_probs = HashMap::new();

        for line in reader.lines() {
            let line = line?;
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() == 2 {
                if let Ok(prob) = parts[1].parse::<f32>() {
                    vocab_probs.insert(parts[0].to_string(), prob);
                } else {
                    error!("Invalid probability value in vocab file: {}", parts[1]);
                }
            } else {
                error!("Invalid line format in vocab file: {}", line);
            }
        }

        if vocab_probs.is_empty() {
            return Err(WinxError::DataLoadingError(
                "Vocabulary file is empty or has invalid format".to_string(),
            )
            .into());
        }

        info!("Loaded {} vocabulary entries", vocab_probs.len());
        Ok(vocab_probs)
    }

    /// Calculate probability scores for a batch of paths
    ///
    /// # Arguments
    ///
    /// * `paths` - List of paths to score
    ///
    /// # Returns
    ///
    /// Vector of tuples with the score and the corresponding paths
    pub fn calculate_path_probabilities_batch(&self, paths: &[String]) -> Vec<(f32, String)> {
        let mut results = Vec::with_capacity(paths.len());

        for path in paths {
            let score = self.calculate_path_probability(path);
            results.push((score, path.clone()));
        }

        // Sort by score in descending order (highest score first)
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        results
    }

    /// Calculate probability score for a single path
    ///
    /// # Arguments
    ///
    /// * `path` - Path to score
    ///
    /// # Returns
    ///
    /// The probability score for the path
    pub fn calculate_path_probability(&self, path: &str) -> f32 {
        // Normalize the path to avoid platform-specific differences
        let normalized_path = self.normalize_path(path);

        // Tokenize the path
        let encoding = match self.tokenizer.encode(normalized_path, false) {
            Ok(encoding) => encoding,
            Err(e) => {
                error!("Failed to tokenize path {}: {}", path, e);
                return self.unknown_score * 5.0; // Severely penalize paths we can't tokenize
            }
        };

        // Calculate the probability score based on the tokens
        self.calculate_score_from_encoding(&encoding)
    }

    /// Normalize a path to ensure consistent scoring across platforms
    ///
    /// # Arguments
    ///
    /// * `path` - Path to normalize
    ///
    /// # Returns
    ///
    /// The normalized path string
    fn normalize_path(&self, path: &str) -> String {
        // Convert backslashes to forward slashes for consistency
        let normalized = path.replace('\\', "/");

        // Remove leading ./ or / for consistency
        let normalized = normalized.trim_start_matches("./").trim_start_matches('/');

        normalized.to_string()
    }

    /// Calculate a score from the token encoding
    ///
    /// # Arguments
    ///
    /// * `encoding` - Token encoding from the tokenizer
    ///
    /// # Returns
    ///
    /// The probability score for the encoding
    fn calculate_score_from_encoding(&self, encoding: &Encoding) -> f32 {
        let tokens = encoding.get_tokens();
        let token_count = tokens.len().min(MAX_PATH_TOKENS);

        if token_count == 0 {
            return self.unknown_score * 3.0; // Penalize empty token lists
        }

        let mut total_score = 0.0;
        let mut _valid_tokens = 0;

        for token in tokens.iter().take(token_count) {
            if let Some(prob) = self.vocab_probs.get(token) {
                total_score += prob;
                _valid_tokens += 1;
            } else {
                total_score += self.unknown_score;
            }
        }

        // Return average score to prevent longer paths from dominating
        // but ensure at least 1 valid token to avoid division by zero
        total_score / token_count as f32
    }

    /// Filter and rank paths by relevance threshold
    ///
    /// # Arguments
    ///
    /// * `paths` - List of paths to filter and rank
    /// * `threshold` - Minimum relevance score (optional)
    ///
    /// # Returns
    ///
    /// Vector of paths sorted by relevance, filtered by the threshold
    pub fn filter_and_rank_paths(&self, paths: &[String], threshold: Option<f32>) -> Vec<String> {
        let threshold = threshold.unwrap_or(DEFAULT_RELEVANCE_THRESHOLD);

        let scored_paths = self.calculate_path_probabilities_batch(paths);

        scored_paths
            .into_iter()
            .filter(|(score, _)| *score >= threshold)
            .map(|(_, path)| path)
            .collect()
    }

    /// Get the default paths for the model and vocabulary files
    ///
    /// # Returns
    ///
    /// A tuple containing the default model and vocabulary paths
    pub fn get_default_paths() -> (PathBuf, PathBuf) {
        let resources_dir = PathBuf::from("resources");
        let model_path = resources_dir.join("paths_tokens.model");
        let vocab_path = resources_dir.join("paths_model.vocab");

        (model_path, vocab_path)
    }
}

/// Create a PathScorer with default configuration
///
/// # Returns
///
/// A Result containing the PathScorer or an error
pub fn create_default_path_scorer() -> Result<PathScorer> {
    let (model_path, vocab_path) = PathScorer::get_default_paths();
    PathScorer::new(&model_path, &vocab_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    // Helper to create a test vocabulary file
    fn create_test_vocab_file(dir: &TempDir) -> PathBuf {
        let vocab_path = dir.path().join("test.vocab");
        let mut file = File::create(&vocab_path).unwrap();

        // Write some test vocab entries
        let test_data = "src 0.5\n\
             main 0.3\n\
             utils -0.1\n\
             test -0.2\n\
             .rs 0.4\n\
             .py -0.3\n\
             .txt -0.8\n";

        file.write_all(test_data.as_bytes()).unwrap();
        vocab_path
    }

    // Test for path normalization
    #[test]
    fn test_path_normalization() {
        // Mock a scorer without loading files
        let scorer = PathScorer {
            tokenizer: Arc::new(Tokenizer::new(
                tokenizers::models::wordpiece::WordPiece::empty(),
            )),
            vocab_probs: HashMap::new(),
            unknown_score: DEFAULT_UNKNOWN_SCORE,
        };

        // Test path normalization
        assert_eq!(scorer.normalize_path("/src/main.rs"), "src/main.rs");
        assert_eq!(
            scorer.normalize_path("./src/utils/test.rs"),
            "src/utils/test.rs"
        );
        assert_eq!(
            scorer.normalize_path("src\\utils\\test.rs"),
            "src/utils/test.rs"
        );
    }

    // Future tests would include:
    // - test_calculate_path_probability
    // - test_filter_and_rank_paths
    // These require mocking a tokenizer which is more complex
}
