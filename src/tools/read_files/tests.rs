#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::{
    handle_tool_call_with_budget, parse_read_parallelism, read_file, rendered_line_bytes,
    trim_to_last_line_boundary, ReadFilesOutcome, WorkerCaps,
};
use crate::config::ReadResponseBudget;
use crate::state::bash_state::{BashState, FileWhitelistData};
use crate::types::{FileWriteOrEdit, Modes, ReadFiles};

type Slot = Arc<Mutex<Option<BashState>>>;

fn slot_for(root: &Path, thread_id: &str) -> Slot {
    let mut state = BashState::new();
    state.initialized = true;
    state.cwd = root.to_path_buf();
    state.workspace_root = root.to_path_buf();
    state.current_thread_id = thread_id.to_string();
    state.mode = Modes::Wcgw;
    Arc::new(Mutex::new(Some(state)))
}

fn request(paths: &[String], thread_id: &str, force: bool) -> ReadFiles {
    serde_json::from_value(serde_json::json!({
        "file_paths": paths,
        "thread_id": thread_id,
        "force": force,
    }))
    .expect("valid ReadFiles request")
}

fn budget(call: usize, file: usize, ttl_secs: u64) -> ReadResponseBudget {
    ReadResponseBudget::parse(
        Some(&call.to_string()),
        Some(&file.to_string()),
        Some(&ttl_secs.to_string()),
    )
}

/// Default byte budgets with the opt-in unchanged window enabled (it is off by
/// default), independent of the caller's environment.
fn defaults() -> ReadResponseBudget {
    ReadResponseBudget::parse(None, None, Some("900"))
}

async fn read(slot: &Slot, paths: &[String], budget: ReadResponseBudget) -> ReadFilesOutcome {
    read_as(slot, paths, "thread", false, budget).await
}

async fn read_as(
    slot: &Slot,
    paths: &[String],
    thread_id: &str,
    force: bool,
    budget: ReadResponseBudget,
) -> ReadFilesOutcome {
    handle_tool_call_with_budget(slot, request(paths, thread_id, force), budget)
        .await
        .expect("ReadFiles call")
}

fn write_lines(path: &Path, lines: usize) -> String {
    let mut source = String::new();
    for line in 1..=lines {
        writeln!(source, "line {line}: deterministic payload for read budget tests").unwrap();
    }
    std::fs::write(path, &source).unwrap();
    source
}

/// Exactly what `ReadFiles` renders for an inclusive line range.
fn rendered(source: &str, start: usize, end: usize) -> String {
    source.lines().enumerate().skip(start - 1).take(end + 1 - start).fold(
        String::new(),
        |mut output, (index, line)| {
            writeln!(output, "{} {line}", index + 1).unwrap();
            output
        },
    )
}

fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().to_string_lossy().into_owned()
}

async fn guard(slot: &Slot, path: &Path) -> Option<FileWhitelistData> {
    slot.lock().await.as_ref().unwrap().whitelist_for_overwrite.get(&canonical(path)).cloned()
}

#[test]
fn token_cut_never_exposes_a_partial_line() {
    let mut content = "complete line\npartial line suffix".to_string();
    content.truncate("complete line\npartial".len());
    trim_to_last_line_boundary(&mut content);
    assert_eq!(content, "complete line\n");

    let mut first_line_only = "partial first line".to_string();
    first_line_only.truncate(7);
    trim_to_last_line_boundary(&mut first_line_only);
    assert!(first_line_only.is_empty(), "expected empty first_line_only");
}

#[test]
fn token_truncation_whitelists_only_complete_visible_lines() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("large.txt");
    write_lines(&path, 200);
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("fixture path is not UTF-8"))?;

    let result = read_file(
        path,
        Some(50),
        temp.path(),
        temp.path(),
        false,
        None,
        None,
        WorkerCaps::UNBOUNDED,
    )?;

    assert!(result.token_limit.is_some());
    assert!(result.content.ends_with('\n'));
    let visible_lines = result.content.lines().count();
    let coverage = FileWhitelistData::new(
        result.file_hash,
        vec![(result.first_line, visible_lines)],
        result.total_lines,
    );
    assert!(!coverage.is_read_enough());
    assert_eq!(coverage.get_unread_ranges(), vec![(visible_lines + 1, result.total_lines)]);
    Ok(())
}

#[test]
fn worker_measures_every_line_but_renders_only_its_cap() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("big.txt");
    let source = write_lines(&path, 5_000);
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("fixture path is not UTF-8"))?;
    let caps = WorkerCaps { render: Some(4_096), horizon: 8_192 };

    let result =
        read_file(path, Some(1_000_000), temp.path(), temp.path(), true, None, None, caps)?;

    assert!(result.content.len() <= 4_096);
    assert_eq!(result.content, rendered(&source, 1, result.content.lines().count()));
    assert_eq!(result.requested_bytes, rendered(&source, 1, 5_000).len());
    assert_eq!(result.line_ends.last().copied(), Some(*result.line_ends.last().unwrap()));
    assert!(result.line_ends.last().is_some_and(|end| *end <= 8_192));
    assert_eq!(rendered_line_bytes(1_234, "abc", true), "1234 abc\n".len());
    Ok(())
}

