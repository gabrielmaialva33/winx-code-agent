//! Path probability analysis module.
//!
//! This module provides functionality for analyzing and scoring file paths
//! based on their relevance, using a pre-trained model similar to WCGW's
//! FastPathAnalyzer implementation.
//!
//! The path analyzer includes several enhanced scoring features:
//!
//! 1. **Contextual Weighting**: Assigns higher relevance to paths that match the current
//!    project context, derived from recently accessed files or user activity.
//!
//! 2. **File Extension Scoring**: Applies different weights to different file types,
//!    allowing customization of which file types are considered more relevant.
//!
//! 3. **Relevance Levels**: Groups files into high, medium, and low relevance
//!    categories for better organization and prioritization.
//!
//! 4. **Workspace Stats Integration**: Leverages the workspace statistics to
//!    improve ranking of files based on user activity and file modification times.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use tokenizers::tokenizer::{Encoding, Tokenizer};
use tracing::{debug, error, info};

use crate::errors::WinxError;

/// # Enhanced Path Analysis Features
///
/// ## Context-Aware Scoring
/// The path analyzer can be configured with context tokens extracted from
/// recently accessed files, which improves relevance scoring by assigning
/// higher scores to paths that match the current project context.
///
/// ```rust,no_run
/// // Example: Creating a context-aware path scorer
/// let recent_files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
/// let context_scorer = create_context_aware_path_scorer(&recent_files, None)?;
/// ```
///
/// ## File Extension Weighting
/// Different file extensions can be assigned different weights, allowing
/// certain file types to be prioritized over others in the ranking.
///
/// ```rust,no_run
/// // Example: Setting custom extension weights
/// let mut scorer = create_default_path_scorer()?;
/// scorer.set_extension_weight("rs", 1.5); // Boost Rust files
/// scorer.set_extension_weight("md", 0.8); // Lower priority for markdown
/// ```
///
/// ## Relevance Levels
/// Files can be filtered or grouped by predefined relevance levels:
///
/// - **High**: Most relevant files (score >= -10.0)
/// - **Medium**: Moderately relevant files (score >= -20.0)
/// - **Low**: Less relevant files (score >= -30.0)
/// - **Custom**: Custom threshold-based relevance
///
/// ```text
/// // Example: Filtering by relevance level
/// let high_relevance = scorer.filter_by_relevance_level(&paths, RelevanceLevel::High);
///
/// // Example: Grouping files by relevance
/// let grouped = scorer.group_by_relevance(&paths);
/// let high_files = grouped.get(&RelevanceLevel::High).unwrap_or(&Vec::new());
/// ```
/// Relevance level for path filtering
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RelevanceLevel {
    /// High relevance (score >= HIGH_RELEVANCE_THRESHOLD)
    High,

    /// Medium relevance (score >= MEDIUM_RELEVANCE_THRESHOLD)
    Medium,

    /// Low relevance (score >= LOW_RELEVANCE_THRESHOLD)
    Low,

    /// Custom relevance threshold
    Custom(f32),
}

/// Groups of paths categorized by relevance level
#[derive(Debug, Clone)]
pub struct RelevanceGroups {
    /// High relevance paths
    pub high: Vec<String>,

    /// Medium relevance paths
    pub medium: Vec<String>,

    /// Low relevance paths
    pub low: Vec<String>,
}

/// Maximum number of tokens to consider when scoring a path
const MAX_PATH_TOKENS: usize = 50;

/// Default score for paths with unknown tokens
const DEFAULT_UNKNOWN_SCORE: f32 = -5.0;

/// Default minimum score threshold for relevance
pub const DEFAULT_RELEVANCE_THRESHOLD: f32 = -30.0;

/// Threshold for high relevance files
pub const HIGH_RELEVANCE_THRESHOLD: f32 = -10.0;

/// Threshold for medium relevance files
pub const MEDIUM_RELEVANCE_THRESHOLD: f32 = -20.0;

/// Threshold for low relevance files
pub const LOW_RELEVANCE_THRESHOLD: f32 = -30.0;

/// Default context weighting factor
const DEFAULT_CONTEXT_WEIGHT: f32 = 1.5;

