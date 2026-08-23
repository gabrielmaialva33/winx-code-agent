#![allow(clippy::expect_used)]

use std::fmt::Write as _;
use std::path::Path;

use super::commit::{commit_edit, plan_edit};
use super::matcher::{
    apply_blocks, apply_blocks_with_unescape_retry, closest_snippet, fix_indentation,
};
use super::parser::{parse_blocks, SearchReplaceBlock};
use super::report::{change_summary, operation_result, MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES};
use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;

#[test]
fn closest_snippet_search_longer_than_file_does_not_panic() {
    let lines = vec!["the only line".to_string()];
    let search = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let (snippet, similarity) = closest_snippet(&lines, 0, &search);
    assert!(snippet.is_empty());
    assert!(similarity.abs() < f64::EPSILON, "expected 0.0 similarity, got {similarity}");
}

#[test]
fn closest_snippet_normal_case_still_finds_a_window() {
    let lines = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    let search = vec!["beta".to_string()];
    let (snippet, similarity) = closest_snippet(&lines, 0, &search);
    assert!(snippet.contains("beta"));
    assert!(similarity > 0.0);
}

#[test]
fn fix_indentation_adds_multibyte_indent_without_panic() {
    let matched = vec!["\u{3000}\u{3000}x".to_string()];
    let searched = vec!["\u{3000}x".to_string()];
    let replaced = vec!["y".to_string()];
    assert_eq!(fix_indentation(&matched, &searched, &replaced), vec!["\u{3000}y"]);
}

#[test]
fn fix_indentation_removes_multibyte_indent_without_panic() {
    let matched = vec!["\u{3000}x".to_string()];
    let searched = vec!["\u{3000}\u{3000}x".to_string()];
    let replaced = vec!["\u{3000}y".to_string()];
    assert_eq!(fix_indentation(&matched, &searched, &replaced), vec!["y"]);
}

#[test]
fn apply_blocks_preserves_crlf_endings() -> Result<()> {
    let content = "line one\r\nline two\r\nline three\r\n";
    let block = SearchReplaceBlock {
        search: vec!["line two".to_string()],
        replace: vec!["line TWO".to_string()],
        ..Default::default()
    };
    let (output, _) = apply_blocks(content, &[block])?;
    assert_eq!(output, "line one\r\nline TWO\r\nline three\r\n");
    Ok(())
}

#[test]
fn apply_blocks_leaves_lf_files_as_lf() -> Result<()> {
    let content = "a\nb\nc\n";
    let block = SearchReplaceBlock {
        search: vec!["b".to_string()],
        replace: vec!["B".to_string()],
        ..Default::default()
    };
    let (output, _) = apply_blocks(content, &[block])?;
    assert_eq!(output, "a\nB\nc\n");
    assert!(!output.contains('\r'));
    Ok(())
}

#[test]
fn apply_blocks_reports_indentation_tolerance() -> Result<()> {
    let content = "  alpha\n  beta\n";
    let block = SearchReplaceBlock {
        search: vec!["alpha".to_string(), "beta".to_string()],
        replace: vec!["alpha".to_string(), "BETA".to_string()],
        ..Default::default()
    };
    let (_, tolerances) = apply_blocks(content, &[block])?;
    assert!(!tolerances.is_empty());
    Ok(())
}

#[test]
fn apply_blocks_exact_match_reports_no_tolerances() -> Result<()> {
    let block = SearchReplaceBlock {
        search: vec!["b".to_string()],
        replace: vec!["B".to_string()],
        ..Default::default()
    };
    let (_, tolerances) = apply_blocks("a\nb\nc\n", &[block])?;
    assert!(tolerances.is_empty());
    Ok(())
}

#[test]
fn anchor_parses_start_and_range() -> Result<()> {
    let ranged = parse_blocks("<<<<<<< SEARCH @5-8\nfoo\n=======\nbar\n>>>>>>> REPLACE")?;
    assert_eq!(ranged[0].anchor_start, Some(5));
    assert_eq!(ranged[0].anchor_end, Some(8));
    let single = parse_blocks("<<<<<<< SEARCH @3\nfoo\n=======\nbar\n>>>>>>> REPLACE")?;
    assert_eq!(single[0].anchor_start, Some(3));
    assert_eq!(single[0].anchor_end, None);
    let plain = parse_blocks("<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE")?;
    assert_eq!(plain[0].anchor_start, None);
    Ok(())
}

#[test]
fn anchor_disambiguates_a_repeated_block() -> Result<()> {
    let raw = "<<<<<<< SEARCH @3\nx\n=======\nY\n>>>>>>> REPLACE";
    let (output, _) = apply_blocks_with_unescape_retry("x\nx\nx\n", raw)?;
    assert_eq!(output, "x\nx\nY\n");
    Ok(())
}

