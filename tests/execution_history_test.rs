use std::path::PathBuf;

use anyhow::Context;

use winx_code_agent::state::execution_history::{
    CapturedOutput, ExecutionCapture, ExecutionCatalog, ExecutionScope, HistoryLimits, OutputSource,
};
use winx_code_agent::tools::bash_command::{BashCommandState, BashProcessStatus};

fn scope(thread: &str, workspace: PathBuf) -> ExecutionScope {
    ExecutionScope { thread_id: thread.to_string(), workspace_root: workspace }
}

fn capture(id: &str, output: &str, now: u64, exit: Option<i32>) -> ExecutionCapture {
    ExecutionCapture {
        execution_id: id.to_string(),
        execution_token: None,
        command_generation: Some(1),
        state: BashCommandState {
            process_status: if exit.is_some() {
                BashProcessStatus::Exited
            } else {
                BashProcessStatus::Running
            },
            background_id: None,
            running_for_seconds: None,
            exit_code: exit,
            cwd: PathBuf::from("/workspace"),
            turn_state: None,
        },
        output: CapturedOutput::Snapshot(output.to_string()),
        screen: None,
        explanation: None,
        prompt_receipt: None,
        activity_seq: Some(now),
        observed_at_unix_ms: now,
        label: Some("build".to_string()),
        output_truncated: false,
        dropped_output_file: None,
    }
}

#[test]
fn execution_history_retains_nonzero_exit_after_reopening() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("owner", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    catalog.record(&binding, capture("run-1", "starting\n", 100, None))?;
    let finished =
        catalog.record(&binding, capture("run-1", "starting\nfailed\n", 200, Some(2)))?;
    assert_eq!(finished.exit_code, Some(2));
    drop(catalog);
    let reopened = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let record = reopened.get(&binding, "run-1", 250)?.context("retained execution")?;
    assert_eq!(record.exit_code, Some(2));
    assert_eq!(record.process_status.as_str(), "exited");
    let page = reopened.read(&binding, "run-1", OutputSource::Output, None, 1024, 250)?;
    assert_eq!(page.text, "starting\nfailed\n");
    assert!(!page.has_more);
    Ok(())
}

#[test]
fn execution_history_isolates_principal_scoped_threads_and_workspaces() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let left = scope("p_left__same", temp.path().join("left"));
    let right = scope("p_right__same", temp.path().join("left"));
    let another_workspace = scope("p_left__same", temp.path().join("right"));
    catalog.record(&left, capture("known-id", "left-only", 100, Some(0)))?;
    assert!(catalog.get(&right, "known-id", 101)?.is_none());
    assert!(catalog.get(&another_workspace, "known-id", 101)?.is_none());
    assert_eq!(catalog.list(&right, 101)?.executions.len(), 0);
    Ok(())
}

#[test]
fn execution_history_pages_utf8_without_losing_or_repeating_bytes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("utf8", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "aé🙂z\n", 100, Some(0)))?;
    let first = catalog.read(&binding, "run", OutputSource::Output, None, 4, 101)?;
    assert_eq!(first.text, "aé");
    assert!(first.has_more);
    let second =
        catalog.read(&binding, "run", OutputSource::Output, Some(&first.next_cursor), 4, 101)?;
    assert_eq!(second.text, "🙂");
    let third =
        catalog.read(&binding, "run", OutputSource::Output, Some(&second.next_cursor), 4, 101)?;
    assert_eq!(third.text, "z\n");
    assert!(!third.has_more);
    Ok(())
}

#[test]
fn execution_history_reports_retention_gap_and_rejects_cross_source_cursors() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let limits = HistoryLimits { max_output_bytes: 8, ..HistoryLimits::default() };
    let catalog = ExecutionCatalog::new(temp.path().join("history"), limits);
    let binding = scope("gap", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "12345678", 100, None))?;
    let first = catalog.read(&binding, "run", OutputSource::Output, None, 4, 101)?;
    catalog.record(&binding, capture("run", "1234567890123456", 200, None))?;
    let gap =
        catalog.read(&binding, "run", OutputSource::Output, Some(&first.next_cursor), 4, 201)?;
    assert!(gap.resync_required);
    assert!(gap.gap);
    assert_eq!(gap.retained_start, 8);
    assert!(catalog
        .read(&binding, "run", OutputSource::Screen, Some(&first.next_cursor), 4, 201)
        .is_err());
    Ok(())
}

