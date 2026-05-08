//! Implementation of the `FileWriteOrEdit` tool.
//!
//! This module provides the implementation for the `FileWriteOrEdit` tool, which is used
//! to write or edit files, with support for both full file content and search/replace blocks.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, FileWhitelistData};
use crate::types::{normalize_thread_id, FileWriteOrEdit};
use crate::utils::path::{expand_user, validate_path_in_workspace};

static SEARCH_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
static DIVIDER_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
static REPLACE_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();

fn regex_marker(
    marker: &'static OnceLock<std::result::Result<Regex, regex::Error>>,
    pattern: &'static str,
) -> Result<&'static Regex> {
    marker.get_or_init(|| Regex::new(pattern)).as_ref().map_err(|error| {
        WinxError::ArgumentParseError(format!("Invalid edit marker regex: {error}"))
    })
}

fn search_marker() -> Result<&'static Regex> {
    regex_marker(&SEARCH_MARKER, r"(?m)^<<<<<<+\s*SEARCH>?\s*$")
}

fn divider_marker() -> Result<&'static Regex> {
    regex_marker(&DIVIDER_MARKER, r"(?m)^======*\s*$")
}

fn replace_marker() -> Result<&'static Regex> {
    regex_marker(&REPLACE_MARKER, r"(?m)^>>>>>>+\s*REPLACE\s*$")
}

const MAX_FILE_SIZE: u64 = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchReplaceBlock {
    search: Vec<String>,
    replace: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToleranceKind {
    TrimEnd,
    IgnoreIndentation,
    RemoveLineNumbers,
    NormalizeCommonMistakes,
    IgnoreWhitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineMatch {
    Exact,
    Tolerated(ToleranceKind),
}

impl ToleranceKind {
    fn score(self) -> usize {
        match self {
            ToleranceKind::TrimEnd => 1,
            ToleranceKind::RemoveLineNumbers | ToleranceKind::NormalizeCommonMistakes => 5,
            ToleranceKind::IgnoreIndentation => 10,
            ToleranceKind::IgnoreWhitespace => 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchCandidate {
    start: usize,
    end: usize,
    score: usize,
    tolerances: Vec<ToleranceKind>,
    replace: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    start: usize,
    end: usize,
    replace: Vec<String>,
}

fn parse_blocks(content: &str) -> Result<Vec<SearchReplaceBlock>> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if search_marker()?.is_match(lines[i]) {
            let line_num = i + 1;
            i += 1;
            let mut search_lines = Vec::new();
            while i < lines.len() && !divider_marker()?.is_match(lines[i]) {
                if search_marker()?.is_match(lines[i]) || replace_marker()?.is_match(lines[i]) {
                    return Err(WinxError::SearchReplaceSyntaxError(format!(
                        "Line {}: stray marker in SEARCH block",
                        i + 1
                    )));
                }
                search_lines.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {line_num}: unclosed SEARCH block - missing ======= marker"
                )));
            }

            if search_lines.is_empty() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {line_num}: SEARCH block cannot be empty"
                )));
            }

            i += 1;
            let mut replace_lines = Vec::new();
            while i < lines.len() && !replace_marker()?.is_match(lines[i]) {
                if search_marker()?.is_match(lines[i]) || divider_marker()?.is_match(lines[i]) {
                    return Err(WinxError::SearchReplaceSyntaxError(format!(
                        "Line {}: stray marker in REPLACE block",
                        i + 1
                    )));
                }
                replace_lines.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {line_num}: unclosed block - missing REPLACE marker"
                )));
            }

            blocks.push(SearchReplaceBlock {
                search: search_lines.into_iter().map(str::to_string).collect(),
                replace: replace_lines.into_iter().map(str::to_string).collect(),
            });
        } else if divider_marker()?.is_match(lines[i]) || replace_marker()?.is_match(lines[i]) {
            return Err(WinxError::SearchReplaceSyntaxError(format!(
                "Line {}: stray marker outside block",
                i + 1
            )));
        }
        i += 1;
    }

    if blocks.is_empty() {
        return Err(WinxError::SearchReplaceSyntaxError("No valid blocks found".to_string()));
    }

    Ok(blocks)
}

