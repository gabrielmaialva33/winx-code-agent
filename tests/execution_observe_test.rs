use std::path::{Path, PathBuf};

use anyhow::Context;

use winx_code_agent::state::execution_history::{
    validate_observation_requests, CapturedOutput, ExecutionCapture, ExecutionCatalog,
    ExecutionScope, HistoryError, HistoryLimits, ObservationItem, ObservationRequest, OutputPage,
    OutputSource,
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
        label: Some("observe fixture".to_string()),
        output_truncated: false,
        dropped_output_file: None,
    }
}

fn request(id: &str, source: OutputSource, page_bytes: usize) -> ObservationRequest {
    ObservationRequest { execution_id: id.to_string(), source, cursor: None, page_bytes }
}

fn page(item: &ObservationItem) -> anyhow::Result<&OutputPage> {
    match &item.page {
        Ok(page) => Ok(page),
        Err(error) => anyhow::bail!("expected an output page: {error}"),
    }
}

#[test]
fn observe_pages_are_utf8_safe_interoperable_and_non_consuming() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-utf8", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    catalog.record(&binding, capture("run", "aé🙂z\n", 100, Some(7)))?;
    let initial = request("run", OutputSource::Output, 4);

    let first = catalog.observe(&binding, std::slice::from_ref(&initial), 101)?;
    let first_page = page(&first.items[0])?;
    assert_eq!(first_page.text, "aé");
    assert!(first_page.has_more);
    assert_eq!(first.items[0].record.as_ref().context("metadata")?.exit_code, Some(7));
    let legacy = catalog.read(
        &binding,
        "run",
        OutputSource::Output,
        Some(&first_page.next_cursor),
        4,
        102,
    )?;
    assert_eq!(legacy.text, "🙂");
    let continued = ObservationRequest { cursor: Some(legacy.next_cursor), ..initial.clone() };
    let last = catalog.observe(&binding, &[continued], 103)?;
    assert_eq!(page(&last.items[0])?.text, "z\n");
    assert!(!page(&last.items[0])?.has_more);

    let repeated = catalog.observe(&binding, &[initial], 104)?;
    assert_eq!(page(&repeated.items[0])?.text, "aé");
    assert_eq!(catalog.read(&binding, "run", OutputSource::Output, None, 4, 105)?.text, "aé");
    Ok(())
}

#[test]
fn observe_same_execution_sources_keep_independent_cursors_and_request_order() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-sources", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let mut accepted = capture("run", "stdout-tail", 100, None);
    accepted.screen = Some("screen-tail".to_string());
    catalog.record(&binding, accepted)?;
    let initial = catalog.observe(
        &binding,
        &[
            request("run", OutputSource::Screen, 4),
            request("run", OutputSource::Output, 4),
            request("run", OutputSource::Detection, 4),
        ],
        101,
    )?;
    assert_eq!(initial.items.len(), 3);
    assert_eq!(initial.items[0].source, OutputSource::Screen);
    assert_eq!(initial.items[1].source, OutputSource::Output);
    assert_eq!(initial.items[2].source, OutputSource::Detection);
    assert_eq!(page(&initial.items[0])?.text, "scre");
    assert_eq!(page(&initial.items[1])?.text, "stdo");
    assert_eq!(page(&initial.items[2])?.text, "scre");

    let mut output = request("run", OutputSource::Output, 4);
    output.cursor = Some(page(&initial.items[1])?.next_cursor.clone());
    let mut screen = request("run", OutputSource::Screen, 4);
    screen.cursor = Some(page(&initial.items[0])?.next_cursor.clone());
    let continued = catalog.observe(&binding, &[output, screen], 102)?;
    assert_eq!(page(&continued.items[0])?.text, "ut-t");
    assert_eq!(page(&continued.items[1])?.text, "en-t");

    let mut wrong_source = request("run", OutputSource::Detection, 4);
    wrong_source.cursor = Some(page(&initial.items[0])?.next_cursor.clone());
    let rejected = catalog.observe(&binding, &[wrong_source], 103)?;
    assert!(rejected.items[0].record.is_some());
    assert!(matches!(&rejected.items[0].page, Err(HistoryError::InvalidCursor)));
    Ok(())
}