#[tokio::test]
async fn oversized_first_line_records_no_read_coverage() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("single-line.txt");
    std::fs::write(&path, "x".repeat(10_000))?;
    let slot = slot_for(temp.path(), "thread");

    let outcome = read(&slot, &[path.to_string_lossy().into_owned()], budget(2048, 2048, 0)).await;

    let receipt = &outcome.files[0];
    assert!(receipt.truncated);
    assert!(receipt.visible_ranges.is_empty());
    assert_eq!((receipt.next_start_line, receipt.next_end_line), (Some(1), Some(1)));
    assert!(outcome.text.contains("line 1 alone exceeds"), "{}", outcome.text);
    let coverage = guard(&slot, &path).await.expect("hash is still recorded");
    assert_eq!(coverage.line_ranges_read, Vec::<(usize, usize)>::new());
    Ok(())
}

#[test]
fn parallelism_uses_safe_defaults_and_bounds() {
    assert_eq!(parse_read_parallelism(None), 4);
    assert_eq!(parse_read_parallelism(Some("invalid")), 4);
    assert_eq!(parse_read_parallelism(Some("0")), 4);
    assert_eq!(parse_read_parallelism(Some("1")), 1);
    assert_eq!(parse_read_parallelism(Some("8")), 8);
    assert_eq!(parse_read_parallelism(Some("999")), 32);
}

#[tokio::test]
async fn whole_file_over_budget_returns_head_and_exact_next_chunk() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("large.rs");
    let source = write_lines(&path, 2_000);
    let display = path.to_string_lossy().into_owned();
    let slot = slot_for(temp.path(), "thread");
    let budget = budget(16_384, 8_192, 0);

    let head = read(&slot, std::slice::from_ref(&display), budget).await;
    let receipt = &head.files[0];
    assert!(receipt.truncated);
    assert_eq!(receipt.total_lines, 2_000);
    let shown_end = receipt.visible_ranges[0].end_line;
    let head_text = rendered(&source, 1, shown_end);
    assert!(head_text.len() <= 8_192);
    assert!(rendered(&source, 1, shown_end + 1).len() > 8_192, "head must fill the budget");
    assert!(head.text.contains(&format!("```\n{head_text}\n```")), "line-boundary head");
    let next_start = receipt.next_start_line.expect("continuation start");
    let next_end = receipt.next_end_line.expect("continuation end");
    assert_eq!(next_start, shown_end + 1);
    assert!(head.text.contains(&format!("lines 1-{shown_end} of 2000 shown")), "{}", head.text);
    assert!(
        head.text.contains(&format!("Next: ReadFiles \"{display}:{next_start}-{next_end}\"")),
        "{}",
        head.text
    );
    assert!(head.text.contains(&receipt.revision));
    assert_eq!(head.budget.truncated_files, 1);
    assert_eq!(head.budget.returned_bytes, head_text.len());
    assert_eq!(
        head.budget.omitted_bytes,
        rendered(&source, 1, 2_000).len() - head_text.len(),
        "omitted bytes are exact"
    );

    let follow = read(&slot, &[format!("{display}:{next_start}-{next_end}")], budget).await;
    let receipt = &follow.files[0];
    assert!(!receipt.truncated, "{}", follow.text);
    assert_eq!(receipt.visible_ranges[0].start_line, next_start);
    assert_eq!(receipt.visible_ranges[0].end_line, next_end);
    let chunk = rendered(&source, next_start, next_end);
    assert!(follow.text.contains(&format!("```\n{chunk}\n```")));
    assert!(chunk.len() <= 8_192);
    assert!(rendered(&source, next_start, next_end + 1).len() > 8_192, "chunk is maximal");
    assert!(!follow.budget.is_notable());
    let coverage = guard(&slot, &path).await.expect("guard");
    assert!(coverage.covers_range(1, next_end));
    assert!(!coverage.covers_range(1, next_end + 1));
    Ok(())
}

#[tokio::test]
async fn bounded_range_that_fits_is_exact_and_exempt_from_file_cap() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("range.txt");
    let source = write_lines(&path, 400);
    let slot = slot_for(temp.path(), "thread");
    let range = format!("{}:10-200", path.display());

    // 191 rendered lines exceed the 4 KiB per-file cap but fit the call budget.
    let outcome = read(&slot, &[range], budget(16_384, 4_096, 0)).await;

    let expected = rendered(&source, 10, 200);
    assert!(expected.len() > 4_096 && expected.len() <= 16_384);
    assert!(!outcome.files[0].truncated);
    assert!(outcome.text.contains(&format!("```\n{expected}\n```")));
    assert!(!outcome.budget.is_notable());
    Ok(())
}