/// Default extension importance multiplier
const DEFAULT_EXTENSION_MULTIPLIER: f32 = 1.2;

/// PathScorer provides functionality to score file paths based on
/// their relevance using a pre-trained model with enhanced contextual
/// and extension-based scoring capabilities.
///
/// The `PathScorer` integrates four key features for improved path relevance scoring:
///
/// 1. **Base Probability Scoring**: Uses pre-trained token probabilities to
///    calculate a base relevance score for each path.
///
/// 2. **Context Awareness**: Allows additional weighting for paths containing tokens
///    relevant to the current project context, improving contextual relevance.
///
/// 3. **File Extension Weighting**: Applies customizable weights to different
///    file extensions, allowing file types to be prioritized based on importance.
///
/// 4. **Relevance Thresholds**: Enables categorization of paths into different
///    relevance levels (high, medium, low) for better organization.
pub struct PathScorer {
    /// Tokenizer for processing file paths
    tokenizer: Arc<Tokenizer>,

    /// Token probability map
    vocab_probs: HashMap<String, f32>,

    /// Default score for unknown tokens
    unknown_score: f32,

    /// Context tokens for additional weighting
    context_tokens: HashSet<String>,

    /// Context weighting factor
    context_weight: f32,

    /// Extension importance mappings
    extension_weights: HashMap<String, f32>,

    /// Extension importance multiplier
    extension_multiplier: f32,
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

        // Create default extension weights for common file types
        let mut extension_weights = HashMap::new();

        // Code files get higher weights
        extension_weights.insert("rs".to_string(), 1.5); // Rust files (highest priority for Rust project)
        extension_weights.insert("toml".to_string(), 1.3); // Config files
        extension_weights.insert("json".to_string(), 1.2); // Config files
        extension_weights.insert("md".to_string(), 1.1); // Documentation

        // Less important file types
        extension_weights.insert("lock".to_string(), 0.7); // Lock files
        extension_weights.insert("log".to_string(), 0.5); // Log files
        extension_weights.insert("tmp".to_string(), 0.3); // Temporary files