fn apply_blocks(content: &str, blocks: &[SearchReplaceBlock]) -> Result<String> {
    let original_lines = split_lines(content);
    let edited = apply_blocks_ordered(&original_lines, blocks).or_else(|ordered_error| {
        if blocks.len() == 1 {
            Err(ordered_error)
        } else {
            apply_blocks_individually(&original_lines, blocks)
        }
    })?;

    Ok(edited.join("\n"))
}

fn split_lines(content: &str) -> Vec<String> {
    content.split('\n').map(str::to_string).collect()
}

fn apply_blocks_ordered(lines: &[String], blocks: &[SearchReplaceBlock]) -> Result<Vec<String>> {
    let (_, replacements) = best_ordered_replacements(lines, blocks, 0, 0)?;
    Ok(apply_replacements(lines, &replacements))
}

fn best_ordered_replacements(
    lines: &[String],
    blocks: &[SearchReplaceBlock],
    block_index: usize,
    offset: usize,
) -> Result<(usize, Vec<Replacement>)> {
    if block_index >= blocks.len() {
        return Ok((0, Vec::new()));
    }

    let block = &blocks[block_index];
    let candidates = find_candidates(lines, block, offset);
    if candidates.is_empty() {
        return Err(not_found_error(block, lines, offset));
    }

    let mut valid_paths = Vec::new();
    for candidate in candidates {
        if let Ok((tail_score, mut tail)) =
            best_ordered_replacements(lines, blocks, block_index + 1, candidate.end)
        {
            let mut path = vec![Replacement {
                start: candidate.start,
                end: candidate.end,
                replace: candidate.replace,
            }];
            path.append(&mut tail);
            valid_paths.push((candidate.score + tail_score, path));
        }
    }

    select_unique_best_path(block, valid_paths)
}

fn select_unique_best_path(
    block: &SearchReplaceBlock,
    paths: Vec<(usize, Vec<Replacement>)>,
) -> Result<(usize, Vec<Replacement>)> {
    let Some(best_score) = paths.iter().map(|(score, _)| *score).min() else {
        return Err(WinxError::SearchBlockNotFound(format!(
            "Block not found: {}",
            block.search.join("\n")
        )));
    };

    let best_paths: Vec<(usize, Vec<Replacement>)> =
        paths.into_iter().filter(|(score, _)| *score == best_score).collect();

    if best_paths.len() == 1 {
        return best_paths.into_iter().next().ok_or_else(|| {
            WinxError::SearchBlockNotFound(format!("Block not found: {}", block.search.join("\n")))
        });
    }

    Err(WinxError::SearchBlockAmbiguous {
        block_content: block.search.join("\n"),
        match_count: best_paths.len(),
        suggestions: vec!["Add more context before or after this block.".to_string()],
    })
}

fn apply_blocks_individually(
    lines: &[String],
    blocks: &[SearchReplaceBlock],
) -> Result<Vec<String>> {
    let mut running_lines = lines.to_vec();
    for block in blocks {
        let candidate = select_unique_candidate(block, find_candidates(&running_lines, block, 0))?;
        running_lines = apply_replacements(
            &running_lines,
            &[Replacement {
                start: candidate.start,
                end: candidate.end,
                replace: candidate.replace,
            }],
        );
    }
    Ok(running_lines)
}

fn select_unique_candidate(
    block: &SearchReplaceBlock,
    candidates: Vec<MatchCandidate>,
) -> Result<MatchCandidate> {
    if candidates.is_empty() {
        return Err(WinxError::SearchBlockNotFound(format!(
            "Block not found: {}",
            block.search.join("\n")
        )));
    }

    let best_score = candidates.iter().map(|candidate| candidate.score).min().unwrap_or(0);
    let best: Vec<MatchCandidate> =
        candidates.into_iter().filter(|candidate| candidate.score == best_score).collect();

    if best.len() == 1 {
        return best.into_iter().next().ok_or_else(|| {
            WinxError::SearchBlockNotFound(format!("Block not found: {}", block.search.join("\n")))
        });
    }

    Err(WinxError::SearchBlockAmbiguous {
        block_content: block.search.join("\n"),
        match_count: best.len(),
        suggestions: vec!["Add more context to make the search block unique.".to_string()],
    })
}