#[tokio::test]
async fn mixed_batch_keeps_order_errors_and_defers_after_call_budget() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let small = temp.path().join("small.rs");
    std::fs::write(&small, "fn small() {}\n")?;
    let big = temp.path().join("big.txt");
    let big_source = write_lines(&big, 400);
    let mid = temp.path().join("mid.rs");
    write_lines(&mid, 400);
    let tail = temp.path().join("tail.rs");
    write_lines(&tail, 10);
    let paths = vec![
        small.display().to_string(),
        temp.path().join("missing.rs").display().to_string(),
        big.display().to_string(),
        format!("{}:1-200", mid.display()),
        tail.display().to_string(),
    ];
    let slot = slot_for(temp.path(), "thread");

    let outcome = read(&slot, &paths, budget(6_144, 3_072, 0)).await;

    // small, big (per-file cut, batch continues), mid (call-budget cut, stops).
    assert_eq!(outcome.successful_files, 3);
    assert_eq!(outcome.errors.len(), 1, "missing path stays an inline error");
    assert!(outcome.files[0].visible_ranges[0].end_line == 1 && !outcome.files[0].truncated);
    assert!(outcome.files[1].truncated, "big whole file is cut by the per-file cap");
    let big_end = outcome.files[1].visible_ranges[0].end_line;
    assert!(rendered(&big_source, 1, big_end).len() <= 3_072);
    assert!(outcome.files[2].truncated, "mid range is cut by the remaining call budget");
    assert!(outcome.budget.returned_bytes <= 6_144);
    assert_eq!(outcome.budget.deferred, vec![tail.display().to_string()]);
    assert!(outcome.text.contains(&format!("not read: \"{}\"", tail.display())));
    let order = ["small.rs", "missing.rs", "big.txt", "mid.rs:1-200", "not read"]
        .map(|needle| outcome.text.find(needle).expect(needle));
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{}", outcome.text);
    assert!(guard(&slot, &tail).await.is_none(), "deferred files gain no edit evidence");
    let big_guard = guard(&slot, &big).await.expect("big guard");
    assert!(big_guard.covers_range(1, big_end) && !big_guard.covers_range(1, big_end + 1));

    // When only a sliver remains, the next item is deferred whole instead.
    let slot = slot_for(temp.path(), "thread");
    let outcome = read(
        &slot,
        &[format!("{}:1-80", big.display()), format!("{}:1-20", tail.display())],
        budget(4_096, 4_096, 0),
    )
    .await;
    assert_eq!(outcome.successful_files, 1);
    assert!(4_096 - outcome.budget.returned_bytes < 1_024);
    assert_eq!(outcome.budget.deferred, vec![format!("{}:1-20", tail.display())]);
    assert!(guard(&slot, &tail).await.is_none());
    Ok(())
}

#[tokio::test]
async fn unchanged_repeat_is_compacted_and_keeps_the_edit_guard() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("module.rs");
    let source = write_lines(&path, 60);
    let display = path.display().to_string();
    let slot = slot_for(temp.path(), "thread");

    let first = read(&slot, std::slice::from_ref(&display), defaults()).await;
    assert!(first.text.contains(&rendered(&source, 1, 60)));
    let repeat = read(&slot, std::slice::from_ref(&display), defaults()).await;

    assert!(repeat.files[0].unchanged);
    assert_eq!(repeat.files[0].revision, first.files[0].revision);
    assert_eq!(repeat.files[0].visible_ranges[0].end_line, 60);
    assert!(repeat.text.contains("[unchanged: identical to the copy already returned"));
    assert!(repeat.text.contains("force=true"));
    assert!(!repeat.text.contains("line 7: deterministic"), "content must be elided");
    assert_eq!(repeat.budget.unchanged_files, 1);
    assert_eq!(repeat.budget.unchanged_bytes, rendered(&source, 1, 60).len());
    assert!(guard(&slot, &path).await.expect("guard").covers_range(1, 60));

    // The guard is intact, so an edit after a compacted read still succeeds.
    let edit = FileWriteOrEdit {
        file_path: display.clone(),
        percentage_to_change: 10,
        text_or_search_replace_blocks: "<<<<<<< SEARCH\nline 7: deterministic payload for read \
                                        budget tests\n=======\nline 7: edited\n>>>>>>> REPLACE"
            .to_string(),
        thread_id: "thread".to_string(),
    };
    crate::tools::file_write_or_edit::handle_tool_call(&slot, edit).await?;
    let after_edit = read(&slot, std::slice::from_ref(&display), defaults()).await;
    assert!(!after_edit.files[0].unchanged, "an edit invalidates the delivery");
    assert!(after_edit.text.contains("7 line 7: edited"));
    Ok(())
}

