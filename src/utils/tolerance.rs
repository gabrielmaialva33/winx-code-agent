//! WCGW-style tolerance system for search/replace matching.
//!
//! This module implements a 5-level tolerance system for matching search blocks
//! in file content, inspired by the wcgw Python implementation.
//!
//! ## Optimization: Tiered Matching with Early Exit (v0.2.2)
//!
//! The matching algorithm uses a tiered approach with early exit:
//! - **Tier 0**: Exact match (fastest, no tolerance)
//! - **Tier 1-5**: Progressive tolerance application with early exit
//!
//! When a unique match is found at any tier, processing stops immediately.
//! This dramatically reduces processing time for well-formatted code.
//!
//! Tolerance levels (applied in order):
//! 1. `rstrip` - Remove trailing whitespace (SILENT)
//! 2. `lstrip` - Remove leading indentation (WARNING, score 10x)
//! 3. `remove_leading_linenums` - Remove `^\d+ ` patterns (WARNING, score 5x)
//! 4. `normalize_common_mistakes` - Unicode to ASCII normalization (WARNING, score 5x)
//! 5. `all_whitespace` - Remove ALL whitespace (WARNING, score 50x)

use regex::Regex;
use std::collections::{HashMap, HashSet};
use tracing::{debug, trace, warn};

/// Severity categories for tolerances
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToleranceSeverity {
    /// Silent - no warning generated
    Silent,
    /// Warning - generates a warning message
    Warning,
    /// Error - generates an error and stops processing
    Error,
}

/// Common character translations from Unicode to ASCII
/// Based on wcgw's `COMMON_MISTAKE_TRANSLATION`
pub const COMMON_MISTAKES: &[(&str, &str)] = &[
    // Smart quotes to ASCII
    ("\u{2018}", "'"),  // LEFT SINGLE QUOTATION MARK
    ("\u{2019}", "'"),  // RIGHT SINGLE QUOTATION MARK
    ("\u{201A}", ","),  // SINGLE LOW-9 QUOTATION MARK
    ("\u{201B}", "'"),  // SINGLE HIGH-REVERSED-9 QUOTATION MARK
    ("\u{2032}", "'"),  // PRIME
    ("\u{201C}", "\""), // LEFT DOUBLE QUOTATION MARK
    ("\u{201D}", "\""), // RIGHT DOUBLE QUOTATION MARK
    ("\u{201F}", "\""), // DOUBLE HIGH-REVERSED-9 QUOTATION MARK
    ("\u{2033}", "\""), // DOUBLE PRIME
    ("\u{2039}", "<"),  // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
    ("\u{203A}", ">"),  // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
    // Dashes
    ("\u{2010}", "-"), // HYPHEN
    ("\u{2011}", "-"), // NON-BREAKING HYPHEN
    ("\u{2012}", "-"), // FIGURE DASH
    ("\u{2013}", "-"), // EN DASH
    ("\u{2014}", "-"), // EM DASH
    ("\u{2015}", "-"), // HORIZONTAL BAR
    ("\u{2212}", "-"), // MINUS SIGN
    // Ellipsis
    ("\u{2026}", "..."), // HORIZONTAL ELLIPSIS
];

/// Warning messages for different tolerance types
pub const REMOVE_INDENTATION_WARNING: &str =
    "Warning: matching without considering indentation (leading spaces).";
pub const REMOVE_LINE_NUMS_WARNING: &str =
    "Warning: you gave search/replace blocks with leading line numbers, do not give them from the next time.";
pub const NORMALIZE_CHARS_WARNING: &str =
    "Warning: matching after normalizing commonly confused characters (quotes, dashes, ellipsis).";
pub const REMOVE_ALL_WHITESPACE_WARNING: &str =
    "Warning: matching after removing all spaces in lines.";

/// A single tolerance level configuration
#[derive(Clone)]
pub struct Tolerance {
    /// Function to process a line
    pub line_process: fn(&str) -> String,
    /// Severity category for this tolerance
    pub severity: ToleranceSeverity,
    /// Score multiplier when this tolerance is used
    pub score_multiplier: f64,
    /// Error/warning name for reporting
    pub error_name: &'static str,
}

impl std::fmt::Debug for Tolerance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tolerance")
            .field("severity", &self.severity)
            .field("score_multiplier", &self.score_multiplier)
            .field("error_name", &self.error_name)
            .finish()
    }
}

/// Track which tolerance was hit and how many times
#[derive(Debug, Clone)]
pub struct ToleranceHit {
    /// The tolerance that was hit
    pub tolerance_index: usize,
    /// Severity of the tolerance
    pub severity: ToleranceSeverity,
    /// Score multiplier
    pub score_multiplier: f64,
    /// Error/warning name
    pub error_name: &'static str,
    /// Number of times this tolerance was hit
    pub count: usize,
}

impl ToleranceHit {
    /// Create a new tolerance hit
    pub fn new(tolerance: &Tolerance, index: usize) -> Self {
        Self {
            tolerance_index: index,
            severity: tolerance.severity,
            score_multiplier: tolerance.score_multiplier,
            error_name: tolerance.error_name,
            count: 0,
        }
    }

    /// Calculate the score contribution from this hit
    pub fn score(&self) -> f64 {
        self.count as f64 * self.score_multiplier
    }
}

/// Result of a match attempt
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// The slice of the original content that matched
    pub matched_slice: (usize, usize),
    /// Line range (`start_line`, `end_line`) - 1-indexed
    pub line_range: (usize, usize),
    /// Tolerances that were used to achieve the match
    pub tolerances_hit: Vec<ToleranceHit>,
    /// Total score for this match
    pub score: f64,
    /// Warnings generated during matching
    pub warnings: HashSet<String>,
    /// The actual matched lines from the file
    pub matched_lines: Vec<String>,
    /// Detected indentation removed (for lstrip tolerance)
    pub removed_indentation: Option<String>,
}

