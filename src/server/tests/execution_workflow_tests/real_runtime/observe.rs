use super::*;
use crate::server::usage::lifecycle::{Phase, UsageTrace};
use std::time::{Duration, Instant};

fn observe(items: Value) -> Value {
    let mut request = json!({"action": {"type": "observe"}});
    request["action"]["items"] = items;
    request
}

#[tokio::test]
async fn observe_combines_exits_and_interoperates_with_read_cursors() -> anyhow::Result<()> {
    let session = Session::new().await?;
    let left = session.command("sh -c 'printf retained-left; exit 7'", false).await?;
    let right = session.command("sh -c 'printf retained-right; exit 9'", false).await?;
    let left_id = left["data"]["execution_id"].as_str().context("left ID")?;
    let right_id = right["data"]["execution_id"].as_str().context("right ID")?;
    let first = session
        .call(
            "Execution",
            json!({"action": {
                "type": "read", "execution_id": left_id, "page_bytes": 4
            }}),
        )
        .await?;
    let cursor = first["data"]["page"]["next_cursor"].as_str().context("read cursor")?;
    let expected = session
        .call(
            "Execution",
            json!({"action": {
                "type": "read", "execution_id": left_id, "cursor": cursor, "page_bytes": 4
            }}),
        )
        .await?;
    let result = session
        .call(
            "Execution",
            observe(json!([
                {"execution_id": right_id, "page_bytes": 4096},
                {"execution_id": left_id, "cursor": cursor, "page_bytes": 4}
            ])),
        )
        .await?;
    assert_eq!(result["status"], "completed", "{result}");
    assert!(result.get("output").is_none(), "{result}");
    let items = result["data"]["items"].as_array().context("observation items")?;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["execution_id"], right_id);
    assert_eq!(items[0]["execution"]["exit_code"], 9);
    assert_eq!(items[1]["execution"]["exit_code"], 7);
    assert_eq!(items[1]["page"], expected["data"]["page"]);
    assert!(items[0]["page"]["text"].as_str().context("right page")?.contains("retained-right"));
    for item in items {
        assert_eq!(item["status"], "completed");
        assert_eq!(item["execution"]["process_status"], "exited");
        assert!(item["execution"].get("execution_token").is_none());
        assert!(item["execution"].get("command_generation").is_none());
        assert!(item["execution"].get("label").is_none());
        assert!(item["execution"].get("cwd").is_none());
        assert_eq!(item["errors"], json!([]));
    }
    let observed_cursor = items[1]["page"]["next_cursor"].clone();
    let continued = session
        .call(
            "Execution",
            json!({"action": {
                "type": "read", "execution_id": left_id, "cursor": observed_cursor, "page_bytes": 4
            }}),
        )
        .await?;
    let repeated = session
        .call(
            "Execution",
            observe(json!([
                {"execution_id": left_id, "cursor": observed_cursor, "page_bytes": 4}
            ])),
        )
        .await?;
    assert_eq!(repeated["data"]["items"][0]["page"], continued["data"]["page"]);
    let waited = session
        .call(
            "Execution",
            json!({"action": {
                "type": "wait", "execution_ids": [left_id, right_id],
                "after": result["data"]["catalog_cursor"], "timeout_seconds": 0.0
            }}),
        )
        .await?;
    assert_eq!(waited["status"], "completed", "{waited}");
    session.close().await
}

#[tokio::test]
async fn observe_partial_failures_preserve_siblings_and_metadata() -> anyhow::Result<()> {
    let session = Session::new().await?;
    let command = session.command("printf partial-output", false).await?;
    let id = command["data"]["execution_id"].as_str().context("execution ID")?;
    let result = session
        .call(
            "Execution",
            observe(json!([
                {"execution_id": id},
                {"execution_id": "missing-execution"},
                {"execution_id": id, "source": "screen", "cursor": "invalid-cursor"}
            ])),
        )
        .await?;
    assert_eq!(result["status"], "completed_with_issues", "{result}");
    assert_eq!(result["errorCode"], "execution_observation_issues");
    let items = result["data"]["items"].as_array().context("items")?;
    assert_eq!(items[0]["status"], "completed");
    assert!(items[0]["page"]["text"].as_str().context("valid output")?.contains("partial-output"));
    assert_eq!(items[1]["errors"][0]["code"], "execution_not_found");
    assert!(items[1]["execution"].is_null());
    assert_eq!(items[2]["errors"][0]["code"], "invalid_execution_cursor");
    assert_eq!(items[2]["execution"]["execution_id"], id);
    assert!(items[2]["page"].is_null());
    assert!(result["data"]["catalog_cursor"].is_string());
    session.close().await
}

