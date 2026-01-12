//! Simplified path probability analysis module.
//!
//! This is a simplified version without NLP tokenizers.
//! Uses heuristics based on file extension and path depth.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use tracing::debug;

/// Relevance level for path filtering
#[derive(Debug, PartialEq, Clone, Copy, Eq, Hash)]
pub enum RelevanceLevel {
    High,
    Medium,
    Low,
    Custom(i32),
}

/// Groups of paths categorized by relevance level
#[derive(Debug, Clone, Default)]
pub struct RelevanceGroups {
    pub high: Vec<String>,
    pub medium: Vec<String>,
    pub low: Vec<String>,
}

/// Threshold for high relevance files
pub const HIGH_RELEVANCE_THRESHOLD: f32 = -10.0;

/// Threshold for medium relevance files
pub const MEDIUM_RELEVANCE_THRESHOLD: f32 = -20.0;

/// Threshold for low relevance files
pub const LOW_RELEVANCE_THRESHOLD: f32 = -30.0;

/// Default relevance threshold
pub const DEFAULT_RELEVANCE_THRESHOLD: f32 = -30.0;

/// Simplified PathScorer using heuristics instead of NLP
pub struct PathScorer {
    extension_weights: HashMap<String, f32>,
}

impl PathScorer {
    /// Create a new PathScorer with default extension weights
    pub fn new() -> Self {
        let mut extension_weights = HashMap::new();

        // Code files - high priority
        extension_weights.insert("rs".to_string(), 2.0);
        extension_weights.insert("py".to_string(), 2.0);
        extension_weights.insert("js".to_string(), 1.8);
        extension_weights.insert("ts".to_string(), 1.8);
        extension_weights.insert("go".to_string(), 1.8);
        extension_weights.insert("java".to_string(), 1.8);
        extension_weights.insert("c".to_string(), 1.8);
        extension_weights.insert("cpp".to_string(), 1.8);
        extension_weights.insert("h".to_string(), 1.5);

        // Config files - medium-high priority
        extension_weights.insert("toml".to_string(), 1.5);
        extension_weights.insert("yaml".to_string(), 1.4);
        extension_weights.insert("yml".to_string(), 1.4);
        extension_weights.insert("json".to_string(), 1.3);

        // Documentation - medium priority
        extension_weights.insert("md".to_string(), 1.2);
        extension_weights.insert("txt".to_string(), 1.0);

        // Less important
        extension_weights.insert("lock".to_string(), 0.5);
        extension_weights.insert("log".to_string(), 0.3);
        extension_weights.insert("tmp".to_string(), 0.2);

        Self { extension_weights }
    }

    /// Set a custom weight for a file extension
    pub fn set_extension_weight(&mut self, ext: &str, weight: f32) {
        self.extension_weights.insert(ext.to_string(), weight);
    }

    /// Calculate probability score for a single path using heuristics
    pub fn calculate_path_probability(&self, path: &str) -> f32 {
        let path_obj = Path::new(path);
        let mut score: f32 = 0.0;

        // Extension-based scoring
        if let Some(ext) = path_obj.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            if let Some(&weight) = self.extension_weights.get(&ext_str) {
                score += weight * 5.0;
            }
        }

        // Depth penalty (deeper files are less relevant)
        let depth = path.matches('/').count() as f32;
        score -= depth * 0.5;

        // Bonus for src/ directories
        if path.contains("/src/") || path.starts_with("src/") {
            score += 3.0;
        }

        // Penalty for hidden files/directories
        if path.contains("/.") || path.starts_with('.') {
            score -= 5.0;
        }

        // Penalty for common non-essential directories
        if path.contains("/node_modules/")
            || path.contains("/target/")
            || path.contains("/__pycache__/")
            || path.contains("/.git/")
            || path.contains("/vendor/")
        {
            score -= 10.0;
        }

        // Bonus for important filenames
        let filename = path_obj
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if filename == "main.rs"
            || filename == "lib.rs"
            || filename == "mod.rs"
            || filename == "main.py"
            || filename == "index.js"
            || filename == "index.ts"
        {
            score += 5.0;
        }