impl MatchResult {
    /// Check if this match used the lstrip tolerance
    pub fn used_lstrip(&self) -> bool {
        self.tolerances_hit
            .iter()
            .any(|t| t.error_name == REMOVE_INDENTATION_WARNING && t.count > 0)
    }

    /// Check if this match used the line numbers tolerance
    pub fn used_line_nums(&self) -> bool {
        self.tolerances_hit.iter().any(|t| t.error_name == REMOVE_LINE_NUMS_WARNING && t.count > 0)
    }
}

// ============================================================================
// Tolerance functions
// ============================================================================

/// Level 1: Remove trailing whitespace (rstrip)
pub fn apply_rstrip(text: &str) -> String {
    text.trim_end().to_string()
}

/// Level 2: Remove leading whitespace (lstrip)
pub fn apply_lstrip(text: &str) -> String {
    text.trim_start().to_string()
}

/// Level 2 variant: Get the removed indentation
pub fn get_removed_indentation(text: &str) -> String {
    let trimmed = text.trim_start();
    if trimmed.len() < text.len() {
        text[..text.len() - trimmed.len()].to_string()
    } else {
        String::new()
    }
}

/// Level 3: Remove leading line numbers (pattern: `^\d+ `)
pub fn remove_leading_linenums(text: &str) -> String {
    lazy_static::lazy_static! {
        static ref LINE_NUM_RE: Regex = Regex::new(r"^\d+\s").unwrap();
    }
    LINE_NUM_RE.replace(text, "").trim_end().to_string()
}

/// Level 4: Normalize common Unicode mistakes to ASCII
pub fn normalize_unicode(text: &str) -> String {
    let mut result = text.to_string();
    for (from, to) in COMMON_MISTAKES {
        result = result.replace(from, to);
    }
    result.trim_end().to_string()
}

/// Level 5: Remove ALL whitespace
pub fn remove_all_whitespace(text: &str) -> String {
    lazy_static::lazy_static! {
        static ref WHITESPACE_RE: Regex = Regex::new(r"\s").unwrap();
    }
    let stripped = text.trim();
    WHITESPACE_RE.replace_all(stripped, "").to_string()
}

// ============================================================================
// Default tolerances configuration
// ============================================================================

/// Get the default tolerances in order
pub fn default_tolerances() -> Vec<Tolerance> {
    vec![
        // Level 1: rstrip (SILENT)
        Tolerance {
            line_process: apply_rstrip,
            severity: ToleranceSeverity::Silent,
            score_multiplier: 1.0,
            error_name: "",
        },
        // Level 2: lstrip (WARNING, 10x)
        Tolerance {
            line_process: apply_lstrip,
            severity: ToleranceSeverity::Warning,
            score_multiplier: 10.0,
            error_name: REMOVE_INDENTATION_WARNING,
        },
        // Level 3: remove line numbers (WARNING, 5x)
        Tolerance {
            line_process: remove_leading_linenums,
            severity: ToleranceSeverity::Warning,
            score_multiplier: 5.0,
            error_name: REMOVE_LINE_NUMS_WARNING,
        },
        // Level 4: normalize Unicode (WARNING, 5x)
        Tolerance {
            line_process: normalize_unicode,
            severity: ToleranceSeverity::Warning,
            score_multiplier: 5.0,
            error_name: NORMALIZE_CHARS_WARNING,
        },
        // Level 5: remove all whitespace (WARNING, 50x)
        Tolerance {
            line_process: remove_all_whitespace,
            severity: ToleranceSeverity::Warning,
            score_multiplier: 50.0,
            error_name: REMOVE_ALL_WHITESPACE_WARNING,
        },
    ]
}

// ============================================================================
// Matching engine
// ============================================================================

/// Find all contiguous matches in content given line position sets
fn find_contiguous_matches(search_line_positions: &[HashSet<usize>]) -> Vec<(usize, usize)> {
    let n_search_lines = search_line_positions.len();
    if n_search_lines == 0 || search_line_positions[0].is_empty() {
        return vec![];
    }

    let mut matched_slices = Vec::new();

    for &start_index in &search_line_positions[0] {
        let mut valid = true;
        for (offset, positions) in search_line_positions.iter().enumerate().skip(1) {
            if !positions.contains(&(start_index + offset)) {
                valid = false;
                break;
            }
        }
        if valid {
            matched_slices.push((start_index, start_index + n_search_lines));
        }
    }

    matched_slices
}

/// Find exact matches in content
pub fn match_exact(
    content_lines: &[&str],
    content_offset: usize,
    search_lines: &[&str],
) -> Vec<(usize, usize)> {
    let n_search = search_lines.len();
    let n_content = content_lines.len();

    if n_search == 0 || n_content == 0 || n_search > n_content - content_offset {
        return vec![];
    }

    // Build position map for content lines
    let mut content_positions: HashMap<&str, HashSet<usize>> = HashMap::new();
    for (i, line) in content_lines.iter().enumerate().skip(content_offset) {
        content_positions.entry(line).or_default().insert(i);
    }

    // Get positions for each search line
    let search_line_positions: Vec<HashSet<usize>> = search_lines
        .iter()
        .map(|line| content_positions.get(line).cloned().unwrap_or_default())
        .collect();

    find_contiguous_matches(&search_line_positions)
}

// ============================================================================
// Tiered Matching with Early Exit (Optimized)
// ============================================================================