#[tokio::test]
async fn observe_invalid_batches_are_rejected_before_session_admission() -> anyhow::Result<()> {
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let thread = format!("observe_validation_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    let held = slot.lock().await;
    let mut invalid = vec![
        json!([]),
        json!([{"execution_id": ""}]),
        json!([{"execution_id": "bad\0id"}]),
        json!([{"execution_id": "a".repeat(513)}]),
        json!([{"execution_id": "a", "page_bytes": 0}]),
        json!([{"execution_id": "a", "page_bytes": 3}]),
        json!([{"execution_id": "a", "page_bytes": 65537}]),
        json!([{"execution_id": "a"}, {"execution_id": "a", "source": "output"}]),
    ];
    invalid.push(json!((0..33)
        .map(|i| json!({"execution_id": format!("id-{i}")}))
        .collect::<Vec<_>>()));
    let mut overflowing =
        (0..31).map(|i| json!({"execution_id": format!("id-{i}")})).collect::<Vec<_>>();
    overflowing.push(json!({"execution_id": "last", "page_bytes": 2049}));
    invalid.push(json!(overflowing));
    for items in invalid {
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": "Execution",
            "arguments": {"thread_id": thread, "workspace_root": directory.path(),
                "action": {"type": "observe", "items": items}}
        }))?;
        let trace = UsageTrace::for_test("Execution", "observe");
        let result = Box::pin(tokio::time::timeout(
            Duration::from_millis(200),
            trace.scope(service.execute_tool_call(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
            )),
        ))
        .await??;
        let envelope = result.result.structured_content.context("invalid envelope")?;
        assert_eq!(envelope["status"], "invalid_input", "{envelope}");
        assert_eq!(trace.test_calls(Phase::SessionAdmission), None);
        assert_eq!(trace.test_calls(Phase::Catalog), None);
        assert_eq!(trace.test_calls(Phase::RuntimeInspect), None);
    }
    drop(held);
    Ok(())
}

#[tokio::test]
async fn observe_deadline_includes_session_admission_without_lifecycle_wait() -> anyhow::Result<()>
{
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let thread = format!("observe_deadline_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    for (timeout, maximum_ms) in [(0.0_f32, 550), (0.04_f32, 250)] {
        let held = slot.lock().await;
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": "Execution",
            "arguments": {"thread_id": thread, "workspace_root": directory.path(),
                "action": {"type": "observe", "items": [{"execution_id": "watched"}],
                    "timeout_seconds": timeout}}
        }))?;
        let trace = UsageTrace::for_test("Execution", "observe");
        let started = Instant::now();
        let result = Box::pin(tokio::time::timeout(
            Duration::from_secs(1),
            trace.scope(service.execute_tool_call(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
            )),
        ))
        .await;
        let elapsed = started.elapsed();
        drop(held);
        let result = result??;
        let envelope = result.result.structured_content.context("deadline result")?;
        assert_eq!(envelope["status"], "failed", "{envelope}");
        assert!(envelope["data"].get("catalog_cursor").is_none());
        assert_eq!(trace.test_calls(Phase::SessionAdmission), Some(1));
        assert_eq!(trace.test_calls(Phase::WaitIdle), None);
        assert!(elapsed < Duration::from_millis(maximum_ms), "{elapsed:?}");
    }
    Ok(())
}

#[tokio::test]
async fn observe_preserves_foreground_and_background_legacy_delivery() -> anyhow::Result<()> {
    let session = Session::new().await?;
    for background in [false, true] {
        let command = session.call("BashCommand", json!({
            "action_json": {"type": "command", "command": "sleep 0.15; printf late-observe-output",
                "is_background": background, "allow_multi": true},
            "wait_for_seconds": 0.0
        })).await?;
        let id = command["data"]["execution_id"].as_str().context("execution ID")?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let result = session.call("Execution", observe(json!([{"execution_id": id}]))).await?;
        assert_eq!(result["status"], "completed", "{result}");
        assert!(result["data"]["items"][0]["page"]["text"]
            .as_str()
            .context("observed output")?
            .contains("late-observe-output"));
        let legacy = session
            .call(
                "BashCommand",
                json!({
                    "action_json": {"type": "status_check", "status_check": true,
                        "bg_command_id": command["data"]["bg_command_id"]},
                    "wait_for_seconds": 0.0
                }),
            )
            .await?;
        assert!(
            legacy["output"].as_str().context("legacy output")?.contains("late-observe-output"),
            "{legacy}"
        );
    }
    session.close().await
}

