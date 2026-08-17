use super::parser::{parse_blocks, SearchReplaceBlock};
use crate::errors::{Result, WinxError};

const MAX_CANDIDATES_PER_BLOCK: usize = 64;
const MAX_SEARCH_NODES: u32 = 50_000;
const MAX_TOTAL_TOLERANCE_SCORE: usize = 1_000;
const SNIPPET_CONTEXT_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToleranceKind {
    TrimEnd,
    IgnoreIndentation,
    RemoveLineNumbers,
    NormalizeCommonMistakes,
    IgnoreWhitespace,
}

impl ToleranceKind {
    fn score(self) -> usize {
        match self {
            Self::TrimEnd => 1,
            Self::RemoveLineNumbers | Self::NormalizeCommonMistakes => 5,
            Self::IgnoreIndentation => 10,
            Self::IgnoreWhitespace => 50,
        }
    }

    pub(super) fn display_name(self) -> &'static str {
        match self {
            Self::TrimEnd => "trailing whitespace",
            Self::RemoveLineNumbers => "line-number prefixes",
            Self::NormalizeCommonMistakes => "smart-quote/dash normalization",
            Self::IgnoreIndentation => "indentation",
            Self::IgnoreWhitespace => "all whitespace",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineMatch {
    Exact,
    Tolerated(ToleranceKind),
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
    tolerances: Vec<ToleranceKind>,
}

pub(super) fn apply_blocks_with_unescape_retry(
    original: &str,
    raw: &str,
) -> Result<(String, Vec<ToleranceKind>)> {
    let blocks = parse_blocks(raw)?;
    match apply_blocks(original, &blocks) {
        Ok(result) => Ok(result),
        Err(first_error) => {
            let unescaped = raw.replace("\\\"", "\"");
            if unescaped == raw {
                return Err(first_error);
            }
            let retry_blocks = parse_blocks(&unescaped).map_err(|_| first_error)?;
            apply_blocks(original, &retry_blocks)
        }
    }
}

pub(super) fn apply_blocks(
    content: &str,
    blocks: &[SearchReplaceBlock],
) -> Result<(String, Vec<ToleranceKind>)> {
    let uses_crlf = content.contains("\r\n");
    let normalized;
    let content_lf = if uses_crlf {
        normalized = content.replace("\r\n", "\n");
        normalized.as_str()
    } else {
        content
    };

    let original_lines = split_lines(content_lf);
    let (edited, tolerances) =
        apply_blocks_ordered(&original_lines, blocks).or_else(|ordered_error| {
            if blocks.len() == 1 {
                Err(ordered_error)
            } else {
                apply_blocks_individually(&original_lines, blocks)
            }
        })?;

    let joined = edited.join("\n");
    let output = if uses_crlf { joined.replace('\n', "\r\n") } else { joined };
    Ok((output, tolerances))
}

fn split_lines(content: &str) -> Vec<String> {
    content.split('\n').map(str::to_string).collect()
}

fn collect_tolerances(replacements: &[Replacement]) -> Vec<ToleranceKind> {
    let mut output = Vec::new();
    for replacement in replacements {
        for &tolerance in &replacement.tolerances {
            if !output.contains(&tolerance) {
                output.push(tolerance);
            }
        }
    }
    output
}

fn apply_blocks_ordered(
    lines: &[String],
    blocks: &[SearchReplaceBlock],
) -> Result<(Vec<String>, Vec<ToleranceKind>)> {
    let mut budget = MAX_SEARCH_NODES;
    let (score, replacements) = best_ordered_replacements(lines, blocks, 0, 0, &mut budget)?;
    if score > MAX_TOTAL_TOLERANCE_SCORE {
        return Err(WinxError::SearchBlockNotFound(format!(
            "SEARCH blocks only matched very loosely (tolerance score {score} over limit \
             {MAX_TOTAL_TOLERANCE_SCORE}). The file likely changed since you read it — re-read it \
             and make the SEARCH text match the current content exactly."
        )));
    }
    let tolerances = collect_tolerances(&replacements);
    Ok((apply_replacements(lines, &replacements), tolerances))
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
                tolerances: candidate.tolerances,
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
    let best_paths: Vec<_> = paths.into_iter().filter(|(score, _)| *score == best_score).collect();
    if best_paths.len() == 1 {
        return best_paths.into_iter().next().ok_or_else(|| {
            WinxError::SearchBlockNotFound(format!("Block not found: {}", block.search.join("\n")))
        });
    }

    let ranges = best_paths
        .iter()
        .filter_map(|(_, replacements)| {
            replacements
                .first()
                .map(|replacement| format!("{}-{}", replacement.start + 1, replacement.end))
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(WinxError::SearchBlockAmbiguous {
        block_content: block.search.join("\n"),
        match_count: best_paths.len(),
        suggestions: vec![
            format!("Equally-good matches at lines: {ranges}"),
            "Add more context before or after this block to make it unique.".to_string(),
        ],
    })
}

fn apply_blocks_individually(
    lines: &[String],
    blocks: &[SearchReplaceBlock],
) -> Result<(Vec<String>, Vec<ToleranceKind>)> {
    let mut running_lines = lines.to_vec();
    let mut total_score = 0_usize;
    let mut tolerances = Vec::new();
    for block in blocks {
        let candidate = select_unique_candidate(block, find_candidates(&running_lines, block, 0))?;
        total_score = total_score.saturating_add(candidate.score);
        if total_score > MAX_TOTAL_TOLERANCE_SCORE {
            return Err(WinxError::SearchBlockNotFound(format!(
                "SEARCH blocks only matched very loosely (tolerance score {total_score} over \
                 limit {MAX_TOTAL_TOLERANCE_SCORE}). The file likely changed since you read it — \
                 re-read it and make the SEARCH text match the current content exactly."
            )));
        }
        for &tolerance in &candidate.tolerances {
            if !tolerances.contains(&tolerance) {
                tolerances.push(tolerance);
            }
        }
        running_lines = apply_replacements(
            &running_lines,
            &[Replacement {
                start: candidate.start,
                end: candidate.end,
                replace: candidate.replace,
                tolerances: Vec::new(),
            }],
        );
    }
    Ok((running_lines, tolerances))
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
    let best: Vec<_> =
        candidates.into_iter().filter(|candidate| candidate.score == best_score).collect();
    if best.len() == 1 {
        return best.into_iter().next().ok_or_else(|| {
            WinxError::SearchBlockNotFound(format!("Block not found: {}", block.search.join("\n")))
        });
    }

    let ranges = best
        .iter()
        .map(|candidate| format!("{}-{}", candidate.start + 1, candidate.end))
        .collect::<Vec<_>>()
        .join(", ");
    Err(WinxError::SearchBlockAmbiguous {
        block_content: block.search.join("\n"),
        match_count: best.len(),
        suggestions: vec![
            format!("Equally-good matches at lines: {ranges}"),
            "Add more context to make the search block unique.".to_string(),
        ],
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
    narrow_to_anchor(candidates, block)
}

fn narrow_to_anchor(
    candidates: Vec<MatchCandidate>,
    block: &SearchReplaceBlock,
) -> Vec<MatchCandidate> {
    let Some(start) = block.anchor_start else {
        return candidates;
    };
    let low = start.saturating_sub(1);
    let high = block.anchor_end.unwrap_or(start).saturating_sub(1);
    let anchored: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.start >= low && candidate.start <= high)
        .cloned()
        .collect();
    if anchored.is_empty() {
        candidates
    } else {
        anchored
    }
}

fn find_single_line_substring_candidates(
    lines: &[String],
    block: &SearchReplaceBlock,
    offset: usize,
) -> Vec<MatchCandidate> {
    if block.search.len() != 1 || block.search[0].is_empty() {
        return Vec::new();
    }
    let search = &block.search[0];
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
            let actual_lines: Vec<_> = lines[start..end].iter().collect();
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
    let compact_lines: Vec<_> =
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
            let actual_lines: Vec<_> =
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
        replace = fix_indentation(&lines[start..end], search_lines, &replace);
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
    value.chars().filter(|character| !character.is_whitespace()).collect()
}

fn remove_leading_line_number(value: &str) -> String {
    value
        .split_once(' ')
        .filter(|(prefix, _)| {
            !prefix.is_empty() && prefix.chars().all(|character| character.is_ascii_digit())
        })
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

pub(super) fn fix_indentation(
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
        .map(|(matched, searched)| {
            let searched = isize::try_from(searched.chars().count()).unwrap_or(isize::MAX);
            let matched = isize::try_from(matched.chars().count()).unwrap_or(isize::MAX);
            searched - matched
        })
        .collect();
    let Some(&first_diff) = diffs.first() else {
        return replaced_lines.to_vec();
    };
    if first_diff == 0 || !diffs.iter().all(|diff| *diff == first_diff) {
        return replaced_lines.to_vec();
    }
    adjust_replacement_indentation(replaced_lines, &matched_indents[0], first_diff)
}

fn non_empty_indents(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|character| character.is_whitespace()).collect())
        .collect()
}

fn adjust_replacement_indentation(
    replaced_lines: &[String],
    matched_indent: &str,
    diff: isize,
) -> Vec<String> {
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
    line.chars().take(remove_len).filter(|character| character.is_whitespace()).count()
        == remove_len
}

fn trim_empty_edge_lines(lines: &[String]) -> Vec<String> {
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Vec::new();
    };
    let last = lines.iter().rposition(|line| !line.trim().is_empty()).unwrap_or(first);
    lines[first..=last].to_vec()
}

fn not_found_error(block: &SearchReplaceBlock, lines: &[String], offset: usize) -> WinxError {
    let (snippet, similarity) = closest_snippet(lines, offset, &block.search);
    WinxError::SearchBlockNotFound(format!(
        "Block not found in file. The SEARCH block below didn't match anywhere:\n{}\n\n\
         Closest matching context in the file ({:.0}% similar; lines marked ~ are the ones that \
         diverged from your SEARCH — re-read the file and copy the text exactly):\n{}",
        block.search.join("\n"),
        similarity * 100.0,
        snippet
    ))
}

pub(super) fn closest_snippet(lines: &[String], offset: usize, search: &[String]) -> (String, f64) {
    let window = search.len().max(1);
    if lines.is_empty() || offset >= lines.len() || window > lines.len() {
        return (String::new(), 0.0);
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

    let context_start = best_start.saturating_sub(SNIPPET_CONTEXT_LINES);
    let context_end = (best_start + window + SNIPPET_CONTEXT_LINES).min(lines.len());
    let snippet = lines[context_start..context_end]
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let absolute = context_start + index;
            let marker = if absolute >= best_start && absolute < best_start + window {
                let search_line = &search[absolute - best_start];
                if strsim::normalized_levenshtein(line.trim(), search_line.trim()) < 0.6 {
                    '~'
                } else {
                    ' '
                }
            } else {
                ' '
            };
            format!("{:>6} {marker} {line}", absolute + 1)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let similarity = (best_score / usize_to_f64(window)).clamp(0.0, 1.0);
    (snippet, similarity)
}

fn snippet_similarity(candidate: &[String], search: &[String]) -> f64 {
    candidate
        .iter()
        .zip(search)
        .map(|(candidate_line, search_line)| {
            strsim::normalized_levenshtein(candidate_line.trim(), search_line.trim())
        })
        .sum::<f64>()
        - usize_to_f64(candidate.len().abs_diff(search.len()))
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}