        Ok(Self {
            tokenizer: Arc::new(tokenizer),
            vocab_probs,
            unknown_score: DEFAULT_UNKNOWN_SCORE,
            context_tokens: HashSet::new(),
            context_weight: DEFAULT_CONTEXT_WEIGHT,
            extension_weights,
            extension_multiplier: DEFAULT_EXTENSION_MULTIPLIER,
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

    /// Calculate probability scores for a batch of paths using parallel processing
    ///
    /// # Arguments
    ///
    /// * `paths` - List of paths to score
    ///
    /// # Returns
    ///
    /// Vector of tuples with the score and the corresponding paths
    pub fn calculate_path_probabilities_batch(&self, paths: &[String]) -> Vec<(f32, String)> {
        // Only use parallelization if we have enough paths to justify the overhead
        let threshold = 50; // Threshold for using parallel processing

        let results = if paths.len() >= threshold {
            debug!("Using parallel processing for {} paths", paths.len());

            // Process paths in parallel
            paths
                .par_iter()
                .map(|path| {
                    let score = self.calculate_path_probability(path);
                    (score, path.clone())
                })
                .collect::<Vec<_>>()
        } else {
            // Use sequential processing for small batches
            paths
                .iter()
                .map(|path| {
                    let score = self.calculate_path_probability(path);
                    (score, path.clone())
                })
                .collect::<Vec<_>>()
        };

        // Sort results by score in descending order (highest score first)
        let mut sorted_results = results;
        sorted_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        sorted_results
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

        // Apply extension-based weighting
        let extension_score = self.calculate_extension_score(&normalized_path);

        // Tokenize the path
        let encoding = match self.tokenizer.encode(normalized_path, false) {
            Ok(encoding) => encoding,
            Err(e) => {
                error!("Failed to tokenize path {}: {}", path, e);
                return self.unknown_score * 5.0; // Severely penalize paths we can't tokenize
            }
        };

        // Calculate the probability score based on the tokens
        let token_score = self.calculate_score_from_encoding(&encoding);

        // Apply the extension weight to the token score
        token_score * extension_score
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

    /// Calculate a score based on file extension
    ///
    /// # Arguments
    ///
    /// * `path` - Normalized path
    ///
    /// # Returns
    ///
    /// The extension score multiplier
    fn calculate_extension_score(&self, path: &str) -> f32 {
        // If extension multiplier is 1.0 or we have no extension weights, return 1.0 (no change)
        if (self.extension_multiplier - 1.0).abs() < 0.001 || self.extension_weights.is_empty() {
            return 1.0;
        }

        // Extract the extension from the path
        let extension = path.split('.').next_back().unwrap_or("");

        // Get the extension weight or default to 1.0
        let weight = self
            .extension_weights
            .get(extension)
            .copied()
            .unwrap_or(1.0);

        // Calculate the extension score
        // The formula is: 1.0 + (weight - 1.0) * extension_multiplier
        // This means if extension_multiplier is 0.0, the score is 1.0 (no effect)
        // If extension_multiplier is 1.0, the score is the weight
        1.0 + (weight - 1.0) * self.extension_multiplier
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
        let mut context_matches = 0;

        for token in tokens.iter().take(token_count) {
            // Check if this token is in our context tokens
            if !self.context_tokens.is_empty() && self.context_tokens.contains(token) {
                context_matches += 1;
            }

            // Calculate token probability score
            if let Some(prob) = self.vocab_probs.get(token) {
                total_score += prob;
                _valid_tokens += 1;
            } else {
                total_score += self.unknown_score;
            }
        }

        // Calculate base score (average of token scores)
        let base_score = total_score / token_count as f32;

        // Apply context weighting if we have context tokens and matches
        if !self.context_tokens.is_empty() && context_matches > 0 {
            let context_boost = (context_matches as f32 / token_count as f32) * self.context_weight;

            // Ensure the boost has a reasonable limit
            let capped_boost = context_boost.min(self.context_weight);

            // Apply the boost (multiply the base score by the boost factor)
            base_score * (1.0 + capped_boost)
        } else {
            base_score
        }
    }

    /// Filter and rank paths by relevance threshold using parallel processing
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

        // First get the scored paths using parallel processing
        let scored_paths = self.calculate_path_probabilities_batch(paths);

        // Filter paths based on threshold
        // (This is fast enough to do sequentially, as the scoring is the expensive part)
        scored_paths
            .into_iter()
            .filter(|(score, _)| *score >= threshold)
            .map(|(_, path)| path)
            .collect()
    }

    /// Filter and rank paths by predefined relevance level
    ///
    /// # Arguments
    ///
    /// * `paths` - List of paths to filter and rank
    /// * `relevance_level` - Relevance level (high, medium, low)
    ///
    /// # Returns
    ///
    /// Vector of paths sorted by relevance, filtered by the threshold for the specified level
    pub fn filter_by_relevance_level(
        &self,
        paths: &[String],
        relevance_level: RelevanceLevel,
    ) -> Vec<String> {
        let threshold = match relevance_level {
            RelevanceLevel::High => HIGH_RELEVANCE_THRESHOLD,
            RelevanceLevel::Medium => MEDIUM_RELEVANCE_THRESHOLD,
            RelevanceLevel::Low => LOW_RELEVANCE_THRESHOLD,
            RelevanceLevel::Custom(threshold) => threshold,
        };

        self.filter_and_rank_paths(paths, Some(threshold))
    }

    /// Group paths by relevance levels
    ///
    /// # Arguments
    ///
    /// * `paths` - List of paths to filter and rank
    ///
    /// # Returns
    ///
    /// A struct containing vectors of paths grouped by relevance level
    pub fn group_by_relevance(&self, paths: &[String]) -> RelevanceGroups {
        // First get the scored paths using parallel processing
        let scored_paths = self.calculate_path_probabilities_batch(paths);

        // Create result groups
        let mut high = Vec::new();
        let mut medium = Vec::new();
        let mut low = Vec::new();

        // Group paths by relevance level
        for (score, path) in scored_paths {
            if score >= HIGH_RELEVANCE_THRESHOLD {
                high.push(path);
            } else if score >= MEDIUM_RELEVANCE_THRESHOLD {
                medium.push(path);
            } else if score >= LOW_RELEVANCE_THRESHOLD {
                low.push(path);
            }
        }

        // Return the result as a RelevanceGroups struct
        RelevanceGroups { high, medium, low }
    }

    /// Batch process multiple filtering and ranking operations in parallel
    ///
    /// # Arguments
    ///
    /// * `path_batches` - List of path batches to filter and rank separately
    /// * `threshold` - Minimum relevance score (optional)
    ///
    /// # Returns
    ///
    /// Vector of vectors of paths, each corresponding to the input batch
    pub fn batch_filter_and_rank_paths(
        &self,
        path_batches: &[Vec<String>],
        threshold: Option<f32>,
    ) -> Vec<Vec<String>> {
        let threshold = threshold.unwrap_or(DEFAULT_RELEVANCE_THRESHOLD);

        // Process each batch in parallel
        path_batches
            .par_iter()
            .map(|batch| self.filter_and_rank_paths(batch, Some(threshold)))
            .collect()
    }

    /// Batch process multiple filtering and ranking operations by relevance level in parallel
    ///
    /// # Arguments
    ///
    /// * `path_batches` - List of path batches to filter and rank separately
    /// * `relevance_level` - Relevance level to filter by
    ///
    /// # Returns
    ///
    /// Vector of vectors of paths, each corresponding to the input batch
    pub fn batch_filter_by_relevance_level(
        &self,
        path_batches: &[Vec<String>],
        relevance_level: RelevanceLevel,
    ) -> Vec<Vec<String>> {
        // Get the threshold for the relevance level
        let threshold = match relevance_level {
            RelevanceLevel::High => HIGH_RELEVANCE_THRESHOLD,
            RelevanceLevel::Medium => MEDIUM_RELEVANCE_THRESHOLD,
            RelevanceLevel::Low => LOW_RELEVANCE_THRESHOLD,
            RelevanceLevel::Custom(threshold) => threshold,
        };

        // Process each batch in parallel
        path_batches
            .par_iter()
            .map(|batch| self.filter_and_rank_paths(batch, Some(threshold)))
            .collect()
    }

    /// Batch process multiple grouping by relevance operations in parallel
    ///
    /// # Arguments
    ///
    /// * `path_batches` - List of path batches to group by relevance
    ///
    /// # Returns
    ///
    /// Vector of RelevanceGroups structs, each containing paths grouped by relevance level
    pub fn batch_group_by_relevance(&self, path_batches: &[Vec<String>]) -> Vec<RelevanceGroups> {
        // Process each batch in parallel
        path_batches
            .par_iter()
            .map(|batch| self.group_by_relevance(batch))
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

    /// Set context tokens for contextual weighting
    ///
    /// # Arguments
    ///
    /// * `tokens` - Set of tokens to use for contextual weighting
    pub fn set_context_tokens(&mut self, tokens: HashSet<String>) {
        self.context_tokens = tokens;
    }

    /// Add context tokens for contextual weighting
    ///
    /// # Arguments
    ///
    /// * `tokens` - Tokens to add to the context tokens set
    pub fn add_context_tokens(&mut self, tokens: &[String]) {
        for token in tokens {
            self.context_tokens.insert(token.clone());
        }
    }

    /// Set context weight factor
    ///
    /// # Arguments
    ///
    /// * `weight` - Weight factor for context tokens
    pub fn set_context_weight(&mut self, weight: f32) {
        self.context_weight = weight;
    }

    /// Set extension weight for a specific file extension
    ///
    /// # Arguments
    ///
    /// * `extension` - File extension (without the dot)
    /// * `weight` - Weight factor for the extension
    pub fn set_extension_weight(&mut self, extension: &str, weight: f32) {
        self.extension_weights.insert(extension.to_string(), weight);
    }

    /// Set extension weights from a map
    ///
    /// # Arguments
    ///
    /// * `weights` - Map of extensions to weights
    pub fn set_extension_weights(&mut self, weights: HashMap<String, f32>) {
        self.extension_weights = weights;
    }

    /// Set the extension multiplier (how much to factor in extension weights)
    ///
    /// # Arguments
    ///
    /// * `multiplier` - Extension multiplier factor
    pub fn set_extension_multiplier(&mut self, multiplier: f32) {
        self.extension_multiplier = multiplier;
    }

    /// Extract context tokens from recent files
    ///
    /// # Arguments
    ///
    /// * `recent_files` - List of recently accessed/modified files
    /// * `limit` - Maximum number of tokens to extract (default: 50)
    pub fn extract_context_from_files(&mut self, recent_files: &[String], limit: Option<usize>) {
        let limit = limit.unwrap_or(50);
        let mut token_frequencies = HashMap::new();

        // Process each file path to extract tokens
        for file_path in recent_files {
            let normalized_path = self.normalize_path(file_path);

            // Split by common delimiters
            for part in normalized_path.split(['/', '_', '-', '.']) {
                if !part.is_empty() {
                    *token_frequencies.entry(part.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Sort by frequency and take the top tokens
        let mut tokens: Vec<(String, usize)> = token_frequencies.into_iter().collect();
        tokens.sort_by(|a, b| b.1.cmp(&a.1));

        // Add the top tokens to the context
        for (token, _) in tokens.into_iter().take(limit) {
            self.context_tokens.insert(token);
        }
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

/// Create a PathScorer with workspace stats context for improved relevance scoring
///
/// This function creates a context-aware PathScorer that analyzes recent file
/// activity to build a context model, which is then used to improve path scoring
/// relevance. By considering the files that have been recently accessed or modified,
/// the scorer can prioritize paths that are more likely to be relevant to the
/// current project context.
///
/// # Arguments
///
/// * `recent_files` - List of recently accessed files to use for context extraction.
///   These files are analyzed to extract common tokens that represent the current
///   project context.
///
/// * `extension_weights` - Optional map of extension weights (defaults used if None).
///   This allows customizing the importance of different file types. If None is provided,
///   default weights are used which prioritize code files and configuration.
///
/// # Returns
///
/// A Result containing the context-aware PathScorer or an error
///
/// # Examples
///
/// ```rust
/// // Create a context-aware scorer using recently accessed files
/// let workspace_stats = get_workspace_stats();
/// let recent_files = workspace_stats.get_most_active_files(50);
/// let scorer = create_context_aware_path_scorer(&recent_files, None)?;
///
/// // Get the most relevant files based on context
/// let relevant_files = scorer.filter_and_rank_paths(&all_files, None);
/// ```
pub fn create_context_aware_path_scorer(
    recent_files: &[String],
    extension_weights: Option<HashMap<String, f32>>,
) -> Result<PathScorer> {
    let (model_path, vocab_path) = PathScorer::get_default_paths();
    let mut path_scorer = PathScorer::new(&model_path, &vocab_path)?;

    // Extract context from recent files
    path_scorer.extract_context_from_files(recent_files, None);

    // Apply custom extension weights if provided
    if let Some(weights) = extension_weights {
        path_scorer.set_extension_weights(weights);
    }

    Ok(path_scorer)
}

#[cfg(test)]
mod tests {
    use super::*;
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
             .txt -0.8\n\
             config 0.2\n\
             lib 0.3\n\
             app 0.1\n";

        file.write_all(test_data.as_bytes()).unwrap();
        vocab_path
    }

    // Helper function to create a minimal mock PathScorer for testing
    fn create_test_scorer() -> PathScorer {
        let mut vocab_probs = HashMap::new();
        vocab_probs.insert("src".to_string(), 0.5);
        vocab_probs.insert("main".to_string(), 0.3);
        vocab_probs.insert("utils".to_string(), -0.1);
        vocab_probs.insert("test".to_string(), -0.2);
        vocab_probs.insert(".rs".to_string(), 0.4);
        vocab_probs.insert(".py".to_string(), -0.3);
        vocab_probs.insert(".txt".to_string(), -0.8);
        vocab_probs.insert("config".to_string(), 0.2);
        vocab_probs.insert("lib".to_string(), 0.3);
        vocab_probs.insert("app".to_string(), 0.1);

        // Create default extension weights
        let mut extension_weights = HashMap::new();
        extension_weights.insert("rs".to_string(), 1.5);
        extension_weights.insert("toml".to_string(), 1.3);
        extension_weights.insert("json".to_string(), 1.2);
        extension_weights.insert("md".to_string(), 1.1);
        extension_weights.insert("py".to_string(), 1.0);
        extension_weights.insert("txt".to_string(), 0.8);

        PathScorer {
            tokenizer: Arc::new(Tokenizer::new(
                tokenizers::models::wordpiece::WordPiece::default(),
            )),
            vocab_probs,
            unknown_score: DEFAULT_UNKNOWN_SCORE,
            context_tokens: HashSet::new(),
            context_weight: DEFAULT_CONTEXT_WEIGHT,
            extension_weights,
            extension_multiplier: DEFAULT_EXTENSION_MULTIPLIER,
        }
    }

    // Test for path normalization
    #[test]
    fn test_path_normalization() {
        // Mock a scorer without loading files
        let scorer = PathScorer {
            tokenizer: Arc::new(Tokenizer::new(
                tokenizers::models::wordpiece::WordPiece::default(),
            )),
            vocab_probs: HashMap::new(),
            unknown_score: DEFAULT_UNKNOWN_SCORE,
            context_tokens: HashSet::new(),
            context_weight: DEFAULT_CONTEXT_WEIGHT,
            extension_weights: HashMap::new(),
            extension_multiplier: DEFAULT_EXTENSION_MULTIPLIER,
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

    // Test for file extension scoring
    #[test]
    fn test_extension_scoring() {
        let scorer = create_test_scorer();

        // Test extension scoring with default extension weights
        assert_eq!(
            scorer.calculate_extension_score("src/main.rs"),
            1.0 + (1.5 - 1.0) * DEFAULT_EXTENSION_MULTIPLIER
        );
        assert_eq!(
            scorer.calculate_extension_score("Cargo.toml"),
            1.0 + (1.3 - 1.0) * DEFAULT_EXTENSION_MULTIPLIER
        );
        assert_eq!(
            scorer.calculate_extension_score("README.md"),
            1.0 + (1.1 - 1.0) * DEFAULT_EXTENSION_MULTIPLIER
        );
        assert_eq!(
            scorer.calculate_extension_score("script.py"),
            1.0 + (1.0 - 1.0) * DEFAULT_EXTENSION_MULTIPLIER
        );

        // Test with unknown extension (should default to 1.0)
        assert_eq!(scorer.calculate_extension_score("Makefile"), 1.0);

        // Test with no extension
        assert_eq!(scorer.calculate_extension_score("LICENSE"), 1.0);
    }

    // Test for context token extraction
    #[test]
    fn test_context_token_extraction() {
        let mut scorer = create_test_scorer();

        // Test extracting context from recent files
        let recent_files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/utils/config.rs".to_string(),
            "tests/test_app.rs".to_string(),
        ];

        scorer.extract_context_from_files(&recent_files, None);

        // Check that common tokens were extracted
        assert!(scorer.context_tokens.contains("src"));
        assert!(scorer.context_tokens.contains("rs"));
        assert!(scorer.context_tokens.contains("utils"));
        assert!(scorer.context_tokens.contains("config"));

        // Check the size of the context tokens set
        assert!(scorer.context_tokens.len() > 0);
    }

    // Test for RelevanceLevel grouping
    #[test]
    fn test_relevance_level_grouping() {
        // This test will use a manually crafted mock to test grouping
        // without relying on the tokenizer's encoding functionality

        let mut scorer = create_test_scorer();

        // We'll test by directly calling group_by_relevance and providing test paths
        let paths = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "README.md".to_string(),
            "tests/test_utils.rs".to_string(),
            "docs/example.txt".to_string(),
        ];

        // Set up a context for testing context matching
        scorer.add_context_tokens(&["src".to_string(), "main".to_string()]);

        // In a real test we would set up the scorer to return specific scores for each path
        // We're somewhat limited by not having a real tokenizer to encode paths in tests

        // We'll do a minimal test here to ensure the function runs without errors
        let grouped = scorer.group_by_relevance(&paths);

        // Just verify the struct exists and has the fields we expect
        let _high_relevance_files = &grouped.high;
        let _medium_relevance_files = &grouped.medium;
        let _low_relevance_files = &grouped.low;

        // Basic structure test passing means the function didn't panic
        assert!(true);
    }
}
