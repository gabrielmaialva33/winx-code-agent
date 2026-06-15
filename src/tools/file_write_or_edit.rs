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

/// Apply search/replace blocks, retrying once with `\"` unescaped if the first
/// attempt fails to match. LLMs frequently over-escape quotes in SEARCH text;
/// wcgw does the same fallback in `do_diff_edit`.
fn apply_blocks_with_unescape_retry(original: &str, raw: &str) -> Result<String> {
    let blocks = parse_blocks(raw)?;
    match apply_blocks(original, &blocks) {
        Ok(content) => Ok(content),
        Err(first_err) => {
            let unescaped = raw.replace("\\\"", "\"");
            if unescaped == raw {
                return Err(first_err);
            }
            let retry_blocks = parse_blocks(&unescaped).map_err(|_| first_err)?;
            apply_blocks(original, &retry_blocks)
        }
    }
}

fn apply_blocks(content: &str, blocks: &[SearchReplaceBlock]) -> Result<String> {
    // `parse_blocks` reads the LLM's SEARCH/REPLACE via `.lines()`, which strips
    // `\r`, so blocks are always LF. Normalize the file to LF too: otherwise a
    // CRLF line matches only via the TrimEnd tolerance (inflating the score) and
    // untouched lines keep `\r\n` while replacement lines come back as bare `\n`,
    // silently turning the file into mixed line endings. Re-apply CRLF on join.
    let uses_crlf = content.contains("\r\n");
    let normalized;
    let content_lf: &str = if uses_crlf {
        normalized = content.replace("\r\n", "\n");
        &normalized
    } else {
        content
    };

    let original_lines = split_lines(content_lf);
    let edited = apply_blocks_ordered(&original_lines, blocks).or_else(|ordered_error| {
        if blocks.len() == 1 {
            Err(ordered_error)
        } else {
            apply_blocks_individually(&original_lines, blocks)
        }
    })?;

    let joined = edited.join("\n");
    Ok(if uses_crlf { joined.replace('\n', "\r\n") } else { joined })
}

fn split_lines(content: &str) -> Vec<String> {
    content.split('\n').map(str::to_string).collect()
}

/// Atomically write `content` to `path`: write a sibling temp file, fsync it,
/// then `rename` it over the target.
///
/// Two properties this buys us over a plain `truncate`+`write`:
/// - **Atomicity / crash safety.** The old path truncated the target first and
///   streamed bytes in; a mid-write failure (ENOSPC, EIO, the process dying)
///   left the file corrupted or empty. `rename(2)` is atomic within a
///   filesystem, so a reader sees either the old file or the complete new one,
///   never a half-written file.
/// - **Symlink safety.** `validate_path_in_workspace` checks the parent, but a
///   TOCTOU window lets someone swap the target for a symlink before the write.
///   `rename` replaces the target entry without following it, so it can't be
///   redirected outside the workspace — the same guarantee the old `O_NOFOLLOW`
///   gave, kept here.
///
/// The temp file inherits the target's permissions when it already exists, so an
/// edit doesn't silently flip a 0644 file to the temp's default mode. Non-Unix
/// targets fall back to `tempfile`'s cross-platform persist.
fn write_no_follow(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Error;

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // Random-named temp in the SAME directory so `persist` is a same-filesystem
    // rename (cross-fs rename would fail with EXDEV). A fresh random name can't
    // be a pre-planted symlink, so no O_NOFOLLOW is needed on the temp itself.
    let mut tmp =
        tempfile::Builder::new().prefix(".winx-tmp-").tempfile_in(parent).map_err(|e| {
            Error::new(e.kind(), format!("create temp file in {}: {e}", parent.display()))
        })?;

    // Preserve the existing file's permissions (use symlink_metadata so we read
    // the target itself, not a symlink's destination).
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_file() {
            let _ = tmp.as_file().set_permissions(meta.permissions());
        }
    }

    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    // Atomic replace. `persist` maps to `rename`; on conflict it returns the
    // temp back inside the error, which we drop (the temp is cleaned up).
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A single SEARCH block matching more than this many locations is rejected as
/// too ambiguous, instead of fanning the matcher out exponentially.
const MAX_CANDIDATES_PER_BLOCK: usize = 64;
/// Global cap on nodes explored by the ordered matcher — a backstop against the
/// O(C^B) blow-up when several blocks each match in many places.
const MAX_SEARCH_NODES: u32 = 50_000;
/// Refuse to apply an edit whose accumulated tolerance score exceeds this — the
/// match was forced through too many fuzzy fixups to trust. Mirrors wcgw's
/// `replace_or_throw` "Too many warnings, not applying" guard.
const MAX_TOTAL_TOLERANCE_SCORE: usize = 1000;