#[test]
fn execution_history_changes_snapshot_cursors_when_screen_changes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("screen", temp.path().to_path_buf());
    let mut initial = capture("run", "", 100, None);
    initial.screen = Some("before".to_string());
    catalog.record(&binding, initial)?;
    let page = catalog.read(&binding, "run", OutputSource::Screen, None, 4, 101)?;
    let mut changed = capture("run", "", 200, None);
    changed.screen = Some("after".to_string());
    catalog.record(&binding, changed)?;
    let stale =
        catalog.read(&binding, "run", OutputSource::Screen, Some(&page.next_cursor), 4, 201)?;
    assert!(stale.resync_required);
    assert_eq!(stale.text, "");
    Ok(())
}

#[test]
fn execution_history_retention_preserves_active_records_and_expires_terminal_records(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let limits = HistoryLimits { max_records: 2, retention_ms: 50, ..HistoryLimits::default() };
    let catalog = ExecutionCatalog::new(temp.path().join("history"), limits);
    let binding = scope("retention", temp.path().to_path_buf());
    catalog.record(&binding, capture("active", "waiting", 100, None))?;
    catalog.record(&binding, capture("done", "done", 100, Some(0)))?;
    let listing = catalog.list(&binding, 151)?;
    assert_eq!(listing.executions.len(), 1);
    assert_eq!(listing.executions[0].execution_id, "active");
    catalog.record(&binding, capture("other-active", "", 151, None))?;
    assert!(catalog.record(&binding, capture("third-active", "", 151, None)).is_err());
    assert!(catalog.get(&binding, "active", 151)?.is_some());
    Ok(())
}

#[test]
fn execution_history_wait_cursor_reports_only_real_changes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("changes", temp.path().to_path_buf());
    catalog.record(&binding, capture("one", "a", 100, None))?;
    catalog.record(&binding, capture("two", "b", 100, None))?;
    let baseline = catalog.list(&binding, 100)?;
    let mut same = capture("one", "a", 110, None);
    same.activity_seq = Some(100);
    catalog.record(&binding, same)?;
    let unchanged = catalog.changes(&binding, &baseline.cursor, &["one".to_string()], 111)?;
    assert_eq!(unchanged.executions.len(), 0);
    catalog.record(&binding, capture("two", "b", 120, Some(7)))?;
    let changed = catalog.changes(
        &binding,
        &baseline.cursor,
        &["one".to_string(), "two".to_string()],
        121,
    )?;
    assert_eq!(changed.executions.len(), 1);
    assert_eq!(changed.executions[0].execution_id, "two");
    assert_eq!(changed.executions[0].exit_code, Some(7));
    assert!(!changed.resync_required);
    Ok(())
}

#[test]
fn execution_history_serializes_concurrent_writers_without_losing_records() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("history");
    let binding = scope("writers", temp.path().to_path_buf());
    std::thread::scope(|threads| -> anyhow::Result<()> {
        let mut tasks = Vec::new();
        for id in 0..8 {
            let root = root.clone();
            let binding = binding.clone();
            tasks.push(threads.spawn(move || {
                let catalog = ExecutionCatalog::new(root, HistoryLimits::default());
                catalog.record(&binding, capture(&format!("run-{id}"), "output", 100, Some(0)))
            }));
        }
        for task in tasks {
            task.join().map_err(|_| anyhow::anyhow!("writer did not finish"))??;
        }
        Ok(())
    })?;
    let catalog = ExecutionCatalog::new(root, HistoryLimits::default());
    assert_eq!(catalog.list(&binding, 101)?.executions.len(), 8);
    Ok(())
}

#[test]
fn execution_history_terminal_outcomes_do_not_regress_under_reordered_observations(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("reordered", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "finished", 100, Some(3)))?;
    catalog.record(&binding, capture("run", "older running output", 200, None))?;
    let retained = catalog.get(&binding, "run", 201)?.context("retained result")?;
    assert_eq!(retained.process_status.as_str(), "exited");
    assert_eq!(retained.exit_code, Some(3));
    assert_eq!(
        catalog.read(&binding, "run", OutputSource::Output, None, 1024, 201)?.text,
        "finished"
    );
    Ok(())
}