fn apply_replacements(lines: &[String], replacements: &[Replacement]) -> Vec<String> {
    let mut edited = Vec::new();
    let mut cursor = 0;

    for replacement in replacements {
        edited.extend_from_slice(&lines[cursor..replacement.start]);
        edited.extend(replacement.replace.clone());
        cursor = replacement.end;
    }

    edited.extend_from_slice(&lines[cursor..]);
    edited
}

fn find_candidates(
    lines: &[String],
    block: &SearchReplaceBlock,
    offset: usize,
) -> Vec<MatchCandidate> {
    let mut candidates = find_contiguous_candidates(lines, block, offset, false);
    if candidates.is_empty() {
        candidates = find_contiguous_candidates(lines, block, offset, true);
    }
    candidates
}

fn find_contiguous_candidates(
    lines: &[String],
    block: &SearchReplaceBlock,
    offset: usize,
    ignore_empty_lines: bool,
) -> Vec<MatchCandidate> {
    let search_lines = if ignore_empty_lines {
        block.search.iter().filter(|line| !line.trim().is_empty()).cloned().collect()
    } else {
        block.search.clone()
    };

    if search_lines.is_empty() || lines.len().saturating_sub(offset) < search_lines.len() {
        return Vec::new();
    }

    if ignore_empty_lines {
        return find_empty_line_tolerant_candidates(lines, block, offset, &search_lines);
    }

    let max_start = lines.len() - search_lines.len();
    (offset..=max_start)
        .filter_map(|start| {
            let end = start + search_lines.len();
            let actual_lines: Vec<&String> = lines[start..end].iter().collect();
            match_candidate(lines, &actual_lines, &search_lines, block, start, end, false)
        })
        .collect()
}

fn find_empty_line_tolerant_candidates(
    lines: &[String],
    block: &SearchReplaceBlock,
    offset: usize,
    search_lines: &[String],
) -> Vec<MatchCandidate> {
    let compact_lines: Vec<(usize, &String)> =
        lines.iter().enumerate().skip(offset).filter(|(_, line)| !line.trim().is_empty()).collect();

    if compact_lines.len() < search_lines.len() {
        return Vec::new();
    }

    let max_start = compact_lines.len() - search_lines.len();
    (0..=max_start)
        .filter_map(|compact_start| {
            let compact_end = compact_start + search_lines.len();
            let start = compact_lines[compact_start].0;
            let end = compact_lines[compact_end - 1].0 + 1;
            let actual_lines: Vec<&String> =
                compact_lines[compact_start..compact_end].iter().map(|(_, line)| *line).collect();
            match_candidate(lines, &actual_lines, search_lines, block, start, end, true)
        })
        .collect()
}

fn match_candidate(
    lines: &[String],
    actual_lines: &[&String],
    search_lines: &[String],
    block: &SearchReplaceBlock,
    start: usize,
    end: usize,
    ignore_empty_lines: bool,
) -> Option<MatchCandidate> {
    let mut tolerances = Vec::new();
    let mut score = 0;

    for (actual, expected) in actual_lines.iter().zip(search_lines) {
        let line_match = matching_tolerance(actual, expected)?;
        if let LineMatch::Tolerated(tolerance) = line_match {
            score += tolerance.score();
            if !tolerances.contains(&tolerance) {
                tolerances.push(tolerance);
            }
        }
    }

    let mut replace = if ignore_empty_lines {
        trim_empty_edge_lines(&block.replace)
    } else {
        block.replace.clone()
    };
    if tolerances.contains(&ToleranceKind::RemoveLineNumbers) {
        replace = replace.into_iter().map(|line| remove_leading_line_number(&line)).collect();
    }
    if tolerances.contains(&ToleranceKind::IgnoreIndentation) {
        let matched = &lines[start..end];
        replace = fix_indentation(matched, search_lines, &replace);
    }

    Some(MatchCandidate { start, end, score, tolerances, replace })
}