/// Result of tiered matching - includes which tier found the match
#[derive(Debug, Clone)]
pub struct TieredMatchResult {
    /// The match result
    pub result: MatchResult,
    /// Which tier (0-5) found the match (0 = exact, 1-5 = tolerance levels)
    pub tier: usize,
    /// Whether early exit was triggered
    pub early_exit: bool,
}

/// Tiered matching with early exit optimization
///
/// This is the PREFERRED matching function for performance.
/// It applies tolerances progressively and exits as soon as a unique match is found.
///
/// # Algorithm
/// 1. Try exact match first (Tier 0)
/// 2. If no unique match, apply tolerance level 1 (rstrip)
/// 3. If no unique match, apply tolerance level 2 (lstrip)
/// 4. Continue until unique match found OR all tiers exhausted
///
/// # Early Exit Conditions
/// - Exactly 1 match found at current tier → return immediately
/// - 0 matches at all tiers → return empty
/// - Multiple matches at final tier → return all (caller handles ambiguity)
pub fn match_tiered(
    content_lines: &[&str],
    content_offset: usize,
    search_lines: &[&str],
) -> Vec<TieredMatchResult> {
    let n_search = search_lines.len();
    let n_content = content_lines.len();

    if n_search == 0 || n_content == 0 || n_search > n_content - content_offset {
        return vec![];
    }

    // Tier 0: Exact match (fastest path)
    let exact_matches = match_exact(content_lines, content_offset, search_lines);
    if exact_matches.len() == 1 {
        let (start, end) = exact_matches[0];
        debug!("Tiered match: early exit at Tier 0 (exact match)");
        return vec![TieredMatchResult {
            result: MatchResult {
                matched_slice: (start, end),
                line_range: (start + 1, end),
                tolerances_hit: vec![],
                score: 0.0,
                warnings: HashSet::new(),
                matched_lines: content_lines[start..end].iter().map(|s| (*s).to_string()).collect(),
                removed_indentation: None,
            },
            tier: 0,
            early_exit: true,
        }];
    }

    // If multiple exact matches, return all (ambiguous)
    if exact_matches.len() > 1 {
        debug!("Tiered match: {} exact matches (ambiguous)", exact_matches.len());
        return exact_matches
            .into_iter()
            .map(|(start, end)| TieredMatchResult {
                result: MatchResult {
                    matched_slice: (start, end),
                    line_range: (start + 1, end),
                    tolerances_hit: vec![],
                    score: 0.0,
                    warnings: HashSet::new(),
                    matched_lines: content_lines[start..end]
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    removed_indentation: None,
                },
                tier: 0,
                early_exit: false,
            })
            .collect();
    }

    // Tier 1-5: Apply tolerances progressively with early exit
    let tolerances = default_tolerances();

    // Pre-process content lines once (avoid repeated allocations)
    let content_processed: Vec<Vec<String>> = tolerances
        .iter()
        .map(|tol| {
            content_lines.iter().skip(content_offset).map(|line| (tol.line_process)(line)).collect()
        })
        .collect();

    // Try each tolerance tier
    for (tier_idx, tolerance) in tolerances.iter().enumerate() {
        let tier = tier_idx + 1; // Tier 1-5 (0 was exact)

        // Process search lines with this tolerance
        let search_processed: Vec<String> =
            search_lines.iter().map(|line| (tolerance.line_process)(line)).collect();

        // Build position map for this tier
        let mut content_positions: HashMap<&str, HashSet<usize>> = HashMap::new();
        for (i, processed_line) in content_processed[tier_idx].iter().enumerate() {
            content_positions
                .entry(processed_line.as_str())
                .or_default()
                .insert(i + content_offset);
        }

        // Get positions for each search line
        let search_line_positions: Vec<HashSet<usize>> = search_processed
            .iter()
            .map(|line| content_positions.get(line.as_str()).cloned().unwrap_or_default())
            .collect();

        // Find contiguous matches at this tier
        let matched_slices = find_contiguous_matches(&search_line_positions);

        // Early exit: exactly 1 match found
        if matched_slices.len() == 1 {
            let (start, end) = matched_slices[0];
            debug!("Tiered match: early exit at Tier {} ({})", tier, tolerance.error_name);

            let matched_lines: Vec<String> =
                content_lines[start..end].iter().map(|s| (*s).to_string()).collect();

            // Check for removed indentation
            let removed_indentation = if tolerance.error_name == REMOVE_INDENTATION_WARNING {
                matched_lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| get_removed_indentation(l))
                    .next()
            } else {
                None
            };

            let mut warnings = HashSet::new();
            if tolerance.severity == ToleranceSeverity::Warning && !tolerance.error_name.is_empty()
            {
                warnings.insert(tolerance.error_name.to_string());
            }

            return vec![TieredMatchResult {
                result: MatchResult {
                    matched_slice: (start, end),
                    line_range: (start + 1, end),
                    tolerances_hit: vec![ToleranceHit {
                        tolerance_index: tier_idx,
                        severity: tolerance.severity,
                        score_multiplier: tolerance.score_multiplier,
                        error_name: tolerance.error_name,
                        count: n_search,
                    }],
                    score: tolerance.score_multiplier * n_search as f64,
                    warnings,
                    matched_lines,
                    removed_indentation,
                },
                tier,
                early_exit: true,
            }];
        }

        // Multiple matches at this tier - continue to next tier for more specificity
        // (unless this is the last tier)
        if !matched_slices.is_empty() && tier == tolerances.len() {
            trace!("Tiered match: {} matches at final tier {}", matched_slices.len(), tier);
            return matched_slices
                .into_iter()
                .map(|(start, end)| {
                    let matched_lines: Vec<String> =
                        content_lines[start..end].iter().map(|s| (*s).to_string()).collect();

                    let removed_indentation = if tolerance.error_name == REMOVE_INDENTATION_WARNING
                    {
                        matched_lines
                            .iter()
                            .filter(|l| !l.trim().is_empty())
                            .map(|l| get_removed_indentation(l))
                            .next()
                    } else {
                        None
                    };

                    let mut warnings = HashSet::new();
                    if tolerance.severity == ToleranceSeverity::Warning
                        && !tolerance.error_name.is_empty()
                    {
                        warnings.insert(tolerance.error_name.to_string());
                    }

                    TieredMatchResult {
                        result: MatchResult {
                            matched_slice: (start, end),
                            line_range: (start + 1, end),
                            tolerances_hit: vec![ToleranceHit {
                                tolerance_index: tier_idx,
                                severity: tolerance.severity,
                                score_multiplier: tolerance.score_multiplier,
                                error_name: tolerance.error_name,
                                count: n_search,
                            }],
                            score: tolerance.score_multiplier * n_search as f64,
                            warnings,
                            matched_lines,
                            removed_indentation,
                        },
                        tier,
                        early_exit: false,
                    }
                })
                .collect();
        }
    }

    // No matches found at any tier
    trace!("Tiered match: no matches found at any tier");
    vec![]
}