        if filename == "cargo.toml"
            || filename == "package.json"
            || filename == "pyproject.toml"
            || filename == "readme.md"
        {
            score += 3.0;
        }

        score
    }

    /// Calculate probability scores for a batch of paths
    pub fn calculate_path_probabilities_batch(&self, paths: &[String]) -> Vec<(f32, String)> {
        let mut results: Vec<(f32, String)> = paths
            .iter()
            .map(|path| {
                let score = self.calculate_path_probability(path);
                (score, path.clone())
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Rank paths by probability and return the top N
    pub fn rank_paths(&self, paths: &[String], top_n: usize) -> Vec<String> {
        let scored = self.calculate_path_probabilities_batch(paths);
        scored.into_iter().take(top_n).map(|(_, p)| p).collect()
    }

    /// Filter paths by minimum score threshold
    pub fn filter_by_threshold(&self, paths: &[String], threshold: f32) -> Vec<String> {
        paths
            .iter()
            .filter(|p| self.calculate_path_probability(p) >= threshold)
            .cloned()
            .collect()
    }

    /// Filter paths by relevance level
    pub fn filter_by_relevance_level(
        &self,
        paths: &[String],
        level: RelevanceLevel,
    ) -> Vec<String> {
        let threshold = match level {
            RelevanceLevel::High => HIGH_RELEVANCE_THRESHOLD,
            RelevanceLevel::Medium => MEDIUM_RELEVANCE_THRESHOLD,
            RelevanceLevel::Low => LOW_RELEVANCE_THRESHOLD,
            RelevanceLevel::Custom(t) => t as f32,
        };
        self.filter_by_threshold(paths, threshold)
    }

    /// Group paths by relevance level
    pub fn group_by_relevance(&self, paths: &[String]) -> RelevanceGroups {
        let mut groups = RelevanceGroups::default();

        for path in paths {
            let score = self.calculate_path_probability(path);
            if score >= HIGH_RELEVANCE_THRESHOLD {
                groups.high.push(path.clone());
            } else if score >= MEDIUM_RELEVANCE_THRESHOLD {
                groups.medium.push(path.clone());
            } else {
                groups.low.push(path.clone());
            }
        }

        groups
    }

    /// Set context tokens (no-op in simplified version)
    pub fn set_context_tokens(&mut self, _tokens: Vec<String>) {
        debug!("Context tokens not used in simplified path analyzer");
    }

    /// Extract context from files (no-op in simplified version)
    pub fn extract_context_from_files(
        &mut self,
        _files: &[String],
        _workspace_stats: Option<&crate::utils::repo::WorkspaceStats>,
    ) {
        debug!("Context extraction not used in simplified path analyzer");
    }
}

impl Default for PathScorer {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a default PathScorer instance
pub fn create_default_path_scorer() -> Result<PathScorer> {
    Ok(PathScorer::new())
}

/// Create a context-aware path scorer (simplified - ignores context)
pub fn create_context_aware_path_scorer(
    _recent_files: &[String],
    _workspace_stats: Option<&crate::utils::repo::WorkspaceStats>,
) -> Result<PathScorer> {
    Ok(PathScorer::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_scoring() {
        let scorer = PathScorer::new();

        let score_main = scorer.calculate_path_probability("src/main.rs");
        let score_lock = scorer.calculate_path_probability("Cargo.lock");
        let score_deep = scorer.calculate_path_probability("a/b/c/d/e/f/file.rs");

        assert!(score_main > score_lock);
        assert!(score_main > score_deep);
    }

    #[test]
    fn test_batch_scoring() {
        let scorer = PathScorer::new();
        let paths = vec![
            "src/main.rs".to_string(),
            "Cargo.lock".to_string(),
            "README.md".to_string(),
        ];

        let results = scorer.calculate_path_probabilities_batch(&paths);
        assert_eq!(results.len(), 3);
        // main.rs should be first (highest score)
        assert!(results[0].1.contains("main.rs"));
    }
}