#[test]
fn execution_history_unknown_recovery_does_not_invent_exit_or_block_capacity_forever(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let limits = HistoryLimits { max_records: 1, ..HistoryLimits::default() };
    let catalog = ExecutionCatalog::new(temp.path().join("history"), limits);
    let binding = scope("recovery", temp.path().to_path_buf());
    catalog.record(&binding, capture("lost", "started", 100, None))?;
    let unknown = catalog
        .mark_unknown(&binding, "lost", "runtime_identity_unavailable", 200)?
        .context("known execution")?;
    assert_eq!(unknown.process_status.as_str(), "unknown");
    assert_eq!(unknown.exit_code, None);
    catalog.record(&binding, capture("replacement", "new", 201, None))?;
    assert!(catalog.get(&binding, "replacement", 202)?.is_some());
    Ok(())
}

#[test]
fn execution_history_repeated_rolling_output_requires_resync() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("rolling", temp.path().to_path_buf());
    let mut first = capture("run", "same\nsame\n", 100, None);
    first.output_truncated = true;
    catalog.record(&binding, first)?;
    let cursor = catalog.read(&binding, "run", OutputSource::Output, None, 5, 101)?.next_cursor;
    let mut rolled = capture("run", "same\nsame\n", 200, None);
    rolled.output_truncated = true;
    catalog.record(&binding, rolled)?;
    let page = catalog.read(&binding, "run", OutputSource::Output, Some(&cursor), 5, 201)?;
    assert!(page.resync_required, "identical rolling tails cannot prove byte continuity");
    assert_eq!(page.text, "");
    Ok(())
}

#[test]
fn execution_history_wait_reports_expired_targets_and_rejects_foreign_cursors() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let limits = HistoryLimits { retention_ms: 50, ..HistoryLimits::default() };
    let catalog = ExecutionCatalog::new(temp.path().join("history"), limits);
    let binding = scope("wait-owner", temp.path().to_path_buf());
    let other = scope("wait-other", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "", 100, Some(0)))?;
    let baseline = catalog.list(&binding, 100)?;
    let expired = catalog.changes(&binding, &baseline.cursor, &["run".to_string()], 151)?;
    assert!(expired.resync_required);
    assert_eq!(expired.missing_ids, vec!["run"]);
    assert!(catalog.changes(&other, &baseline.cursor, &[], 151).is_err());
    assert!(catalog.changes(&binding, "malformed-cursor", &[], 151).is_err());
    Ok(())
}

#[test]
fn execution_history_rejects_conflicting_terminal_exit_codes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("terminal-authority", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "failure", 100, Some(7)))?;
    assert!(catalog.record(&binding, capture("run", "success?", 200, Some(0))).is_err());
    assert_eq!(catalog.get(&binding, "run", 201)?.context("retained record")?.exit_code, Some(7));
    Ok(())
}

#[test]
fn execution_history_session_budget_counts_screens_and_preserves_metadata() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let limits = HistoryLimits { max_session_output_bytes: 8, ..HistoryLimits::default() };
    let catalog = ExecutionCatalog::new(temp.path().join("history"), limits);
    let binding = scope("screen-budget", temp.path().to_path_buf());
    let mut first = capture("first", "1234", 100, Some(2));
    first.screen = Some("abcdefgh".into());
    let record = catalog.record(&binding, first)?;
    assert!(record.retained_bytes + record.screen_retained_bytes <= 8);
    assert_eq!(record.exit_code, Some(2));
    let mut second = capture("second", "5678", 200, None);
    second.screen = Some("ijklmnop".into());
    catalog.record(&binding, second)?;
    let snapshot = catalog.list(&binding, 201)?;
    let bytes: usize = snapshot
        .executions
        .iter()
        .map(|record| record.retained_bytes + record.screen_retained_bytes)
        .sum();
    assert!(bytes <= 8);
    assert_eq!(snapshot.executions.len(), 2, "budget eviction must preserve exit metadata");
    Ok(())
}

#[test]
fn terminal_snapshots_preserve_newer_output_when_exit_observations_arrive_out_of_order(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("terminal-snapshots", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "complete terminal output", 200, Some(7)))?;
    catalog.record(&binding, capture("run", "old partial output", 100, Some(7)))?;
    let record = catalog.get(&binding, "run", 201)?.context("terminal record")?;
    assert_eq!(record.exit_code, Some(7));
    assert_eq!(record.activity_seq, Some(200));
    assert_eq!(
        catalog.read(&binding, "run", OutputSource::Output, None, 1024, 201)?.text,
        "complete terminal output"
    );
    Ok(())
}