fn matching_tolerance(actual: &str, expected: &str) -> Option<LineMatch> {
    if actual == expected {
        return Some(LineMatch::Exact);
    }
    if actual.trim_end() == expected.trim_end() {
        return Some(LineMatch::Tolerated(ToleranceKind::TrimEnd));
    }
    if actual.trim_start() == expected.trim_start() {
        return Some(LineMatch::Tolerated(ToleranceKind::IgnoreIndentation));
    }
    if remove_leading_line_number(actual) == remove_leading_line_number(expected) {
        return Some(LineMatch::Tolerated(ToleranceKind::RemoveLineNumbers));
    }
    if normalize_common_mistakes(actual) == normalize_common_mistakes(expected) {
        return Some(LineMatch::Tolerated(ToleranceKind::NormalizeCommonMistakes));
    }
    if remove_ascii_whitespace(actual) == remove_ascii_whitespace(expected) {
        return Some(LineMatch::Tolerated(ToleranceKind::IgnoreWhitespace));
    }
    None
}

fn remove_ascii_whitespace(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn remove_leading_line_number(value: &str) -> String {
    value
        .split_once(' ')
        .filter(|(prefix, _)| !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()))
        .map_or_else(|| value.trim_end().to_string(), |(_, rest)| rest.trim_end().to_string())
}

fn normalize_common_mistakes(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\u{2018}' | '\u{2019}' | '\u{201b}' | '\u{2032}' => normalized.push('\''),
            '\u{201a}' => normalized.push(','),
            '\u{201c}' | '\u{201d}' | '\u{201f}' | '\u{2033}' => normalized.push('"'),
            '\u{2039}' => normalized.push('<'),
            '\u{203a}' => normalized.push('>'),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => normalized.push('-'),
            '\u{2026}' => normalized.push_str("..."),
            other => normalized.push(other),
        }
    }
    normalized.trim_end().to_string()
}

fn fix_indentation(
    matched_lines: &[String],
    searched_lines: &[String],
    replaced_lines: &[String],
) -> Vec<String> {
    if matched_lines.is_empty() || searched_lines.is_empty() || replaced_lines.is_empty() {
        return replaced_lines.to_vec();
    }

    let matched_indents = non_empty_indents(matched_lines);
    let searched_indents = non_empty_indents(searched_lines);
    if matched_indents.len() != searched_indents.len() || matched_indents.is_empty() {
        return replaced_lines.to_vec();
    }

    let diffs: Vec<isize> = matched_indents
        .iter()
        .zip(&searched_indents)
        .map(|(matched, searched)| searched.len() as isize - matched.len() as isize)
        .collect();
    let first_diff = diffs[0];
    if first_diff == 0 || !diffs.iter().all(|diff| *diff == first_diff) {
        return replaced_lines.to_vec();
    }

    adjust_replacement_indentation(replaced_lines, &matched_indents[0], first_diff)
}

fn non_empty_indents(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|c| c.is_whitespace()).collect())
        .collect()
}

fn adjust_replacement_indentation(
    replaced_lines: &[String],
    matched_indent: &str,
    diff: isize,
) -> Vec<String> {
    if diff < 0 {
        let prefix_len = usize::try_from(-diff).unwrap_or(0).min(matched_indent.len());
        let prefix = &matched_indent[..prefix_len];
        return replaced_lines.iter().map(|line| format!("{prefix}{line}")).collect();
    }

    let remove_len = usize::try_from(diff).unwrap_or(0);
    if !replaced_lines.iter().all(|line| removable_indent(line, remove_len)) {
        return replaced_lines.to_vec();
    }
    replaced_lines.iter().map(|line| line[remove_len..].to_string()).collect()
}

fn removable_indent(line: &str, remove_len: usize) -> bool {
    line.len() >= remove_len && line[..remove_len].chars().all(char::is_whitespace)
}

