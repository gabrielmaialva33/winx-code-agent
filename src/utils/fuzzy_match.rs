//! Fuzzy matching module.
//!
//! This module provides advanced fuzzy matching algorithms for text comparison,
//! particularly useful for code search and replace operations.

use rayon::prelude::*;
use std::cmp::{max, min};
use std::collections::HashMap;
#[allow(unused_imports)]
use tracing::debug;

/// Levenshtein distance threshold for considering strings similar
pub const DEFAULT_LEVENSHTEIN_THRESHOLD: f64 = 0.8;

/// Minimum length of token to consider for token-based matching
pub const MIN_TOKEN_LENGTH: usize = 3;

/// Represents a fuzzy match result with similarity score and matched text
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// The matching text
    pub text: String,
    /// Similarity score (0.0-1.0, higher is better)
    pub similarity: f64,
    /// The start position of the match
    pub start_pos: usize,
    /// The end position of the match
    pub end_pos: usize,
    /// Match type (algorithm used)
    pub match_type: MatchType,
}

/// Type of matching algorithm that produced the result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchType {
    /// Exact matching (100% match)
    Exact,
    /// Levenshtein edit distance
    Levenshtein,
    /// Normalized whitespace matching
    NormalizedWhitespace,
    /// Line-by-line partial matching
    LineByLine,
    /// Token-based matching
    TokenBased,
    /// Longest common substring
    LongestCommonSubstring,
    /// Case-insensitive matching
    CaseInsensitive,
    /// AST-based matching (for code)
    AstBased,
}

impl FuzzyMatch {
    /// Create a new fuzzy match result
    pub fn new(
        text: String,
        similarity: f64,
        start_pos: usize,
        end_pos: usize,
        match_type: MatchType,
    ) -> Self {
        Self {
            text,
            similarity,
            start_pos,
            end_pos,
            match_type,
        }
    }

    /// Create a new exact match result
    pub fn exact(text: String, start_pos: usize, end_pos: usize) -> Self {
        Self::new(text, 1.0, start_pos, end_pos, MatchType::Exact)
    }
}

/// Fuzzy matcher configuration
#[derive(Debug, Clone)]
pub struct FuzzyMatcherConfig {
    /// Whether to use case-insensitive matching
    pub case_insensitive: bool,
    /// Whether to normalize whitespace
    pub normalize_whitespace: bool,
    /// Whether to use token-based matching
    pub use_token_matching: bool,
    /// Whether to use line-by-line matching
    pub use_line_matching: bool,
    /// Whether to use Levenshtein distance
    pub use_levenshtein: bool,
    /// Whether to use longest common substring
    pub use_longest_common_substring: bool,
    /// Levenshtein similarity threshold (0.0-1.0)
    pub levenshtein_threshold: f64,
    /// Maximum number of top matches to return
    pub max_matches: usize,
    /// Whether to use parallel processing
    pub use_parallel: bool,
    /// Whether to use AST-based matching for code
    pub use_ast_matching: bool,
}

impl Default for FuzzyMatcherConfig {
    fn default() -> Self {
        Self {
            case_insensitive: true,
            normalize_whitespace: true,
            use_token_matching: true,
            use_line_matching: true,
            use_levenshtein: true,
            use_longest_common_substring: true,
            levenshtein_threshold: DEFAULT_LEVENSHTEIN_THRESHOLD,
            max_matches: 5,
            use_parallel: true,
            use_ast_matching: false, // Off by default as it's more expensive
        }
    }
}