#[test]
fn first_exit_remains_authoritative_without_replacing_newer_running_output() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("first-exit", temp.path().to_path_buf());
    catalog.record(&binding, capture("run", "newer output", 200, None))?;
    catalog.record(&binding, capture("run", "older output", 100, Some(7)))?;
    let record = catalog.get(&binding, "run", 201)?.context("terminal record")?;
    assert_eq!(record.process_status.as_str(), "exited");
    assert_eq!(record.exit_code, Some(7));
    assert_eq!(record.activity_seq, Some(200));
    assert_eq!(
        catalog.read(&binding, "run", OutputSource::Output, None, 1024, 201)?.text,
        "newer output"
    );
    assert_eq!(serde_json::to_value(&record)?["output_pending"], true);
    Ok(())
}

#[test]
fn metadata_only_observations_keep_output_pending_until_a_full_snapshot_arrives(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let binding = scope("pending-output", temp.path().to_path_buf());
    for exit in [None, Some(9)] {
        let id = if exit.is_some() { "terminal" } else { "running" };
        catalog.record(&binding, capture(id, "old", 100, None))?;
        let mut metadata = capture(id, "", 200, exit);
        metadata.output = CapturedOutput::None;
        let pending = catalog.record(&binding, metadata)?;
        let serialized = serde_json::to_value(&pending)?;
        assert_eq!(serialized["output_pending"], true);
        assert_eq!(serialized["output_activity_seq"], 100);
        assert_eq!(pending.activity_seq, Some(200));
        let complete = catalog.record(&binding, capture(id, "old plus final output", 200, exit))?;
        let serialized = serde_json::to_value(&complete)?;
        assert_eq!(serialized["output_pending"], false);
        assert_eq!(serialized["output_activity_seq"], 200);
        assert_eq!(
            catalog.read(&binding, id, OutputSource::Output, None, 1024, 201)?.text,
            "old plus final output"
        );
    }
    Ok(())
}

#[test]
fn history_lock_contention_has_a_finite_deadline() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("history");
    let catalog = ExecutionCatalog::new(root.clone(), HistoryLimits::default());
    let binding = scope("lock-deadline", temp.path().to_path_buf());
    catalog.list(&binding, 100)?;
    let directory = std::fs::read_dir(&root)?.next().context("scope directory")??.path();
    let lock =
        std::fs::OpenOptions::new().read(true).write(true).open(directory.join("history.lock"))?;
    lock.lock()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = catalog.list(&binding, 101);
        let _ = sender.send((result, started.elapsed()));
    });
    // Always release and join, including on the pre-fix failure, so RED cannot hang.
    let completion = receiver.recv_timeout(std::time::Duration::from_secs(2));
    drop(lock);
    worker.join().map_err(|_| anyhow::anyhow!("catalog worker did not finish"))?;
    let (result, elapsed) = completion.context("catalog must stop waiting for the lock")?;
    assert!(result.is_err(), "a held lock cannot produce a fresh catalog snapshot");
    assert!(elapsed < std::time::Duration::from_secs(1), "lock wait took {elapsed:?}");
    Ok(())
}

#[test]
fn execution_history_updates_require_existing_exact_identity_and_do_not_recreate_expired_records(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("updates-only", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(
        temp.path().join("history"),
        HistoryLimits { retention_ms: 50, ..HistoryLimits::default() },
    );
    let mut accepted = capture("run", "original", 100, Some(0));
    accepted.execution_token = Some(winx_code_agent::runtime::ShellExecutionToken {
        guardian_epoch: "guardian".into(),
        session_epoch: "session".into(),
        generation: 1,
    });
    assert!(catalog.update_existing(&binding, accepted.clone())?.is_none());
    catalog.record(&binding, accepted.clone())?;
    let mut wrong = accepted.clone();
    wrong.execution_token.as_mut().context("token")?.session_epoch = "foreign".into();
    assert!(matches!(
        catalog.update_existing(&binding, wrong),
        Err(winx_code_agent::state::execution_history::HistoryError::IdentityConflict)
    ));
    let updated =
        catalog.update_existing(&binding, accepted.clone())?.context("existing record")?;
    assert_eq!(updated.exit_code, Some(0));
    accepted.observed_at_unix_ms = 151;
    assert!(catalog.update_existing(&binding, accepted)?.is_none());
    assert_eq!(catalog.list(&binding, 151)?.executions.len(), 0);
    Ok(())
}