#[tokio::test]
async fn observe_refreshes_one_identity_once_for_multiple_sources() -> anyhow::Result<()> {
    let session = Session::new().await?;
    let command = session
        .call(
            "BashCommand",
            json!({
                "action_json": {"type": "command", "command": "printf ready; sleep 10",
                    "is_background": true, "allow_multi": true},
                "wait_for_seconds": 0.0
            }),
        )
        .await?;
    let id = command["data"]["execution_id"].as_str().context("execution ID")?;
    let trace = UsageTrace::for_test("Execution", "observe");
    let result = Box::pin(trace.scope(session.call(
        "Execution",
        observe(json!([
            {"execution_id": id, "source": "output"},
            {"execution_id": id, "source": "screen"},
            {"execution_id": id, "source": "detection"}
        ])),
    )))
    .await?;
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(trace.test_calls(Phase::RuntimeInspect), Some(1));
    assert!(trace.test_calls(Phase::RuntimeCapture).unwrap_or_default() <= 1);
    assert_eq!(trace.test_calls(Phase::WaitIdle), None);
    let items = result["data"]["items"].as_array().context("items")?;
    assert_eq!(items[0]["execution"], items[1]["execution"]);
    assert_eq!(items[1]["execution"], items[2]["execution"]);
    assert_eq!(items[0]["execution"]["process_status"], "running");
    assert!(items[0]["execution"]["exit_code"].is_null());
    session.close().await
}

#[tokio::test]
async fn observe_dispatch_redacts_structured_pages_without_duplicating_text() -> anyhow::Result<()>
{
    let session = Session::new().await?;
    let canary = format!("sk-{}", "z".repeat(36));
    let command =
        session.command(&format!("printf 'visible-start {canary} visible-end'"), false).await?;
    let id = command["data"]["execution_id"].as_str().context("execution ID")?;
    let request: CallToolRequestParams = serde_json::from_value(json!({
        "name": "Execution",
        "arguments": {"thread_id": session.thread, "workspace_root": session.root,
            "action": {"type": "observe", "items": [
                {"execution_id": id}, {"execution_id": canary}
            ]}}
    }))?;
    let call = session
        .service
        .execute_tool_call(request, ShellActionOptions::default(), EditPermissionSet::default())
        .await?;
    assert!(call.result.content.is_empty(), "observation must not duplicate page text");
    assert_eq!(call.result.is_error, Some(false));
    let envelope = call.result.structured_content.context("result envelope")?;
    assert_eq!(envelope["status"], "completed_with_issues");
    assert!(envelope.get("output").is_none());
    assert!(!envelope.to_string().contains(&canary));
    let page = envelope["data"]["items"][0]["page"]["text"].as_str().context("page")?;
    assert!(page.contains("[REDACTED:openai-key]"), "{page}");
    assert!(page.contains("visible-start"));
    assert!(page.contains("visible-end"));
    session.close().await
}

#[tokio::test]
async fn observe_rejects_invalid_deadlines_before_session_admission() -> anyhow::Result<()> {
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let thread = format!("observe_timeout_validation_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    let held = slot.lock().await;
    for timeout in [-1.0, 30.01] {
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": "Execution",
            "arguments": {"thread_id": thread, "workspace_root": directory.path(),
                "action": {"type": "observe", "items": [{"execution_id": "id"}],
                    "timeout_seconds": timeout}}
        }))?;
        let trace = UsageTrace::for_test("Execution", "observe");
        let result = Box::pin(tokio::time::timeout(
            Duration::from_millis(200),
            trace.scope(service.execute_tool_call(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
            )),
        ))
        .await??;
        let envelope = result.result.structured_content.context("result")?;
        assert_eq!(envelope["status"], "invalid_input", "{envelope}");
        assert_eq!(trace.test_calls(Phase::SessionAdmission), None);
    }
    drop(held);
    Ok(())
}

#[tokio::test]
async fn observe_rejects_a_foreign_workspace() -> anyhow::Result<()> {
    let session = Session::new().await?;
    let command = session.command("printf workspace-private", false).await?;
    let other = tempfile::tempdir()?;
    let request: CallToolRequestParams = serde_json::from_value(json!({
        "name": "Execution",
        "arguments": {"thread_id": session.thread, "workspace_root": other.path(),
            "action": {"type": "observe", "items": [
                {"execution_id": command["data"]["execution_id"]}
            ]}}
    }))?;
    let call = session
        .service
        .execute_tool_call(request, ShellActionOptions::default(), EditPermissionSet::default())
        .await?;
    let envelope = call.result.structured_content.context("result envelope")?;
    assert_ne!(envelope["status"], "completed", "{envelope}");
    assert!(envelope["data"].get("items").is_none(), "{envelope}");
    assert!(!envelope.to_string().contains("workspace-private"));
    session.close().await
}