/// High-performance fuzzy matcher implementation
#[derive(Debug, Clone)]
pub struct FuzzyMatcher {
    /// Configuration for the matcher
    config: FuzzyMatcherConfig,
    /// Token cache for performance optimization
    token_cache: HashMap<String, Vec<String>>,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher {
    /// Create a new fuzzy matcher with default configuration
    pub fn new() -> Self {
        Self {
            config: FuzzyMatcherConfig::default(),
            token_cache: HashMap::new(),
        }
    }

    /// Create a new fuzzy matcher with custom configuration
    pub fn with_config(config: FuzzyMatcherConfig) -> Self {
        Self {
            config,
            token_cache: HashMap::new(),
        }
    }

    /// Set the case sensitivity
    pub fn case_insensitive(mut self, value: bool) -> Self {
        self.config.case_insensitive = value;
        self
    }

    /// Set whitespace normalization
    pub fn normalize_whitespace(mut self, value: bool) -> Self {
        self.config.normalize_whitespace = value;
        self
    }

    /// Set Levenshtein threshold
    pub fn levenshtein_threshold(mut self, value: f64) -> Self {
        self.config.levenshtein_threshold = value.clamp(0.0, 1.0);
        self
    }

    /// Find fuzzy matches for a pattern in text
    pub fn find_matches(&mut self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        if pattern.is_empty() || text.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();

        // Try exact match first (fastest)
        if let Some(pos) = text.find(pattern) {
            matches.push(FuzzyMatch::exact(
                pattern.to_string(),
                pos,
                pos + pattern.len(),
            ));
            return matches;
        }

        // Apply various fuzzy matching strategies based on configuration
        let mut all_matches: Vec<FuzzyMatch> = Vec::new();

        // Run strategies directly instead of using closures
        if self.config.case_insensitive {
            all_matches.extend(self.find_case_insensitive_matches(pattern, text));
        }

        if self.config.normalize_whitespace {
            all_matches.extend(self.find_normalized_whitespace_matches(pattern, text));
        }

        if self.config.use_line_matching {
            all_matches.extend(self.find_line_matches(pattern, text));
        }

        if self.config.use_token_matching {
            all_matches.extend(self.find_token_matches(pattern, text));
        }

        if self.config.use_levenshtein {
            all_matches.extend(self.find_levenshtein_matches(pattern, text));
        }

        if self.config.use_longest_common_substring {
            all_matches.extend(self.find_longest_common_substring_matches(pattern, text));
        }

        // Since we've already collected all the matches, we don't need this section
        // Sort the matches by relevance/similarity

        // Sort by similarity (highest first) and take top matches
        all_matches.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_matches.truncate(self.config.max_matches);

        all_matches
    }

    /// Check if the pattern approximately matches the text
    pub fn is_match(&mut self, pattern: &str, text: &str) -> bool {
        !self.find_matches(pattern, text).is_empty()
    }

    /// Get the best match for a pattern in text
    pub fn best_match(&mut self, pattern: &str, text: &str) -> Option<FuzzyMatch> {
        let matches = self.find_matches(pattern, text);
        matches.into_iter().next()
    }

    /// Find case-insensitive matches
    fn find_case_insensitive_matches(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        let pattern_lower = pattern.to_lowercase();
        let text_lower = text.to_lowercase();

        if pattern_lower == text_lower {
            // Perfect case-insensitive match
            return vec![FuzzyMatch::new(
                pattern.to_string(),
                1.0,
                0,
                text.len(),
                MatchType::CaseInsensitive,
            )];
        }

        let mut matches = Vec::new();
        let mut start = 0;

        while let Some(pos) = text_lower[start..].find(&pattern_lower) {
            let abs_pos = start + pos;
            let end_pos = abs_pos + pattern.len();

            // Ensure indices are valid
            if abs_pos < text.len() {
                let actual_text = if end_pos <= text.len() {
                    text[abs_pos..end_pos].to_string()
                } else {
                    text[abs_pos..].to_string()
                };

                matches.push(FuzzyMatch::new(
                    actual_text,
                    0.9, // Slightly lower than exact match
                    abs_pos,
                    end_pos.min(text.len()),
                    MatchType::CaseInsensitive,
                ));
            }

            start = abs_pos + 1;
            if start >= text.len() {
                break;
            }
        }

        matches
    }

    /// Find matches with normalized whitespace
    fn find_normalized_whitespace_matches(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        // Normalize whitespace in pattern and text
        let pattern_norm = normalize_whitespace(pattern);
        let text_norm = normalize_whitespace(text);

        // Build a mapping from normalized positions to original positions
        let pos_mapping = build_position_mapping(text, &text_norm);

        let mut matches = Vec::new();
        let mut start = 0;

        while let Some(pos) = text_norm[start..].find(&pattern_norm) {
            let abs_pos = start + pos;
            let end_pos = abs_pos + pattern_norm.len();

            // Convert normalized positions back to original positions
            let orig_start = pos_mapping.get(&abs_pos).copied().unwrap_or(0);
            let orig_end = pos_mapping.get(&end_pos).copied().unwrap_or(text.len());

            // Ensure bounds are valid
            if orig_start < text.len() && orig_start <= orig_end && orig_end <= text.len() {
                let actual_text = text[orig_start..orig_end].to_string();

                matches.push(FuzzyMatch::new(
                    actual_text,
                    0.85, // Slightly lower than case-insensitive
                    orig_start,
                    orig_end,
                    MatchType::NormalizedWhitespace,
                ));
            }

            start = abs_pos + 1;
            if start >= text_norm.len() {
                break;
            }
        }

        matches
    }

    /// Find matches line by line
    fn find_line_matches(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        let pattern_lines: Vec<&str> = pattern.lines().collect();
        let text_lines: Vec<&str> = text.lines().collect();

        if pattern_lines.is_empty() || text_lines.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();

        // For each possible starting position in text_lines
        for start_idx in 0..=text_lines.len().saturating_sub(pattern_lines.len()) {
            let mut matching_lines = 0;
            let mut total_lines = 0;

            // Check how many lines match from this starting position
            for (p_idx, p_line) in pattern_lines.iter().enumerate() {
                let p_line_trim = p_line.trim();
                if p_line_trim.is_empty() {
                    continue; // Skip empty lines
                }

                total_lines += 1;
                let t_idx = start_idx + p_idx;

                if t_idx < text_lines.len() {
                    let t_line = text_lines[t_idx];
                    let t_line_trim = t_line.trim();

                    if t_line_trim == p_line_trim || t_line.contains(p_line_trim) {
                        matching_lines += 1;
                    }
                }
            }

            // Calculate similarity score based on matching lines
            if total_lines > 0 && matching_lines > 0 {
                let similarity = matching_lines as f64 / total_lines as f64;

                // Only include matches with reasonable similarity
                if similarity >= 0.5 {
                    // Calculate original text positions
                    let start_pos = if start_idx > 0 {
                        text.lines()
                            .take(start_idx)
                            .map(|l| l.len() + 1) // +1 for newline
                            .sum()
                    } else {
                        0
                    };

                    let end_lines = min(start_idx + pattern_lines.len(), text_lines.len());
                    let end_pos = if end_lines > 0 {
                        text.lines()
                            .take(end_lines)
                            .map(|l| l.len() + 1) // +1 for newline
                            .sum()
                    } else {
                        text.len()
                    };

                    // Ensure start_pos is less than or equal to end_pos
                    if start_pos <= end_pos && end_pos <= text.len() {
                        let matched_text = text_lines[start_idx..end_lines].join("\n");

                        matches.push(FuzzyMatch::new(
                            matched_text,
                            similarity,
                            start_pos,
                            end_pos,
                            MatchType::LineByLine,
                        ));
                    }
                }
            }
        }

        matches
    }

    /// Find matches using token-based approach
    fn find_token_matches(&mut self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        // Extract tokens from pattern and text
        let pattern_tokens = self.tokenize(pattern);
        let text_tokens = self.tokenize(text);

        if pattern_tokens.is_empty() || text_tokens.is_empty() {
            return Vec::new();
        }

        // Create a token frequency map for the pattern
        let mut pattern_token_freq = HashMap::new();
        for token in &pattern_tokens {
            *pattern_token_freq.entry(token.to_string()).or_insert(0) += 1;
        }

        // Calculate token matches and scores for sliding windows in the text
        let mut matches = Vec::new();
        let window_size = pattern_tokens.len() * 2; // Use a larger window to capture context

        for window_start in 0..=text_tokens.len().saturating_sub(window_size) {
            let window_end = (window_start + window_size).min(text_tokens.len());
            let window = &text_tokens[window_start..window_end];

            // Count matching tokens in the window
            let mut matched_tokens = 0;
            let mut token_matches = HashMap::new();

            for token in window {
                if pattern_token_freq.contains_key(token) {
                    let count = token_matches.entry(token.to_string()).or_insert(0);
                    *count += 1;

                    if *count <= pattern_token_freq[token] {
                        matched_tokens += 1;
                    }
                }
            }

            // Calculate similarity score
            let similarity = (matched_tokens as f64 / pattern_tokens.len() as f64).min(1.0); // Cap at 1.0

            if similarity >= 0.6 {
                // Find the actual text positions for this window
                let start_pos = find_token_position(text, &text_tokens[window_start]);
                let end_pos = if window_end < text_tokens.len() {
                    find_token_position(text, &text_tokens[window_end - 1])
                        .map(|pos| pos + text_tokens[window_end - 1].len())
                } else {
                    Some(text.len())
                };

                if let Some(start) = start_pos {
                    let end = end_pos.unwrap_or(text.len());
                    // Ensure end index is not less than start index
                    if start <= end && end <= text.len() {
                        let actual_text = text[start..end].to_string();
                        matches.push(FuzzyMatch::new(
                            actual_text,
                            similarity,
                            start,
                            end,
                            MatchType::TokenBased,
                        ));
                    }
                }
            }
        }

        matches
    }

    /// Find matches using Levenshtein distance
    fn find_levenshtein_matches(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        // For very long texts, use a sliding window approach
        if text.len() > pattern.len() * 10 {
            return self.find_levenshtein_matches_windowed(pattern, text);
        }

        // For smaller texts, just compare directly
        let distance = levenshtein_distance(pattern, text);
        let max_distance = max(pattern.len(), text.len());
        let similarity = if max_distance > 0 {
            1.0 - (distance as f64 / max_distance as f64)
        } else {
            0.0
        };

        if similarity >= self.config.levenshtein_threshold {
            vec![FuzzyMatch::new(
                text.to_string(),
                similarity,
                0,
                text.len(),
                MatchType::Levenshtein,
            )]
        } else {
            Vec::new()
        }
    }

    /// Find Levenshtein matches using a sliding window approach for large texts
    fn find_levenshtein_matches_windowed(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        let mut matches = Vec::new();
        let pattern_len = pattern.len();
        let window_size = pattern_len * 2; // Use a larger window to capture context

        // Skip if pattern is too small or text can't fit a window
        if pattern_len < 3 || text.len() < window_size {
            return matches;
        }

        // Sample the text at regular intervals to avoid checking every possible window
        let stride = max(1, pattern_len / 2);
        let mut windows = Vec::new();

        // Collect windows to process
        let mut start_pos = 0;
        while start_pos + window_size <= text.len() {
            let window_text = &text[start_pos..start_pos + window_size];
            windows.push((start_pos, window_text));
            start_pos += stride;
        }

        // Add final window if needed
        if start_pos < text.len() && text.len() - start_pos >= pattern_len {
            let window_text = &text[start_pos..];
            windows.push((start_pos, window_text));
        }

        // Process windows in parallel if configured to do so
        type FuzzyMatchArray = Vec<FuzzyMatch>;
        let window_matches: FuzzyMatchArray = if self.config.use_parallel && windows.len() > 10 {
            windows
                .into_par_iter()
                .filter_map(|(start_pos, window_text)| {
                    let distance = levenshtein_distance(pattern, window_text);
                    let max_distance = max(pattern.len(), window_text.len());
                    let similarity = if max_distance > 0 {
                        1.0 - (distance as f64 / max_distance as f64)
                    } else {
                        0.0
                    };

                    let end_pos = start_pos + window_text.len();
                    if similarity >= self.config.levenshtein_threshold
                        && start_pos <= end_pos
                        && end_pos <= text.len()
                    {
                        Some(FuzzyMatch::new(
                            window_text.to_string(),
                            similarity,
                            start_pos,
                            end_pos,
                            MatchType::Levenshtein,
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            windows
                .into_iter()
                .filter_map(|(start_pos, window_text)| {
                    let distance = levenshtein_distance(pattern, window_text);
                    let max_distance = max(pattern.len(), window_text.len());
                    let similarity = if max_distance > 0 {
                        1.0 - (distance as f64 / max_distance as f64)
                    } else {
                        0.0
                    };

                    let end_pos = start_pos + window_text.len();
                    if similarity >= self.config.levenshtein_threshold
                        && start_pos <= end_pos
                        && end_pos <= text.len()
                    {
                        Some(FuzzyMatch::new(
                            window_text.to_string(),
                            similarity,
                            start_pos,
                            end_pos,
                            MatchType::Levenshtein,
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        };

        matches.extend(window_matches);
        matches
    }

    /// Find matches using longest common substring
    fn find_longest_common_substring_matches(&self, pattern: &str, text: &str) -> Vec<FuzzyMatch> {
        // For very large strings, use a simplified approach
        if pattern.len() > 10000 || text.len() > 100000 {
            return self.find_longest_common_substring_matches_simplified(pattern, text);
        }

        // Use the dynamic programming approach for standard sizes
        let s1_chars: Vec<char> = pattern.chars().collect();
        let s2_chars: Vec<char> = text.chars().collect();

        let m = s1_chars.len();
        let n = s2_chars.len();

        // Early return for empty strings
        if m == 0 || n == 0 {
            return Vec::new();
        }

        // Build the dynamic programming table
        let mut dp = vec![vec![0; n + 1]; m + 1];
        let mut max_length = 0;
        let mut end_pos_in_s1 = 0;
        let mut end_pos_in_s2 = 0;

        for i in 1..=m {
            for j in 1..=n {
                if s1_chars[i - 1] == s2_chars[j - 1] {
                    dp[i][j] = dp[i - 1][j - 1] + 1;

                    if dp[i][j] > max_length {
                        max_length = dp[i][j];
                        end_pos_in_s1 = i;
                        end_pos_in_s2 = j;
                    }
                }
            }
        }

        // If we found a substantial common substring
        if max_length >= min(10, pattern.len() / 2) {
            let start_pos_in_s1 = end_pos_in_s1 - max_length;
            let start_pos_in_s2 = end_pos_in_s2 - max_length;

            // Validate indices before creating substring
            if start_pos_in_s1 < s1_chars.len()
                && end_pos_in_s1 <= s1_chars.len()
                && start_pos_in_s2 < s2_chars.len()
                && end_pos_in_s2 <= s2_chars.len()
                && start_pos_in_s1 <= end_pos_in_s1
                && start_pos_in_s2 <= end_pos_in_s2
            {
                let common_str: String = s1_chars[start_pos_in_s1..end_pos_in_s1].iter().collect();

                // Calculate similarity based on coverage of pattern
                let similarity = max_length as f64 / pattern.len() as f64;

                // Only include matches with reasonable similarity
                if similarity >= 0.4 {
                    vec![FuzzyMatch::new(
                        common_str,
                        similarity,
                        start_pos_in_s2,
                        end_pos_in_s2,
                        MatchType::LongestCommonSubstring,
                    )]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    /// Simplified version for extremely large strings
    fn find_longest_common_substring_matches_simplified(
        &self,
        pattern: &str,
        text: &str,
    ) -> Vec<FuzzyMatch> {
        let min_length = min(20, pattern.len() / 2);

        // Try different window sizes for pattern
        for window_size in [50, 40, 30, 20].iter() {
            if pattern.len() < *window_size {
                continue;
            }

            let pattern_chars: Vec<char> = pattern.chars().collect();

            // Sample windows from pattern at regular intervals
            let stride = max(1, *window_size / 2);
            let mut windows = Vec::new();

            let mut start_pos = 0;
            while start_pos + window_size <= pattern_chars.len() {
                let window: String = pattern_chars[start_pos..start_pos + window_size]
                    .iter()
                    .collect();
                windows.push((start_pos, window));
                start_pos += stride;
            }

            // Try to find each window in the text
            for (_pattern_pos, window) in windows {
                if let Some(text_pos) = text.find(&window) {
                    // We found a match - this is our longest common substring for this window size
                    let pattern_len = pattern.len();
                    let min_size = std::cmp::min(*window_size, pattern_len);
                    let similarity = min_size as f64 / pattern_len as f64;

                    let end_pos = text_pos + window_size;
                    // Ensure indices are valid
                    if text_pos <= end_pos && end_pos <= text.len() {
                        return vec![FuzzyMatch::new(
                            window,
                            similarity,
                            text_pos,
                            end_pos,
                            MatchType::LongestCommonSubstring,
                        )];
                    }
                }
            }
        }

        // Try even smaller windows as a last resort
        let fallback_sizes = [15, 10, min_length];
        for &size in &fallback_sizes {
            if pattern.len() < size {
                continue;
            }

            let pattern_chars: Vec<char> = pattern.chars().collect();

            // Try several positions in the pattern
            for start in [0, pattern.len() / 4, pattern.len() / 2] {
                if start + size > pattern_chars.len() {
                    continue;
                }

                let window: String = pattern_chars[start..start + size].iter().collect();

                if let Some(text_pos) = text.find(&window) {
                    // We found a smaller match
                    let similarity = size as f64 / pattern.len() as f64;

                    let end_pos = text_pos + size;
                    // Ensure indices are valid
                    if text_pos <= end_pos && end_pos <= text.len() {
                        return vec![FuzzyMatch::new(
                            window,
                            similarity,
                            text_pos,
                            end_pos,
                            MatchType::LongestCommonSubstring,
                        )];
                    }
                }
            }
        }

        Vec::new()
    }

    /// Tokenize a string into meaningful tokens for matching
    fn tokenize(&mut self, text: &str) -> Vec<String> {
        // Check cache first
        if let Some(tokens) = self.token_cache.get(text) {
            return tokens.clone();
        }

        // Split by common delimiters (spaces, punctuation, etc.)
        let delimiters = &[
            ' ', '\t', '\n', '\r', ',', '.', ';', ':', '(', ')', '[', ']', '{', '}', '<', '>', '=',
            '+', '-', '*', '/', '&', '|', '!',
        ];

        let mut tokens = Vec::new();
        let mut current_token = String::new();

        for c in text.chars() {
            if delimiters.contains(&c) {
                // End of token
                if !current_token.is_empty() {
                    if current_token.len() >= MIN_TOKEN_LENGTH {
                        tokens.push(current_token.clone());
                    }
                    current_token.clear();
                }

                // Include delimiters that might be significant in code
                if matches!(c, '{' | '}' | '(' | ')' | '[' | ']' | '<' | '>') {
                    tokens.push(c.to_string());
                }
            } else {
                current_token.push(c);
            }
        }

        // Don't forget the last token
        if !current_token.is_empty() && current_token.len() >= MIN_TOKEN_LENGTH {
            tokens.push(current_token);
        }

        // Cache the result
        self.token_cache.insert(text.to_string(), tokens.clone());

        tokens
    }
}

/// Compute Levenshtein edit distance between two strings
pub fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    if s1.is_empty() {
        return s2.len();
    }
    if s2.is_empty() {
        return s1.len();
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let m = s1_chars.len();
    let n = s2_chars.len();

    // Initialize cost matrix
    // We only need two rows, the current one and the previous one
    let mut prev_row = Vec::with_capacity(n + 1);
    let mut curr_row = Vec::with_capacity(n + 1);

    // Initialize the first row
    for j in 0..=n {
        prev_row.push(j);
    }

    // Fill in the rest of the matrix
    for i in 1..=m {
        curr_row.clear();
        curr_row.push(i); // First column

        for j in 1..=n {
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
                0
            } else {
                1
            };

            // Choose the minimum of:
            // - Delete (j-1)th character from s2 (cell to the left + 1)
            // - Insert (i-1)th character of s1 into s2 (cell above + 1)
            // - Substitute (i-1)th character of s1 for (j-1)th character of s2 (diagonal + cost)
            let deletion = curr_row[j - 1] + 1;
            let insertion = prev_row[j] + 1;
            let substitution = prev_row[j - 1] + cost;

            curr_row.push(min(min(deletion, insertion), substitution));
        }

        // Swap rows for next iteration
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    // The result is in the last element of the previous row
    prev_row[n]
}

/// Normalize whitespace in text
pub fn normalize_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_whitespace = false;

    for c in text.chars() {
        if c.is_whitespace() {
            if !last_was_whitespace {
                result.push(' '); // Replace all whitespace with a single space
                last_was_whitespace = true;
            }
        } else {
            result.push(c);
            last_was_whitespace = false;
        }
    }

    result
}

/// Build a mapping from normalized text positions to original text positions
fn build_position_mapping(original: &str, normalized: &str) -> HashMap<usize, usize> {
    let mut mapping = HashMap::new();
    let mut orig_pos = 0;
    let mut norm_pos = 0;

    let orig_chars: Vec<char> = original.chars().collect();
    let norm_chars: Vec<char> = normalized.chars().collect();

    while orig_pos < orig_chars.len() && norm_pos < norm_chars.len() {
        if orig_chars[orig_pos].is_whitespace() {
            // Skip consecutive whitespace in original
            orig_pos += 1;
        } else if norm_chars[norm_pos].is_whitespace() {
            // Skip consecutive whitespace in normalized
            mapping.insert(norm_pos, orig_pos);
            norm_pos += 1;
        } else {
            // Map this position
            mapping.insert(norm_pos, orig_pos);
            orig_pos += 1;
            norm_pos += 1;
        }
    }

    // Map any remaining positions
    while norm_pos < norm_chars.len() {
        mapping.insert(norm_pos, orig_pos.min(orig_chars.len()));
        norm_pos += 1;
    }

    mapping
}

/// Find the position of a token in the original text
fn find_token_position(text: &str, token: &str) -> Option<usize> {
    text.find(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("", ""), 0);
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
        assert_eq!(levenshtein_distance("saturday", "sunday"), 3);
    }

    #[test]
    fn test_normalize_whitespace() {
        assert_eq!(normalize_whitespace("abc"), "abc");
        assert_eq!(normalize_whitespace(" abc "), " abc ");
        assert_eq!(normalize_whitespace("  abc  def  "), " abc def ");
        assert_eq!(normalize_whitespace("abc\ndef\t\tghi"), "abc def ghi");
    }

    #[test]
    fn test_exact_match() {
        let mut matcher = FuzzyMatcher::new();
        let matches = matcher.find_matches("abc", "abc def abc ghi");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].match_type, MatchType::Exact);
        assert_eq!(matches[0].start_pos, 0);
        assert_eq!(matches[0].end_pos, 3);
    }

    #[test]
    fn test_case_insensitive_match() {
        let mut matcher = FuzzyMatcher::new();
        let matches = matcher.find_matches("ABC", "abc def ABC ghi");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].match_type, MatchType::Exact);

        // Test when no exact match exists
        let matches = matcher.find_matches("ABC", "abc def ghi");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].match_type, MatchType::CaseInsensitive);
        assert_eq!(matches[0].start_pos, 0);
        assert_eq!(matches[0].end_pos, 3);
    }

    #[test]
    fn test_whitespace_normalization() {
        let mut matcher = FuzzyMatcher::new();
        let matches = matcher.find_matches("abc   def", "abc def");

        // Should find at least one match with NormalizedWhitespace type
        assert!(!matches.is_empty());
        assert!(matches
            .iter()
            .any(|m| m.match_type == MatchType::NormalizedWhitespace));
    }

    #[test]
    fn test_line_matching() {
        let mut matcher = FuzzyMatcher::new();
        let pattern = "line 1\nline 2\nline 3";
        let text = "some text\nline 1\nline 2\nmodified line 3\nmore text";

        let matches = matcher.find_matches(pattern, text);
        assert!(matches
            .iter()
            .any(|m| m.match_type == MatchType::LineByLine));
    }

    #[test]
    fn test_longest_common_substring() {
        let mut matcher = FuzzyMatcher::new();
        let pattern = "function calculateTotal(items) {\n    return items.reduce((sum, item) => sum + item.price, 0);\n}";
        let text = "function calculateSubtotal(items) {\n    return items.reduce((sum, item) => sum + item.price * item.quantity, 0);\n}";

        let matches = matcher.find_matches(pattern, text);
        assert!(matches
            .iter()
            .any(|m| m.match_type == MatchType::LongestCommonSubstring));
    }

    #[test]
    fn test_token_matching() {
        let mut matcher = FuzzyMatcher::new();
        let pattern = "function process(data) { return transform(data); }";
        let text = "// Process function\nfunction processData(data) {\n    const result = transform(data);\n    return result;\n}";

        let matches = matcher.find_matches(pattern, text);
        assert!(matches
            .iter()
            .any(|m| m.match_type == MatchType::TokenBased));
    }
}
