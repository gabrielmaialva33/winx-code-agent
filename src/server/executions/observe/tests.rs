use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::server::usage::lifecycle::{Phase, UsageTrace};
use crate::state::execution_history::{ExecutionScope, ObservationItem};

#[tokio::test]
async fn observe_deadline_bounds_catalog_lock_acquisition() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("history");
    let mut service = WinxService::new();
    service.execution_history = Arc::new(super::super::history::tests::history_at(root.clone()));
    let scope = ExecutionScope {
        thread_id: "observe-catalog-deadline".into(),
        workspace_root: temp.path().canonicalize()?,
    };
    service.execution_history.catalog()?.list(&scope, now_ms())?;
    let directory = std::fs::read_dir(&root)?
        .next()
        .ok_or_else(|| anyhow::anyhow!("catalog scope directory missing"))??
        .path();
    let lock =
        std::fs::OpenOptions::new().read(true).write(true).open(directory.join("history.lock"))?;
    lock.lock()?;
    let (slot, session_guard) = service.tool_session_for(&scope.thread_id).await;
    let context =
        WorkflowContext { slot, scope, allow_prompt: false, _session_guard: session_guard };
    let trace = UsageTrace::for_test("Execution", "observe");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        trace.scope(service.observe_executions(
            &context,
            vec![ObservationRequest {
                execution_id: "watched".into(),
                source: OutputSource::Output,
                cursor: None,
                page_bytes: 2048,
            }],
            Some(0.04),
            None,
        )),
    )
    .await;
    let elapsed = started.elapsed();
    drop(lock);
    let result = result?;
    assert!(matches!(result, Err(HistoryError::Io(ref error))
        if error.kind() == std::io::ErrorKind::TimedOut));
    assert!(elapsed < Duration::from_millis(250), "{elapsed:?}");
    assert_eq!(trace.test_calls(Phase::WaitIdle), None);
    Ok(())
}

#[test]
fn observe_preserves_both_refresh_and_read_errors() -> anyhow::Result<()> {
    let snapshot = CatalogObservation {
        sequence: 7,
        cursor: "retained-catalog-cursor".into(),
        items: vec![ObservationItem {
            execution_id: "execution".into(),
            source: OutputSource::Output,
            record: None,
            page: Err(HistoryError::NotFound),
        }],
    };
    let errors = HashMap::from([(
        "execution".into(),
        ObservationError::new(ObservationStage::Refresh, &HistoryError::IdentityConflict),
    )]);
    let reply = observation_reply(snapshot, &errors);
    assert_eq!(reply.status, ToolResultStatus::CompletedWithIssues);
    assert_eq!(reply.error_code, Some("execution_observation_issues"));
    let item = &reply.data["items"][0];
    assert_eq!(item["status"], "conflict");
    assert_eq!(item["errors"][0]["stage"], "refresh");
    assert_eq!(item["errors"][0]["code"], "execution_identity_conflict");
    assert_eq!(item["errors"][1]["stage"], "read");
    assert_eq!(item["errors"][1]["code"], "execution_not_found");
    assert_eq!(reply.data["items_with_issues"], 1);
    let wire = super::super::workflow_result("Execution", reply)?;
    assert_eq!(wire.is_error, Some(false));
    assert_eq!(wire.content, Vec::<rmcp::model::ContentBlock>::new());
    Ok(())
}

#[test]
fn observe_discovery_preserves_authority_and_exposes_bounded_items() -> anyhow::Result<()> {
    fn find_observe(value: &serde_json::Value) -> Option<&serde_json::Value> {
        if value["properties"]["type"]["const"] == "observe"
            || value["properties"]["type"]["enum"] == json!(["observe"])
        {
            return Some(value);
        }
        match value {
            serde_json::Value::Object(map) => map.values().find_map(find_observe),
            serde_json::Value::Array(items) => items.iter().find_map(find_observe),
            _ => None,
        }
    }

    let tool = crate::server::catalog::winx_tools()
        .into_iter()
        .find(|tool| tool.name == "Execution")
        .ok_or_else(|| anyhow::anyhow!("Execution is not discoverable"))?;
    let wire = serde_json::to_value(tool)?;
    assert_eq!(wire["annotations"]["readOnlyHint"], true);
    assert_eq!(wire["annotations"]["openWorldHint"], false);
    let required = wire["inputSchema"]["required"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing required scope fields"))?;
    assert!(required.iter().any(|key| key == "thread_id"));
    assert!(required.iter().any(|key| key == "workspace_root"));

    let action = find_observe(&wire["inputSchema"])
        .ok_or_else(|| anyhow::anyhow!("observe action missing from schema: {wire}"))?;
    assert_eq!(action["properties"]["items"]["minItems"], 1);
    assert_eq!(action["properties"]["items"]["maxItems"], 32);
    Ok(())
}