fn apply_blocks_ordered(lines: &[String], blocks: &[SearchReplaceBlock]) -> Result<Vec<String>> {
    let mut budget = MAX_SEARCH_NODES;
    let (score, replacements) = best_ordered_replacements(lines, blocks, 0, 0, &mut budget)?;
    if score > MAX_TOTAL_TOLERANCE_SCORE {
        return Err(WinxError::SearchBlockNotFound(format!(
            "SEARCH blocks only matched very loosely (tolerance score {score} over limit \
             {MAX_TOTAL_TOLERANCE_SCORE}). The file likely changed since you read it — re-read it \
             and make the SEARCH text match the current content exactly."
        )));
    }
    Ok(apply_replacements(lines, &replacements))
}

fn best_ordered_replacements(
    lines: &[String],
    blocks: &[SearchReplaceBlock],
    block_index: usize,
    offset: usize,
    budget: &mut u32,
) -> Result<(usize, Vec<Replacement>)> {
    if block_index >= blocks.len() {
        return Ok((0, Vec::new()));
    }
    if *budget == 0 {
        return Err(WinxError::SearchBlockNotFound(
            "Search/replace is too ambiguous (too many candidate combinations). Add more \
             surrounding context so each SEARCH block matches a unique location."
                .to_string(),
        ));
    }
    *budget -= 1;

    let block = &blocks[block_index];
    let candidates = find_candidates(lines, block, offset);
    if candidates.is_empty() {
        return Err(not_found_error(block, lines, offset));
    }
    if candidates.len() > MAX_CANDIDATES_PER_BLOCK {
        return Err(WinxError::SearchBlockNotFound(format!(
            "A SEARCH block matches {} locations (limit {MAX_CANDIDATES_PER_BLOCK}); add more \
             surrounding context to make it unique:\n{}",
            candidates.len(),
            block.search.join("\n")
        )));
    }

    let mut valid_paths = Vec::new();
    for candidate in candidates {
        if let Ok((tail_score, mut tail)) =
            best_ordered_replacements(lines, blocks, block_index + 1, candidate.end, budget)
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
    let mut total_score = 0usize;
    for block in blocks {
        let candidate = select_unique_candidate(block, find_candidates(&running_lines, block, 0))?;
        // Enforce the same fuzzy-fixup ceiling as `apply_blocks_ordered`. Without
        // this, a multi-block edit that fell back to per-block matching could
        // apply very loosely-matched blocks the ordered path would have rejected.
        total_score = total_score.saturating_add(candidate.score);
        if total_score > MAX_TOTAL_TOLERANCE_SCORE {
            return Err(WinxError::SearchBlockNotFound(format!(
                "SEARCH blocks only matched very loosely (tolerance score {total_score} over \
                 limit {MAX_TOTAL_TOLERANCE_SCORE}). The file likely changed since you read it — \
                 re-read it and make the SEARCH text match the current content exactly."
            )));
        }
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
        candidates = find_single_line_substring_candidates(lines, block, offset);
    }
    if candidates.is_empty() {
        candidates = find_contiguous_candidates(lines, block, offset, true);
    }
    candidates
}

fn find_single_line_substring_candidates(
    lines: &[String],
    block: &SearchReplaceBlock,
    offset: usize,
) -> Vec<MatchCandidate> {
    if block.search.len() != 1 {
        return Vec::new();
    }

    let search = &block.search[0];
    if search.is_empty() {
        return Vec::new();
    }

    let replace = block.replace.join("\n");
    lines
        .iter()
        .enumerate()
        .skip(offset)
        .flat_map(|(index, line)| {
            let replace = replace.clone();
            line.match_indices(search).map(move |(byte_index, _)| {
                let mut replaced_line = line.clone();
                replaced_line.replace_range(byte_index..byte_index + search.len(), &replace);
                MatchCandidate {
                    start: index,
                    end: index + 1,
                    score: 0,
                    tolerances: Vec::new(),
                    replace: split_lines(&replaced_line),
                }
            })
        })
        .collect()
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

    // Count by chars, not bytes: indentation can contain multibyte whitespace
    // (NBSP, ideographic space). A byte-based delta would later slice mid-code-point.
    let diffs: Vec<isize> = matched_indents
        .iter()
        .zip(&searched_indents)
        .map(|(matched, searched)| {
            searched.chars().count() as isize - matched.chars().count() as isize
        })
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
    // `diff`/`prefix_len`/`remove_len` are CHAR counts (see `diffs` in
    // `fix_indentation`), so all slicing here goes through char iterators —
    // never raw byte indices that could split a multibyte whitespace char.
    if diff < 0 {
        let prefix_len = usize::try_from(-diff).unwrap_or(0);
        let prefix: String = matched_indent.chars().take(prefix_len).collect();
        return replaced_lines.iter().map(|line| format!("{prefix}{line}")).collect();
    }

    let remove_len = usize::try_from(diff).unwrap_or(0);
    if !replaced_lines.iter().all(|line| removable_indent(line, remove_len)) {
        return replaced_lines.to_vec();
    }
    replaced_lines.iter().map(|line| line.chars().skip(remove_len).collect()).collect()
}

fn removable_indent(line: &str, remove_len: usize) -> bool {
    line.chars().take(remove_len).filter(|c| c.is_whitespace()).count() == remove_len
}

fn trim_empty_edge_lines(lines: &[String]) -> Vec<String> {
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Vec::new();
    };
    let last = lines.iter().rposition(|line| !line.trim().is_empty()).unwrap_or(first);
    lines[first..=last].to_vec()
}