fn trim_empty_edge_lines(lines: &[String]) -> Vec<String> {
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Vec::new();
    };
    let last = lines.iter().rposition(|line| !line.trim().is_empty()).unwrap_or(first);
    lines[first..=last].to_vec()
}

fn not_found_error(block: &SearchReplaceBlock, lines: &[String], offset: usize) -> WinxError {
    let snippet = closest_snippet(lines, offset, &block.search);
    WinxError::SearchBlockNotFound(format!(
        "Block not found: {}\nClosest snippet:\n{}",
        block.search.join("\n"),
        snippet
    ))
}

fn closest_snippet(lines: &[String], offset: usize, search: &[String]) -> String {
    let window = search.len().max(1);
    if lines.is_empty() || offset >= lines.len() {
        return String::new();
    }

    let max_start = lines.len().saturating_sub(window);
    let best_start = (offset..=max_start)
        .min_by_key(|start| rough_distance(&lines[*start..(*start + window)], search))
        .unwrap_or(offset);
    lines[best_start..(best_start + window).min(lines.len())].join("\n")
}

fn rough_distance(candidate: &[String], search: &[String]) -> usize {
    candidate
        .iter()
        .zip(search)
        .map(|(candidate_line, search_line)| candidate_line.len().abs_diff(search_line.len()))
        .sum::<usize>()
        + candidate.len().abs_diff(search.len()) * 10
}

fn uses_search_replace(file_write_or_edit: &FileWriteOrEdit) -> bool {
    if file_write_or_edit.percentage_to_change <= 50 {
        return true;
    }

    let first_content_line =
        file_write_or_edit.text_or_search_replace_blocks.trim_start().lines().next();
    first_content_line.is_some_and(|line| search_marker().is_ok_and(|marker| marker.is_match(line)))
}

#[instrument(level = "info", skip(bash_state_arc, file_write_or_edit))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    file_write_or_edit: FileWriteOrEdit,
) -> Result<String> {
    let mut bash_state_guard = bash_state_arc.lock().await;
    let bash_state = bash_state_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

    let thread_id = normalize_thread_id(&file_write_or_edit.thread_id);
    if thread_id != bash_state.current_thread_id {
        return Err(WinxError::ThreadIdMismatch(thread_id));
    }

    let expanded_path = expand_user(&file_write_or_edit.file_path);
    let path = if Path::new(&expanded_path).is_absolute() {
        PathBuf::from(&expanded_path)
    } else {
        bash_state.cwd.join(&expanded_path)
    };

    let path = validate_path_in_workspace(&path, &bash_state.workspace_root)
        .map_err(|e| WinxError::PathSecurityError { path: path.clone(), message: e.to_string() })?;

    let file_path_str = path.to_string_lossy().to_string();

    let uses_search_replace = uses_search_replace(&file_write_or_edit);
    let operation_allowed = if uses_search_replace {
        bash_state.is_file_edit_allowed(&file_path_str)
    } else {
        bash_state.is_file_write_allowed(&file_path_str)
    };

    if !operation_allowed {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "File operation not allowed in current mode.".to_string(),
        });
    }

    // Whitelist check (WCGW style)
    if path.exists() && !bash_state.whitelist_for_overwrite.contains_key(&file_path_str) {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "Read file first before editing.".to_string(),
        });
    }

    let result = if uses_search_replace {
        let original_content = fs::read_to_string(&path)?;
        let blocks = parse_blocks(&file_write_or_edit.text_or_search_replace_blocks)?;
        let new_content = apply_blocks(&original_content, &blocks)?;

        fs::write(&path, &new_content)?;
        format!("Successfully edited {file_path_str}")
    } else {
        fs::write(&path, &file_write_or_edit.text_or_search_replace_blocks)?;
        format!("Successfully wrote {file_path_str}")
    };

    // Update whitelist
    let final_content = fs::read_to_string(&path)?;
    let digest = Sha256::digest(final_content.as_bytes());
    let hash = digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    });
    let total_lines = final_content.lines().count();

    bash_state
        .whitelist_for_overwrite
        .insert(file_path_str, FileWhitelistData::new(hash, vec![(1, total_lines)], total_lines));

    Ok(result)
}