#[tokio::test]
async fn changed_forced_small_and_truncated_reads_are_never_compacted() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("notes.txt");
    write_lines(&path, 60);
    let display = vec![path.display().to_string()];
    let slot = slot_for(temp.path(), "thread");
    read(&slot, &display, defaults()).await;

    let forced = read_as(&slot, &display, "thread", true, defaults()).await;
    assert!(!forced.files[0].unchanged && forced.text.contains("line 7: deterministic"));

    std::fs::write(&path, "changed\n".repeat(100))?;
    let changed = read(&slot, &display, defaults()).await;
    assert!(!changed.files[0].unchanged && changed.text.contains("1 changed"));

    let tiny = temp.path().join("tiny.txt");
    std::fs::write(&tiny, "short\n")?;
    let tiny = vec![tiny.display().to_string()];
    read(&slot, &tiny, defaults()).await;
    assert!(!read(&slot, &tiny, defaults()).await.files[0].unchanged, "below elision floor");

    let big = temp.path().join("big.txt");
    write_lines(&big, 400);
    let big = vec![big.display().to_string()];
    read(&slot, &big, budget(4_096, 4_096, 900)).await;
    let again = read(&slot, &big, budget(4_096, 4_096, 900)).await;
    assert!(again.files[0].truncated && !again.files[0].unchanged, "partial reads resend");

    let disabled = budget(49_152, 32_768, 0);
    read(&slot, &display, disabled).await;
    assert!(!read(&slot, &display, disabled).await.files[0].unchanged, "ttl=0 disables");
    Ok(())
}

#[tokio::test]
async fn compaction_is_scoped_to_one_explicit_thread_and_session() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("shared.rs");
    write_lines(&path, 60);
    let display = vec![path.display().to_string()];
    let slot = slot_for(temp.path(), "thread");
    read(&slot, &display, defaults()).await;

    // Another principal or thread owns a different slot and never shares it.
    let other = slot_for(temp.path(), "other-thread");
    assert!(!read_as(&other, &display, "other-thread", false, defaults()).await.files[0].unchanged);

    // An anonymous call may resolve to whichever session was last active.
    assert!(!read_as(&slot, &display, "", false, defaults()).await.files[0].unchanged);

    // Rebinding the slot to another identity drops the ledger.
    let mut foreign = BashState::new();
    foreign.workspace_root = temp.path().to_path_buf();
    foreign.current_thread_id = "foreign".to_string();
    let snapshot = foreign.snapshot();
    slot.lock().await.as_mut().unwrap().apply_snapshot(&snapshot);
    assert!(!read_as(&slot, &display, "foreign", false, defaults()).await.files[0].unchanged);
    Ok(())
}

#[tokio::test]
async fn search_conflict_revocation_forces_a_full_resend_and_refreshes_guard() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("conflict.rs");
    let source = write_lines(&path, 60);
    let display = path.display().to_string();
    let slot = slot_for(temp.path(), "thread");
    read(&slot, std::slice::from_ref(&display), defaults()).await;

    let conflicting = FileWriteOrEdit {
        file_path: display.clone(),
        percentage_to_change: 10,
        text_or_search_replace_blocks: "<<<<<<< SEARCH\nthis text is not in the file\n=======\n\
                                        replacement\n>>>>>>> REPLACE"
            .to_string(),
        thread_id: "thread".to_string(),
    };
    assert!(crate::tools::file_write_or_edit::handle_tool_call(&slot, conflicting).await.is_err());
    assert!(guard(&slot, &path).await.is_none(), "conflict revokes the permit");

    let recovery = read(&slot, std::slice::from_ref(&display), defaults()).await;
    assert!(!recovery.files[0].unchanged, "a revoked permit must never be answered by a marker");
    assert!(recovery.text.contains(&rendered(&source, 1, 60)));
    assert!(guard(&slot, &path).await.expect("guard refreshed").covers_range(1, 60));

    let corrected = FileWriteOrEdit {
        file_path: display.clone(),
        percentage_to_change: 10,
        text_or_search_replace_blocks: "<<<<<<< SEARCH\nline 3: deterministic payload for read \
                                        budget tests\n=======\nline 3: fixed\n>>>>>>> REPLACE"
            .to_string(),
        thread_id: "thread".to_string(),
    };
    crate::tools::file_write_or_edit::handle_tool_call(&slot, corrected).await?;
    assert!(std::fs::read_to_string(&path)?.contains("line 3: fixed\n"));
    Ok(())
}