/// Lines of surrounding context shown around the closest match (wcgw parity:
/// `find_least_edit_distance_substring` returns the match ± 10 lines).
const SNIPPET_CONTEXT_LINES: usize = 10;

fn not_found_error(block: &SearchReplaceBlock, lines: &[String], offset: usize) -> WinxError {
    let snippet = closest_snippet(lines, offset, &block.search);
    WinxError::SearchBlockNotFound(format!(
        "Block not found in file. The SEARCH block below didn't match anywhere:\n{}\n\n\
         Closest matching context in the file (with surrounding lines):\n{}",
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
    let mut best_start = offset;
    let mut best_score = f64::MIN;
    for start in offset..=max_start {
        let score = snippet_similarity(&lines[start..(start + window)], search);
        if score > best_score {
            best_score = score;
            best_start = start;
        }
    }

    // Widen to ±10 lines around the best match so the model can locate it, with
    // 1-based line numbers (the file is shown numbered elsewhere too).
    let context_start = best_start.saturating_sub(SNIPPET_CONTEXT_LINES);
    let context_end = (best_start + window + SNIPPET_CONTEXT_LINES).min(lines.len());
    lines[context_start..context_end]
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}  {}", context_start + index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn snippet_similarity(candidate: &[String], search: &[String]) -> f64 {
    candidate
        .iter()
        .zip(search)
        .map(|(candidate_line, search_line)| {
            strsim::normalized_levenshtein(candidate_line.trim(), search_line.trim())
        })
        .sum::<f64>()
        - candidate.len().abs_diff(search.len()) as f64
}

fn uses_search_replace(file_write_or_edit: &FileWriteOrEdit) -> bool {
    if file_write_or_edit.percentage_to_change <= 50 {
        return true;
    }

    let first_content_line =
        file_write_or_edit.text_or_search_replace_blocks.trim_start().lines().next();
    first_content_line.is_some_and(|line| search_marker().is_ok_and(|marker| marker.is_match(line)))
}

fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    })
}