#[test]
fn observe_item_errors_preserve_metadata_and_successful_siblings() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-partial", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    catalog.record(&binding, capture("bad-cursor", "retained", 100, Some(9)))?;
    catalog.record(&binding, capture("healthy", "healthy output", 100, Some(0)))?;
    let mut invalid = request("bad-cursor", OutputSource::Output, 64);
    invalid.cursor = Some("malformed-cursor".to_string());
    let observed = catalog.observe(
        &binding,
        &[
            invalid,
            request("missing", OutputSource::Output, 64),
            request("healthy", OutputSource::Output, 64),
        ],
        101,
    )?;
    assert_eq!(observed.items.len(), 3);
    assert_eq!(observed.items[0].execution_id, "bad-cursor");
    assert_eq!(
        observed.items[0].record.as_ref().context("failed page metadata")?.exit_code,
        Some(9)
    );
    assert!(matches!(&observed.items[0].page, Err(HistoryError::InvalidCursor)));
    assert_eq!(observed.items[1].execution_id, "missing");
    assert!(observed.items[1].record.is_none());
    assert!(matches!(&observed.items[1].page, Err(HistoryError::NotFound)));
    assert_eq!(observed.items[2].execution_id, "healthy");
    assert_eq!(page(&observed.items[2])?.text, "healthy output");
    Ok(())
}

// Locate a fixture payload by its contents, without depending on private filename hashes.
fn remove_payload_with_output(root: &Path, output: &str) -> anyhow::Result<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
                continue;
            }
            let bytes = std::fs::read(entry.path())?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
            if value.get("output").and_then(serde_json::Value::as_str) == Some(output) {
                std::fs::remove_file(entry.path())?;
                return Ok(());
            }
        }
    }
    anyhow::bail!("fixture output payload was not found")
}

#[test]
fn observe_payload_failure_is_local_and_does_not_erase_metadata() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("history");
    let binding = scope("observe-payload-failure", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(root.clone(), HistoryLimits::default());
    catalog.record(&binding, capture("broken", "unique-broken-payload", 100, Some(13)))?;
    catalog.record(&binding, capture("healthy", "healthy payload", 100, Some(0)))?;
    remove_payload_with_output(&root, "unique-broken-payload")?;

    let observed = catalog.observe(
        &binding,
        &[
            request("broken", OutputSource::Output, 64),
            request("healthy", OutputSource::Output, 64),
        ],
        101,
    )?;
    assert_eq!(observed.items[0].record.as_ref().context("retained metadata")?.exit_code, Some(13));
    assert!(matches!(&observed.items[0].page, Err(HistoryError::Io(_))));
    assert_eq!(page(&observed.items[1])?.text, "healthy payload");
    Ok(())
}

#[test]
fn observe_keeps_scoped_metadata_and_rejects_foreign_cursors() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let left = scope("p_left__chat", temp.path().join("left"));
    let right = scope("p_right__chat", temp.path().join("left"));
    let another_workspace = scope("p_left__chat", temp.path().join("right"));
    catalog.record(&left, capture("shared-id", "left-only", 100, Some(7)))?;
    catalog.record(&right, capture("shared-id", "right-only", 100, Some(9)))?;
    let initial = request("shared-id", OutputSource::Output, 4);
    let left_observation = catalog.observe(&left, std::slice::from_ref(&initial), 101)?;
    let foreign = ObservationRequest {
        cursor: Some(page(&left_observation.items[0])?.next_cursor.clone()),
        ..initial.clone()
    };
    let right_observation = catalog.observe(&right, &[foreign], 101)?;
    assert_eq!(
        right_observation.items[0].record.as_ref().context("own metadata")?.exit_code,
        Some(9)
    );
    assert!(matches!(&right_observation.items[0].page, Err(HistoryError::InvalidCursor)));
    assert!(matches!(
        catalog.changes(&right, &left_observation.cursor, &[], 101),
        Err(HistoryError::InvalidCursor)
    ));
    let missing = catalog.observe(&another_workspace, &[initial], 101)?;
    assert!(missing.items[0].record.is_none());
    assert!(matches!(&missing.items[0].page, Err(HistoryError::NotFound)));
    Ok(())
}