/// Match with tolerances applied
pub fn match_with_tolerance(
    content_lines: &[&str],
    content_offset: usize,
    search_lines: &[&str],
    tolerances: &[Tolerance],
) -> Vec<(MatchResult, Vec<usize>)> {
    let n_search = search_lines.len();
    let n_content = content_lines.len();

    if n_search == 0 || n_content == 0 || n_search > n_content - content_offset {
        return vec![];
    }

    // Build initial position map from exact matches
    let mut content_positions: HashMap<String, HashSet<usize>> = HashMap::new();
    for (i, line) in content_lines.iter().enumerate().skip(content_offset) {
        content_positions.entry((*line).to_string()).or_default().insert(i);
    }

    // Start with exact match positions
    let mut search_line_positions: Vec<HashSet<usize>> = search_lines
        .iter()
        .map(|line| content_positions.get(*line).cloned().unwrap_or_default())
        .collect();

    // Track which tolerance was used for each content line in each search position
    let mut tolerance_index_by_content_line: Vec<HashMap<usize, usize>> =
        vec![HashMap::new(); n_search];

    // Apply each tolerance level
    for (tidx, tolerance) in tolerances.iter().enumerate() {
        // Build content position map with this tolerance applied
        let mut content_positions_tol: HashMap<String, HashSet<usize>> = HashMap::new();
        for (i, line) in content_lines.iter().enumerate().skip(content_offset) {
            let processed = (tolerance.line_process)(line);
            content_positions_tol.entry(processed).or_default().insert(i);
        }

        // Find new matches for each search line
        for (search_idx, search_line) in search_lines.iter().enumerate() {
            let processed_search = (tolerance.line_process)(search_line);
            if let Some(new_positions) = content_positions_tol.get(&processed_search) {
                let existing = &search_line_positions[search_idx];
                let new_indices: HashSet<usize> =
                    new_positions.difference(existing).copied().collect();

                search_line_positions[search_idx].extend(&new_indices);

                // Track tolerance index for new matches
                for idx in new_indices {
                    tolerance_index_by_content_line[search_idx].insert(idx, tidx);
                }
            }
        }
    }

    // Find contiguous matches
    let matched_slices = find_contiguous_matches(&search_line_positions);

    // Build results with tolerance hit information
    let mut results = Vec::new();
    for (start, end) in matched_slices {
        let mut tolerances_hit: Vec<ToleranceHit> =
            tolerances.iter().enumerate().map(|(i, t)| ToleranceHit::new(t, i)).collect();

        let mut tolerance_indices_used = Vec::new();

        // Count tolerance hits for this match
        for (search_idx, content_idx) in (start..end).enumerate() {
            if let Some(&tidx) = tolerance_index_by_content_line[search_idx].get(&content_idx) {
                tolerances_hit[tidx].count += 1;
                if !tolerance_indices_used.contains(&tidx) {
                    tolerance_indices_used.push(tidx);
                }
            }
        }

        // Filter out zero-count tolerances and calculate score
        let active_tolerances: Vec<ToleranceHit> =
            tolerances_hit.into_iter().filter(|t| t.count > 0).collect();

        let score: f64 = active_tolerances.iter().map(ToleranceHit::score).sum();

        // Collect warnings
        let warnings: HashSet<String> = active_tolerances
            .iter()
            .filter(|t| t.severity == ToleranceSeverity::Warning && !t.error_name.is_empty())
            .map(|t| t.error_name.to_string())
            .collect();

        // Get matched lines
        let matched_lines: Vec<String> =
            content_lines[start..end].iter().map(|s| (*s).to_string()).collect();

        // Check for removed indentation
        let removed_indentation =
            if active_tolerances.iter().any(|t| t.error_name == REMOVE_INDENTATION_WARNING) {
                // Get the most common indentation from matched lines
                let indents: Vec<String> = matched_lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| get_removed_indentation(l))
                    .collect();

                if indents.is_empty() {
                    None
                } else {
                    Some(indents[0].clone())
                }
            } else {
                None
            };

        results.push((
            MatchResult {
                matched_slice: (start, end),
                line_range: (start + 1, end), // 1-indexed
                tolerances_hit: active_tolerances,
                score,
                warnings,
                matched_lines,
                removed_indentation,
            },
            tolerance_indices_used,
        ));
    }

    results
}

