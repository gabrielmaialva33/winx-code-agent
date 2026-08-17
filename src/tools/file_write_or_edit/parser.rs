use std::sync::OnceLock;

use regex::Regex;

use crate::errors::{Result, WinxError};

static SEARCH_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
static DIVIDER_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
static REPLACE_MARKER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct SearchReplaceBlock {
    pub(super) search: Vec<String>,
    pub(super) replace: Vec<String>,
    pub(super) anchor_start: Option<usize>,
    pub(super) anchor_end: Option<usize>,
}

fn regex_marker(
    marker: &'static OnceLock<std::result::Result<Regex, regex::Error>>,
    pattern: &'static str,
) -> Result<&'static Regex> {
    marker.get_or_init(|| Regex::new(pattern)).as_ref().map_err(|error| {
        WinxError::ArgumentParseError(format!("Invalid edit marker regex: {error}"))
    })
}

pub(super) fn search_marker() -> Result<&'static Regex> {
    regex_marker(&SEARCH_MARKER, r"(?m)^<<<<<<+\s*SEARCH>?(?:\s*@(\d+)(?:-(\d+))?)?\s*$")
}

fn divider_marker() -> Result<&'static Regex> {
    regex_marker(&DIVIDER_MARKER, r"(?m)^======*\s*$")
}

fn replace_marker() -> Result<&'static Regex> {
    regex_marker(&REPLACE_MARKER, r"(?m)^>>>>>>+\s*REPLACE\s*$")
}

pub(super) fn parse_blocks(content: &str) -> Result<Vec<SearchReplaceBlock>> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut index = 0;

    while index < lines.len() {
        let anchors = search_marker()?.captures(lines[index]).map(|captures| {
            (
                captures.get(1).and_then(|value| value.as_str().parse::<usize>().ok()),
                captures.get(2).and_then(|value| value.as_str().parse::<usize>().ok()),
            )
        });
        if let Some((anchor_start, anchor_end)) = anchors {
            let marker_line = index + 1;
            index += 1;
            let mut search = Vec::new();
            while index < lines.len() && !divider_marker()?.is_match(lines[index]) {
                if search_marker()?.is_match(lines[index])
                    || replace_marker()?.is_match(lines[index])
                {
                    return Err(WinxError::SearchReplaceSyntaxError(format!(
                        "Line {}: stray marker in SEARCH block",
                        index + 1
                    )));
                }
                search.push(lines[index].to_string());
                index += 1;
            }

            if index >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {marker_line}: unclosed SEARCH block - missing ======= marker"
                )));
            }
            if search.is_empty() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {marker_line}: SEARCH block cannot be empty"
                )));
            }

            index += 1;
            let mut replace = Vec::new();
            while index < lines.len() && !replace_marker()?.is_match(lines[index]) {
                if search_marker()?.is_match(lines[index])
                    || divider_marker()?.is_match(lines[index])
                {
                    return Err(WinxError::SearchReplaceSyntaxError(format!(
                        "Line {}: stray marker in REPLACE block",
                        index + 1
                    )));
                }
                replace.push(lines[index].to_string());
                index += 1;
            }

            if index >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(format!(
                    "Line {marker_line}: unclosed block - missing REPLACE marker"
                )));
            }

            blocks.push(SearchReplaceBlock { search, replace, anchor_start, anchor_end });
        } else if divider_marker()?.is_match(lines[index])
            || replace_marker()?.is_match(lines[index])
        {
            return Err(WinxError::SearchReplaceSyntaxError(format!(
                "Line {}: stray marker outside block",
                index + 1
            )));
        }
        index += 1;
    }

    if blocks.is_empty() {
        return Err(WinxError::SearchReplaceSyntaxError("No valid blocks found".to_string()));
    }
    Ok(blocks)
}

pub(super) fn uses_search_replace(percentage_to_change: u32, blocks: &str) -> bool {
    if percentage_to_change <= 50 {
        return true;
    }
    blocks
        .trim_start()
        .lines()
        .next()
        .is_some_and(|line| search_marker().is_ok_and(|marker| marker.is_match(line)))
}