#[test]
fn stale_anchor_falls_back_to_normal_search() -> Result<()> {
    let raw = "<<<<<<< SEARCH @99\nx\n=======\nY\n>>>>>>> REPLACE";
    let (output, _) = apply_blocks_with_unescape_retry("a\nx\nb\n", raw)?;
    assert_eq!(output, "a\nY\nb\n");
    Ok(())
}

#[test]
fn later_anchor_uses_original_lines_when_an_earlier_block_changes_line_count() -> Result<()> {
    let raw = "<<<<<<< SEARCH @2\nfirst target\n=======\ninserted one\ninserted two\n>>>>>>> REPLACE\n<<<<<<< SEARCH @4\nsecond target\n=======\nSECOND\n>>>>>>> REPLACE";
    let content = "start\nfirst target\nmiddle\nsecond target\nend\n";
    let (output, _) = apply_blocks_with_unescape_retry(content, raw)?;
    assert_eq!(output, "start\ninserted one\ninserted two\nmiddle\nSECOND\nend\n");
    Ok(())
}

#[test]
fn change_summary_is_none_for_identical_content() {
    assert!(change_summary("a\nb\nc\n", "a\nb\nc\n").is_none());
}

#[test]
fn change_summary_shows_diff_and_counts() {
    let summary = change_summary("a\nb\nc\n", "a\nB\nc\n").expect("content changed");
    assert!(summary.contains("+1 -1"));
    assert!(summary.contains("-b"));
    assert!(summary.contains("+B"));
}

#[test]
fn operation_result_includes_diff_when_previous_differs() {
    let result =
        operation_result("edited", "n.txt", Path::new("n.txt"), "a\nB\n", &[], Some("a\nb\n"));
    assert!(result.contains("Successfully edited n.txt"));
    assert!(result.contains("Changes (+1 -1)"));
}

#[test]
fn operation_result_has_no_diff_for_a_new_file() {
    let result = operation_result("wrote", "n.txt", Path::new("n.txt"), "hello\n", &[], None);
    assert!(result.contains("Successfully wrote n.txt"));
    assert!(!result.contains("Changes"));
}

#[test]
fn change_summary_skips_myers_on_oversized_input() {
    let big = "x\n".repeat(MAX_DIFF_INPUT_BYTES);
    let summary = change_summary("", &big).expect("content changed");
    assert!(summary.contains("file too large to diff"));
}

#[test]
fn change_summary_collapses_a_huge_diff() {
    let big = (0..MAX_DIFF_LINES + 50).fold(String::new(), |mut output, index| {
        let _ = writeln!(output, "line {index}");
        output
    });
    let summary = change_summary("", &big).expect("content changed");
    assert!(summary.contains("diff too large to show"));
    assert!(!summary.contains("line 10"));
}

#[test]
fn file_tool_creates_a_helper_only_in_its_advertised_session_directory() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let mut state = BashState::new();
    state.cwd.clone_from(&root);
    state.workspace_root.clone_from(&root);
    state.current_thread_id = "temp-policy".to_string();
    let helper = crate::utils::agent_temp::session_info(&root, &state.current_thread_id)
        .directory
        .join("review/adapter.py");

    let planned = plan_edit(
        &state,
        helper.to_string_lossy().as_ref(),
        100,
        "# source_path: lib/original.ex:1-20\n",
        true,
    )?;
    commit_edit(&mut state, planned)?;

    assert_eq!(std::fs::read_to_string(helper)?, "# source_path: lib/original.ex:1-20\n");
    Ok(())
}

#[test]
fn file_tool_rejects_cross_session_and_legacy_root_helpers() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let mut state = BashState::new();
    state.cwd.clone_from(&root);
    state.workspace_root.clone_from(&root);
    state.current_thread_id = "active-temp-policy".to_string();

    let other = crate::utils::agent_temp::session_info(&root, "another-session")
        .directory
        .join("adapter.py");
    let error = plan_edit(&state, other.to_string_lossy().as_ref(), 100, "derived\n", true)
        .err()
        .expect("cross-session write must fail");
    assert!(matches!(error, WinxError::TemporaryArtifactPolicy { .. }));

    let legacy = root.join(".winx_tmp/payload/adapter.py");
    let error = plan_edit(&state, legacy.to_string_lossy().as_ref(), 100, "derived\n", true)
        .err()
        .expect("legacy root write must fail");
    assert!(matches!(error, WinxError::TemporaryArtifactPolicy { .. }));
    assert!(!root.join(".winx_tmp").exists());
    Ok(())
}