/// Match with tolerance, also handling empty line removal
pub fn match_with_tolerance_empty_lines(
    content_lines: &[&str],
    content_offset: usize,
    search_lines: &[&str],
    tolerances: &[Tolerance],
) -> Vec<(MatchResult, Vec<usize>)> {
    // Filter out empty lines from content
    let mut new_content: Vec<&str> = Vec::new();
    let mut new_to_original: HashMap<usize, usize> = HashMap::new();

    for (i, line) in content_lines.iter().enumerate().skip(content_offset) {
        if !line.trim().is_empty() {
            new_to_original.insert(new_content.len(), i);
            new_content.push(line);
        }
    }

    // Filter empty lines from search
    let search_filtered: Vec<&str> =
        search_lines.iter().filter(|l| !l.trim().is_empty()).copied().collect();

    if search_filtered.is_empty() {
        return vec![];
    }

    let matches = match_with_tolerance(&new_content, 0, &search_filtered, tolerances);

    // Map back to original indices
    matches
        .into_iter()
        .filter_map(|(mut result, indices)| {
            let orig_start = new_to_original.get(&result.matched_slice.0)?;
            let orig_end = new_to_original.get(&(result.matched_slice.1 - 1))? + 1;

            result.matched_slice = (*orig_start, orig_end);
            result.line_range = (*orig_start + 1, orig_end);
            result.matched_lines =
                content_lines[*orig_start..orig_end].iter().map(|s| (*s).to_string()).collect();

            Some((result, indices))
        })
        .collect()
}

// ============================================================================
// Indentation fixing
// ============================================================================

/// Fix indentation in replace block based on matched content
pub fn fix_indentation(
    matched_lines: &[String],
    searched_lines: &[&str],
    replace_lines: Vec<String>,
) -> Vec<String> {
    if matched_lines.is_empty() || searched_lines.is_empty() || replace_lines.is_empty() {
        return replace_lines;
    }

    fn get_indent(line: &str) -> String {
        lazy_static::lazy_static! {
            static ref INDENT_RE: Regex = Regex::new(r"^(\s*)").unwrap();
        }
        INDENT_RE
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    }

    // Get indentation for non-empty lines
    let matched_indents: Vec<String> =
        matched_lines.iter().filter(|l| !l.trim().is_empty()).map(|l| get_indent(l)).collect();

    let searched_indents: Vec<String> =
        searched_lines.iter().filter(|l| !l.trim().is_empty()).map(|l| get_indent(l)).collect();

    if matched_indents.len() != searched_indents.len() {
        return replace_lines;
    }

    // Calculate indentation differences
    let diffs: Vec<i32> = matched_indents
        .iter()
        .zip(searched_indents.iter())
        .map(|(m, s)| s.len() as i32 - m.len() as i32)
        .collect();

    // Check if all differences are the same
    if diffs.is_empty() || !diffs.iter().all(|&d| d == diffs[0]) {
        return replace_lines;
    }

    let diff = diffs[0];
    if diff == 0 {
        return replace_lines;
    }

    // Adjust indentation
    replace_lines
        .into_iter()
        .map(|line| {
            if diff < 0 {
                // Add indentation
                // SECURITY: Use saturating_abs to prevent overflow on i32::MIN
                let indent_amount = diff.saturating_abs() as usize;
                let add_indent = " ".repeat(indent_amount);
                format!("{add_indent}{line}")
            } else {
                // Remove indentation
                // SECURITY: Safe conversion since diff >= 0
                let diff_usize = diff as usize;
                if line.len() >= diff_usize && line[..diff_usize].chars().all(char::is_whitespace) {
                    line[diff_usize..].to_string()
                } else {
                    line
                }
            }
        })
        .collect()
}

/// Fix line numbers in replace block
pub fn fix_line_nums(replace_lines: Vec<String>) -> Vec<String> {
    replace_lines.into_iter().map(|line| remove_leading_linenums(&line)).collect()
}

/// Remove leading/trailing empty lines from a list of lines
pub fn remove_leading_trailing_empty_lines(lines: Vec<String>) -> Vec<String> {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());

    match (start, end) {
        (Some(s), Some(e)) => lines[s..=e].to_vec(),
        _ => lines,
    }
}

// ============================================================================
// Search/Replace Block Processor
// ============================================================================

/// Search/replace block
#[derive(Debug, Clone)]
pub struct SearchReplaceBlock {
    /// Search content (the text to find)
    pub search: Vec<String>,
    /// Replace content (the text to replace with)
    pub replace: Vec<String>,
}

/// Result of applying search/replace blocks
#[derive(Debug)]
pub struct ApplyResult {
    /// The modified content
    pub content: String,
    /// Warnings generated during processing
    pub warnings: HashSet<String>,
    /// Total score accumulated
    pub total_score: f64,
}

/// Error when applying search/replace blocks
#[derive(Debug)]
pub struct ApplyError {
    /// Error message
    pub message: String,
    /// The search block that failed
    pub failed_block: Option<String>,
    /// Context from the file that might be relevant
    pub context: Option<String>,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(ref block) = self.failed_block {
            write!(f, "\n\nFailed search block:\n```\n{block}\n```")?;
        }
        if let Some(ref ctx) = self.context {
            write!(f, "\n\nRelevant context from file:\n```\n{ctx}\n```")?;
        }
        Ok(())
    }
}

/// Maximum allowed score before rejecting the edit
pub const MAX_TOLERANCE_SCORE: f64 = 1000.0;