#[test]
fn observe_preserves_output_gaps_and_changed_screen_resynchronization() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-retention", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(
        temp.path().join("history"),
        HistoryLimits { max_output_bytes: 8, ..HistoryLimits::default() },
    );
    let mut first = capture("run", "12345678", 100, None);
    first.screen = Some("before".to_string());
    catalog.record(&binding, first)?;
    let initial = catalog.observe(
        &binding,
        &[request("run", OutputSource::Output, 4), request("run", OutputSource::Screen, 4)],
        101,
    )?;
    let mut changed = capture("run", "1234567890123456", 200, None);
    changed.screen = Some("after".to_string());
    catalog.record(&binding, changed)?;
    let mut output = request("run", OutputSource::Output, 4);
    output.cursor = Some(page(&initial.items[0])?.next_cursor.clone());
    let mut screen = request("run", OutputSource::Screen, 4);
    screen.cursor = Some(page(&initial.items[1])?.next_cursor.clone());
    let observed = catalog.observe(&binding, &[output, screen], 201)?;
    let gap = page(&observed.items[0])?;
    assert!(gap.gap);
    assert!(gap.resync_required);
    assert_eq!(gap.text, "");
    assert_eq!(gap.retained_start, 8);
    assert_eq!(observed.items[0].record.as_ref().context("metadata")?.retained_start, 8);
    let screen = page(&observed.items[1])?;
    assert!(screen.resync_required);
    assert!(!screen.gap);
    assert_eq!(screen.text, "");
    let mut reset = request("run", OutputSource::Output, 4);
    reset.cursor = Some(gap.next_cursor.clone());
    let resumed = catalog.observe(&binding, &[reset], 202)?;
    assert_eq!(page(&resumed.items[0])?.text, "9012");
    assert!(!page(&resumed.items[0])?.resync_required);
    Ok(())
}

#[test]
fn observe_expires_terminal_metadata_without_dropping_active_siblings() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-expiration", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(
        temp.path().join("history"),
        HistoryLimits { retention_ms: 50, ..HistoryLimits::default() },
    );
    catalog.record(&binding, capture("active", "waiting", 100, None))?;
    catalog.record(&binding, capture("done", "failed", 100, Some(11)))?;
    let requests =
        [request("done", OutputSource::Output, 64), request("active", OutputSource::Output, 64)];
    let initial = catalog.observe(&binding, &requests, 101)?;
    let observed = catalog.observe(&binding, &requests, 151)?;
    assert!(observed.items[0].record.is_none());
    assert!(matches!(&observed.items[0].page, Err(HistoryError::NotFound)));
    let active = observed.items[1].record.as_ref().context("active metadata")?;
    assert_eq!(active.process_status.as_str(), "running");
    assert_eq!(active.exit_code, None);
    assert_eq!(page(&observed.items[1])?.text, "waiting");
    let changes = catalog.changes(&binding, &initial.cursor, &["done".to_string()], 151)?;
    assert!(changes.resync_required);
    assert_eq!(changes.missing_ids, vec!["done"]);
    let retained = catalog.list(&binding, 151)?;
    assert_eq!(retained.executions.len(), 1);
    assert_eq!(retained.executions[0].execution_id, "active");
    Ok(())
}