fn format_unread_ranges(whitelist: &FileWhitelistData) -> String {
    whitelist
        .get_unread_ranges()
        .into_iter()
        .map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") })
        .collect::<Vec<_>>()
        .join(", ")
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

    if path.exists() {
        let whitelist =
            bash_state.whitelist_for_overwrite.get(&file_path_str).ok_or_else(|| {
                WinxError::FileAccessError {
                    path: path.clone(),
                    message: format!(
                        "This file exists but hasn't been read in this session. \
                         Call ReadFiles on {file_path_str} first, then retry the edit \
                         (winx requires a fresh read so edits are never applied blind)."
                    ),
                }
            })?;
        let original_content = fs::read_to_string(&path)?;
        let current_hash = hash_content(&original_content);
        if whitelist.file_hash != current_hash {
            return Err(WinxError::FileAccessError {
                path: path.clone(),
                message: format!(
                    "{file_path_str} changed on disk since you last read it. \
                     Call ReadFiles again to get the current content, then retry the edit."
                ),
            });
        }
        if !uses_search_replace && !whitelist.is_read_enough() {
            return Err(WinxError::FileAccessError {
                path: path.clone(),
                message: format!(
                    "Read more of the file before overwriting. Unread line ranges: {}",
                    format_unread_ranges(whitelist)
                ),
            });
        }
    }

    let result = if uses_search_replace {
        let original_content = fs::read_to_string(&path)?;
        let new_content = apply_blocks_with_unescape_retry(
            &original_content,
            &file_write_or_edit.text_or_search_replace_blocks,
        )?;

        write_no_follow(&path, new_content.as_bytes())?;
        operation_result("edited", &file_path_str, &path, &new_content)
    } else {
        write_no_follow(&path, file_write_or_edit.text_or_search_replace_blocks.as_bytes())?;
        operation_result(
            "wrote",
            &file_path_str,
            &path,
            &file_write_or_edit.text_or_search_replace_blocks,
        )
    };

    // Update whitelist
    let final_content = fs::read_to_string(&path)?;
    let hash = hash_content(&final_content);
    let total_lines = final_content.lines().count();

    bash_state
        .whitelist_for_overwrite
        .insert(file_path_str, FileWhitelistData::new(hash, vec![(1, total_lines)], total_lines));
    if uses_search_replace {
        let _ = crate::utils::workspace_stats::record_edit(&bash_state.workspace_root, &path);
    } else {
        let _ = crate::utils::workspace_stats::record_write(&bash_state.workspace_root, &path);
    }

    Ok(result)
}

fn operation_result(action: &str, file_path: &str, path: &Path, content: &str) -> String {
    let mut result = format!("Successfully {action} {file_path}");
    if let Some(warning) = crate::utils::syntax::syntax_warning(path, content) {
        let _ = write!(result, "\n\n{warning}");
    }
    result
}

#[cfg(test)]
mod indentation_tests {
    use super::*;

    // The indent fixer used to byte-slice over indentation that can contain
    // multibyte whitespace (ideographic space U+3000, NBSP) — a guaranteed
    // panic. These pin the char-based behavior.

    #[test]
    fn fix_indentation_adds_multibyte_indent_without_panic() {
        // matched has 2 ideographic spaces, searched has 1 -> diff = -1 (add 1).
        let matched = vec!["\u{3000}\u{3000}x".to_string()];
        let searched = vec!["\u{3000}x".to_string()];
        let replaced = vec!["y".to_string()];
        let out = fix_indentation(&matched, &searched, &replaced);
        assert_eq!(out, vec!["\u{3000}y".to_string()]);
    }

    #[test]
    fn fix_indentation_removes_multibyte_indent_without_panic() {
        // matched has 1 ideographic space, searched has 2 -> diff = +1 (remove 1).
        let matched = vec!["\u{3000}x".to_string()];
        let searched = vec!["\u{3000}\u{3000}x".to_string()];
        let replaced = vec!["\u{3000}y".to_string()];
        let out = fix_indentation(&matched, &searched, &replaced);
        assert_eq!(out, vec!["y".to_string()]);
    }

    #[test]
    fn apply_blocks_preserves_crlf_endings() -> Result<()> {
        // A CRLF file must round-trip as CRLF — not turn into mixed endings where
        // the edited line is bare LF while untouched lines keep CRLF.
        let content = "line one\r\nline two\r\nline three\r\n";
        let block = SearchReplaceBlock {
            search: vec!["line two".to_string()],
            replace: vec!["line TWO".to_string()],
        };
        let out = apply_blocks(content, &[block])?;
        assert_eq!(out, "line one\r\nline TWO\r\nline three\r\n");
        Ok(())
    }

    #[test]
    fn apply_blocks_leaves_lf_files_as_lf() -> Result<()> {
        let content = "a\nb\nc\n";
        let block =
            SearchReplaceBlock { search: vec!["b".to_string()], replace: vec!["B".to_string()] };
        let out = apply_blocks(content, &[block])?;
        assert_eq!(out, "a\nB\nc\n");
        assert!(!out.contains('\r'));
        Ok(())
    }
}