/// Apply search/replace blocks with tolerance matching
pub fn apply_search_replace_with_tolerance(
    content: &str,
    blocks: Vec<SearchReplaceBlock>,
) -> Result<ApplyResult, ApplyError> {
    let tolerances = default_tolerances();
    let mut content_lines: Vec<String> =
        content.lines().map(std::string::ToString::to_string).collect();
    let mut all_warnings = HashSet::new();
    let mut total_score = 0.0;
    let mut offset: i64 = 0;

    for (block_idx, block) in blocks.iter().enumerate() {
        let search_refs: Vec<&str> = block.search.iter().map(std::string::String::as_str).collect();
        let content_refs: Vec<&str> =
            content_lines.iter().map(std::string::String::as_str).collect();

        trace!(
            "Processing block {}/{}: {} search lines",
            block_idx + 1,
            blocks.len(),
            search_refs.len()
        );

        // Try exact match first
        let exact_matches = match_exact(&content_refs, 0, &search_refs);

        let (match_result, replace_lines) = if exact_matches.is_empty() {
            // Try tolerance matching
            let tolerance_matches =
                match_with_tolerance(&content_refs, 0, &search_refs, &tolerances);

            if tolerance_matches.is_empty() {
                // Try with empty line removal
                let empty_line_matches =
                    match_with_tolerance_empty_lines(&content_refs, 0, &search_refs, &tolerances);

                if empty_line_matches.is_empty() {
                    // Find closest match for error message
                    let context = find_closest_context(&content_refs, &search_refs);
                    return Err(ApplyError {
                        message: format!(
                            "Search block {} not found in content.\n{}",
                            block_idx + 1,
                            "The search block doesn't match any part of the file."
                        ),
                        failed_block: Some(block.search.join("\n")),
                        context: Some(context),
                    });
                }

                // Use empty line match with filtered replace
                let (result, _) = empty_line_matches.into_iter().next().unwrap();
                let filtered_replace = remove_leading_trailing_empty_lines(block.replace.clone());
                (result, filtered_replace)
            } else if tolerance_matches.len() > 1 {
                return Err(ApplyError {
                    message: format!(
                        "Search block {} matched {} times with tolerances. Add more context.",
                        block_idx + 1,
                        tolerance_matches.len()
                    ),
                    failed_block: Some(block.search.join("\n")),
                    context: None,
                });
            } else {
                let (result, _) = tolerance_matches.into_iter().next().unwrap();
                (result, block.replace.clone())
            }
        } else {
            if exact_matches.len() > 1 {
                return Err(ApplyError {
                    message: format!(
                        "Search block {} matched {} times. Add more context to make it unique.",
                        block_idx + 1,
                        exact_matches.len()
                    ),
                    failed_block: Some(block.search.join("\n")),
                    context: None,
                });
            }

            let (start, end) = exact_matches[0];
            (
                MatchResult {
                    matched_slice: (start, end),
                    line_range: (start + 1, end),
                    tolerances_hit: vec![],
                    score: 0.0,
                    warnings: HashSet::new(),
                    matched_lines: content_lines[start..end].to_vec(),
                    removed_indentation: None,
                },
                block.replace.clone(),
            )
        };

        // Apply indentation fix if needed
        let mut final_replace = replace_lines;

        if match_result.used_lstrip() {
            debug!("Applying indentation fix for block {}", block_idx + 1);
            final_replace =
                fix_indentation(&match_result.matched_lines, &search_refs, final_replace);
        }

        if match_result.used_line_nums() {
            debug!("Removing line numbers from replace block {}", block_idx + 1);
            final_replace = fix_line_nums(final_replace);
        }

        // Accumulate warnings and score
        all_warnings.extend(match_result.warnings);
        total_score += match_result.score;

        // Check score threshold
        if total_score > MAX_TOLERANCE_SCORE {
            return Err(ApplyError {
                message: format!(
                    "Too many tolerance warnings accumulated (score: {total_score:.1} > {MAX_TOLERANCE_SCORE}). Not applying edits."
                ),
                failed_block: None,
                context: None,
            });
        }

        // Apply the replacement with offset adjustment
        let (start, end) = match_result.matched_slice;
        // SECURITY: Use saturating arithmetic to prevent underflow when offset is negative
        let adjusted_start_i64 = start as i64 + offset;
        let adjusted_end_i64 = end as i64 + offset;

        // Ensure adjusted indices are valid (non-negative and within bounds)
        let adjusted_start = adjusted_start_i64.max(0) as usize;
        let adjusted_end = adjusted_end_i64.max(0) as usize;
        let adjusted_end = adjusted_end.min(content_lines.len());
        let adjusted_start = adjusted_start.min(adjusted_end);

        let before: Vec<String> = content_lines[..adjusted_start].to_vec();
        let after: Vec<String> = content_lines[adjusted_end..].to_vec();

        // Update offset for next block
        let old_len = adjusted_end - adjusted_start;
        let new_len = final_replace.len();
        offset += new_len as i64 - old_len as i64;

        content_lines = [before, final_replace, after].concat();
    }

    Ok(ApplyResult { content: content_lines.join("\n"), warnings: all_warnings, total_score })
}

/// Find the closest matching context for error messages
fn find_closest_context(content_lines: &[&str], search_lines: &[&str]) -> String {
    // Simple approach: find the first search line that exists and get context around it
    for search_line in search_lines {
        let search_stripped = search_line.trim();
        if search_stripped.len() < 10 {
            continue;
        }

        for (i, content_line) in content_lines.iter().enumerate() {
            if content_line.contains(search_stripped)
                || (content_line.trim().len() > 10 && search_stripped.contains(content_line.trim()))
            {
                // Found a partial match, get context
                let start = i.saturating_sub(5);
                let end = (i + 10).min(content_lines.len());
                return content_lines[start..end].join("\n");
            }
        }
    }

    // No match found, return beginning of file
    let end = 20.min(content_lines.len());
    format!("(Beginning of file)\n{}", content_lines[..end].join("\n"))
}