#[test]
fn observe_event_cursor_is_a_change_baseline_and_reads_do_not_advance_it() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-changes", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    catalog.record(&binding, capture("one", "a", 100, None))?;
    catalog.record(&binding, capture("two", "b", 100, None))?;
    let requests =
        [request("two", OutputSource::Output, 4), request("one", OutputSource::Output, 4)];
    let baseline = catalog.observe(&binding, &requests, 101)?;
    let ids = ["one".to_string(), "two".to_string()];
    assert_eq!(
        catalog.changes(&binding, &baseline.cursor, &ids, 102)?.executions,
        Vec::<winx_code_agent::state::execution_history::ExecutionRecord>::new()
    );
    let repeated = catalog.observe(&binding, &requests, 103)?;
    assert_eq!(repeated.sequence, baseline.sequence);
    assert_eq!(repeated.cursor, baseline.cursor);
    assert_eq!(repeated.items[0].record.as_ref().context("metadata")?.observed_at_unix_ms, 100);
    catalog.record(&binding, capture("one", "a!", 200, Some(17)))?;
    let changed = catalog.changes(&binding, &baseline.cursor, &ids, 201)?;
    assert_eq!(changed.executions.len(), 1);
    assert_eq!(changed.executions[0].execution_id, "one");
    assert_eq!(changed.executions[0].exit_code, Some(17));
    let current = catalog.observe(&binding, &requests, 202)?;
    assert!(current.sequence > baseline.sequence);
    assert_eq!(
        catalog.changes(&binding, &current.cursor, &ids, 203)?.executions,
        Vec::<winx_code_agent::state::execution_history::ExecutionRecord>::new()
    );
    Ok(())
}

#[test]
fn observe_preflight_rejects_invalid_requests_before_creating_the_catalog() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("history");
    let binding = scope("observe-admission", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(root.clone(), HistoryLimits::default());
    let duplicate = request("same", OutputSource::Output, 4);
    let mut cases = vec![
        Vec::new(),
        (0..33).map(|id| request(&format!("run-{id}"), OutputSource::Output, 4)).collect(),
        vec![request("run", OutputSource::Output, 0)],
        vec![request("run", OutputSource::Output, 3)],
        vec![request("run", OutputSource::Output, 65_537)],
        vec![request("run", OutputSource::Output, usize::MAX)],
        vec![request("one", OutputSource::Output, 65_536), request("two", OutputSource::Output, 4)],
        vec![duplicate.clone(), duplicate],
    ];
    for id in [String::new(), "x".repeat(513), "with\0nul".to_string()] {
        cases.push(vec![request(&id, OutputSource::Output, 4)]);
    }
    for requests in cases {
        assert!(matches!(
            validate_observation_requests(&requests),
            Err(HistoryError::InvalidInput(_))
        ));
        assert!(matches!(
            catalog.observe(&binding, &requests, 100),
            Err(HistoryError::InvalidInput(_))
        ));
        assert!(!root.exists(), "invalid requests must be rejected before catalog I/O");
    }
    Ok(())
}

#[test]
fn observe_defaults_fit_32_streams_in_the_aggregate_text_budget() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let binding = scope("observe-defaults", temp.path().to_path_buf());
    let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
    let requests = (0..32)
        .map(|id| {
            serde_json::from_value::<ObservationRequest>(serde_json::json!({
                "execution_id": format!("run-{id}")
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_observation_requests(&requests)?;
    for request in &requests {
        catalog
            .record(&binding, capture(&request.execution_id, &"x".repeat(3000), 100, Some(0)))?;
    }
    let observed = catalog.observe(&binding, &requests, 101)?;
    assert_eq!(observed.items.len(), 32);
    let mut bytes = 0;
    for item in &observed.items {
        let page = page(item)?;
        assert_eq!(page.source, OutputSource::Output);
        assert_eq!(page.text.len(), 2048);
        assert!(page.has_more);
        bytes += page.text.len();
    }
    assert_eq!(bytes, 65_536);
    Ok(())
}
