use std::fmt::Write as _;
use std::path::Path;

use similar::{ChangeTag, TextDiff};

use super::matcher::ToleranceKind;

const DIFF_CONTEXT_LINES: usize = 3;
pub(super) const MAX_DIFF_LINES: usize = 200;
pub(super) const MAX_DIFF_INPUT_BYTES: usize = 512 * 1024;

pub(super) fn change_summary(previous: &str, current: &str) -> Option<String> {
    if previous == current {
        return None;
    }
    if previous.len().saturating_add(current.len()) > MAX_DIFF_INPUT_BYTES {
        let (before, after) = (previous.lines().count(), current.lines().count());
        return Some(format!("Changes: {before} -> {after} lines (file too large to diff)"));
    }

    let diff = TextDiff::from_lines(previous, current);
    let (mut added, mut removed) = (0_usize, 0_usize);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let rendered = diff.unified_diff().context_radius(DIFF_CONTEXT_LINES).to_string();
    if rendered.lines().count() > MAX_DIFF_LINES {
        return Some(format!("Changes: +{added} -{removed} lines (diff too large to show)"));
    }
    Some(format!("Changes (+{added} -{removed}):\n{}", rendered.trim_end()))
}

pub(super) fn operation_result(
    action: &str,
    file_path: &str,
    path: &Path,
    content: &str,
    tolerances: &[ToleranceKind],
    previous: Option<&str>,
) -> String {
    let mut result = format!("Successfully {action} {file_path}");
    if !tolerances.is_empty() {
        let names = tolerances
            .iter()
            .map(|tolerance| tolerance.display_name())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            result,
            "\n\nNote: matched after tolerating {names} differences — your SEARCH text didn't \
             match the file exactly. Re-read the file if you expected an exact match."
        );
    }
    if let Some(diff) = previous.and_then(|previous| change_summary(previous, content)) {
        let _ = write!(result, "\n\n{diff}");
    }
    if let Some(warning) = crate::utils::syntax::syntax_warning(path, content) {
        let _ = write!(result, "\n\n{warning}");
    }
    result
}