/// Try applying blocks individually as fallback
pub fn apply_with_individual_fallback(
    content: &str,
    blocks: Vec<SearchReplaceBlock>,
) -> Result<ApplyResult, ApplyError> {
    // First try all blocks together
    match apply_search_replace_with_tolerance(content, blocks.clone()) {
        Ok(result) => Ok(result),
        Err(e) if blocks.len() > 1 => {
            // Try one at a time
            warn!("Multi-block apply failed, trying individual blocks: {}", e.message);

            let mut current_content = content.to_string();
            let mut all_warnings = HashSet::new();
            let mut total_score = 0.0;

            for (i, block) in blocks.into_iter().enumerate() {
                match apply_search_replace_with_tolerance(&current_content, vec![block.clone()]) {
                    Ok(result) => {
                        current_content = result.content;
                        all_warnings.extend(result.warnings);
                        total_score += result.total_score;
                    }
                    Err(block_err) => {
                        return Err(ApplyError {
                            message: format!(
                                "Block {} failed during individual apply: {}",
                                i + 1,
                                block_err.message
                            ),
                            failed_block: Some(block.search.join("\n")),
                            context: block_err.context,
                        });
                    }
                }
            }

            Ok(ApplyResult { content: current_content, warnings: all_warnings, total_score })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_rstrip() {
        assert_eq!(apply_rstrip("hello   "), "hello");
        assert_eq!(apply_rstrip("hello\t\t"), "hello");
        assert_eq!(apply_rstrip("hello"), "hello");
    }

    #[test]
    fn test_apply_lstrip() {
        assert_eq!(apply_lstrip("   hello"), "hello");
        assert_eq!(apply_lstrip("\t\thello"), "hello");
        assert_eq!(apply_lstrip("hello"), "hello");
    }

    #[test]
    fn test_remove_leading_linenums() {
        assert_eq!(remove_leading_linenums("123 hello"), "hello");
        assert_eq!(remove_leading_linenums("1 line"), "line");
        assert_eq!(remove_leading_linenums("hello"), "hello");
        assert_eq!(remove_leading_linenums("123hello"), "123hello"); // No space after number
    }

    #[test]
    fn test_normalize_unicode() {
        assert_eq!(normalize_unicode("\u{201C}test\u{201D}"), "\"test\"");
        assert_eq!(normalize_unicode("test\u{2014}value"), "test-value");
        assert_eq!(normalize_unicode("test\u{2026}"), "test...");
    }

    #[test]
    fn test_remove_all_whitespace() {
        assert_eq!(remove_all_whitespace("  hello world  "), "helloworld");
        assert_eq!(remove_all_whitespace("a b\tc\nd"), "abcd");
    }

    #[test]
    fn test_exact_match() {
        let content = vec!["line1", "line2", "line3", "line4"];
        let search = vec!["line2", "line3"];
        let matches = match_exact(&content, 0, &search);
        assert_eq!(matches, vec![(1, 3)]);
    }

    #[test]
    fn test_no_match() {
        let content = vec!["line1", "line2", "line3"];
        let search = vec!["not", "found"];
        let matches = match_exact(&content, 0, &search);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_fix_indentation() {
        let matched = vec!["    fn test() {".to_string(), "        code()".to_string()];
        let searched = vec!["fn test() {", "    code()"];
        let replace = vec!["fn new_test() {".to_string(), "    new_code()".to_string()];

        let fixed = fix_indentation(&matched, &searched, replace);
        assert_eq!(fixed[0], "    fn new_test() {");
        assert_eq!(fixed[1], "        new_code()");
    }

    #[test]
    fn test_tolerance_match_with_trailing_whitespace() {
        // Content has trailing whitespace, search doesn't
        let content = vec!["line1   ", "line2  ", "line3"];
        let search = vec!["line1", "line2"];
        let tolerances = default_tolerances();
        let matches = match_with_tolerance(&content, 0, &search, &tolerances);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0.matched_slice, (0, 2));
    }

    #[test]
    fn test_tolerance_match_with_different_indentation() {
        // Content has different indentation than search
        let content = vec!["    fn foo() {", "        bar()", "    }"];
        let search = vec!["fn foo() {", "    bar()", "}"];
        let tolerances = default_tolerances();
        let matches = match_with_tolerance(&content, 0, &search, &tolerances);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].0.warnings.contains(REMOVE_INDENTATION_WARNING));
    }

    #[test]
    fn test_tolerance_match_with_line_numbers() {
        // Search has line numbers
        let content = vec!["fn main() {", "    println!(\"hello\");", "}"];
        let search = vec!["1 fn main() {", "2     println!(\"hello\");", "3 }"];
        let tolerances = default_tolerances();
        let matches = match_with_tolerance(&content, 0, &search, &tolerances);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].0.warnings.contains(REMOVE_LINE_NUMS_WARNING));
    }

    #[test]
    fn test_tolerance_match_with_unicode_quotes() {
        // Search has unicode quotes, content has ASCII
        let content = vec!["let x = \"hello\";"];
        let search = vec!["let x = \u{201C}hello\u{201D};"];
        let tolerances = default_tolerances();
        let matches = match_with_tolerance(&content, 0, &search, &tolerances);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].0.warnings.contains(NORMALIZE_CHARS_WARNING));
    }

    #[test]
    fn test_apply_search_replace_exact() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let blocks = vec![SearchReplaceBlock {
            search: vec!["    println!(\"hello\");".to_string()],
            replace: vec!["    println!(\"world\");".to_string()],
        }];

        let result = apply_search_replace_with_tolerance(content, blocks);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.content.contains("println!(\"world\")"));
        assert!(result.warnings.is_empty());
        assert_eq!(result.total_score, 0.0);
    }

    #[test]
    fn test_apply_search_replace_with_tolerance() {
        // Search has trailing whitespace, content doesn't
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let blocks = vec![SearchReplaceBlock {
            search: vec!["    println!(\"hello\");   ".to_string()], // trailing whitespace
            replace: vec!["    println!(\"world\");".to_string()],
        }];

        let result = apply_search_replace_with_tolerance(content, blocks);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.content.contains("println!(\"world\")"));
    }

    #[test]
    fn test_apply_multiple_blocks() {
        let content = "fn foo() { 1 }\nfn bar() { 2 }";
        let blocks = vec![
            SearchReplaceBlock {
                search: vec!["fn foo() { 1 }".to_string()],
                replace: vec!["fn foo() { 10 }".to_string()],
            },
            SearchReplaceBlock {
                search: vec!["fn bar() { 2 }".to_string()],
                replace: vec!["fn bar() { 20 }".to_string()],
            },
        ];

        let result = apply_with_individual_fallback(&content, blocks);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.content.contains("fn foo() { 10 }"));
        assert!(result.content.contains("fn bar() { 20 }"));
    }

    #[test]
    fn test_apply_search_not_found() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let blocks = vec![SearchReplaceBlock {
            search: vec!["not found".to_string()],
            replace: vec!["replacement".to_string()],
        }];

        let result = apply_search_replace_with_tolerance(content, blocks);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("not found"));
    }

    #[test]
    fn test_apply_ambiguous_match() {
        // Content has duplicate patterns
        let content = "x = 1\ny = 2\nx = 1";
        let blocks = vec![SearchReplaceBlock {
            search: vec!["x = 1".to_string()],
            replace: vec!["x = 100".to_string()],
        }];

        let result = apply_search_replace_with_tolerance(content, blocks);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("matched") && err.message.contains("times"));
    }

    #[test]
    fn test_indentation_auto_fix() {
        // Content is indented, search is not
        let content = "    fn test() {\n        code()\n    }";
        let blocks = vec![SearchReplaceBlock {
            search: vec!["fn test() {".to_string(), "    code()".to_string(), "}".to_string()],
            replace: vec![
                "fn new_test() {".to_string(),
                "    new_code()".to_string(),
                "}".to_string(),
            ],
        }];

        let result = apply_search_replace_with_tolerance(content, blocks);
        assert!(result.is_ok());
        let result = result.unwrap();
        // The replacement should be indented to match the original
        assert!(result.content.contains("    fn new_test()"));
        assert!(result.content.contains("        new_code()"));
    }

    // ========================================================================
    // Tiered Matching with Early Exit Tests
    // ========================================================================

    #[test]
    fn test_tiered_exact_match_early_exit() {
        // Exact match should return at Tier 0 with early_exit = true
        let content = vec!["line1", "line2", "line3"];
        let search = vec!["line1", "line2"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 0); // Tier 0 = exact match
        assert!(results[0].early_exit);
        assert_eq!(results[0].result.matched_slice, (0, 2));
        assert_eq!(results[0].result.score, 0.0); // No tolerance used
    }

    #[test]
    fn test_tiered_rstrip_early_exit() {
        // Content has trailing whitespace - should match at Tier 1 (rstrip)
        let content = vec!["line1   ", "line2  "];
        let search = vec!["line1", "line2"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 1); // Tier 1 = rstrip
        assert!(results[0].early_exit);
        assert_eq!(results[0].result.score, 2.0); // 1.0 * 2 lines
    }

    #[test]
    fn test_tiered_lstrip_early_exit() {
        // Content has different indentation - should match at Tier 2 (lstrip)
        let content = vec!["    fn foo() {", "        bar()", "    }"];
        let search = vec!["fn foo() {", "    bar()", "}"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 2); // Tier 2 = lstrip
        assert!(results[0].early_exit);
        assert!(results[0].result.warnings.contains(REMOVE_INDENTATION_WARNING));
    }

    #[test]
    fn test_tiered_line_nums_early_exit() {
        // Search has line numbers - should match at Tier 3
        let content = vec!["fn main() {", "    println!(\"hello\");", "}"];
        let search = vec!["1 fn main() {", "2     println!(\"hello\");", "3 }"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 3); // Tier 3 = remove line nums
        assert!(results[0].early_exit);
        assert!(results[0].result.warnings.contains(REMOVE_LINE_NUMS_WARNING));
    }

    #[test]
    fn test_tiered_unicode_early_exit() {
        // Search has unicode quotes - should match at Tier 4
        let content = vec!["let x = \"hello\";"];
        let search = vec!["let x = \u{201C}hello\u{201D};"]; // Smart quotes

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 4); // Tier 4 = normalize unicode
        assert!(results[0].early_exit);
        assert!(results[0].result.warnings.contains(NORMALIZE_CHARS_WARNING));
    }

    #[test]
    fn test_tiered_multiple_exact_matches() {
        // Multiple exact matches - should return all with early_exit = false
        let content = vec!["x = 1", "y = 2", "x = 1"];
        let search = vec!["x = 1"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 2); // Two matches
        assert!(results.iter().all(|r| r.tier == 0)); // All at Tier 0
        assert!(results.iter().all(|r| !r.early_exit)); // No early exit (ambiguous)
    }

    #[test]
    fn test_tiered_no_match() {
        // No match at any tier
        let content = vec!["completely", "different", "content"];
        let search = vec!["not", "found"];

        let results = match_tiered(&content, 0, &search);

        assert!(results.is_empty());
    }

    #[test]
    fn test_tiered_performance_exact_match() {
        // Large content with exact match should return immediately
        let mut content: Vec<&str> = (0..1000).map(|_| "filler line").collect();
        content[500] = "target line 1";
        content[501] = "target line 2";

        let search = vec!["target line 1", "target line 2"];

        let results = match_tiered(&content, 0, &search);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tier, 0); // Found at Tier 0
        assert!(results[0].early_exit);
        assert_eq!(results[0].result.matched_slice, (500, 502));
    }
}
