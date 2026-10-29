use winx_code_agent::report::usage_report;

mod observation_usage_regressions {
    use super::usage_report;
    use serde_json::{json, Value};

    fn report(fields: &[Value]) -> anyhow::Result<Value> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("usage.jsonl");
        let lines = fields
            .iter()
            .map(|fields| {
                json!({"timestamp":"2026-10-29T00:00:00Z", "target":"winx::usage", "fields":fields})
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{lines}\n"))?;
        Ok(serde_json::to_value(usage_report(Some(&path), 100, 0, None)?)?)
    }

    #[test]
    fn read_shape_counts_preserve_legacy_and_malformed_unknowns() -> anyhow::Result<()> {
        let value = report(&[
            json!({"event":"tool_call", "tool":"ReadFiles", "read_request_schema":1,
                "read_whole_file_requests":2, "read_bounded_range_requests":3,
                "read_open_ended_range_requests":1, "response_bytes":400}),
            json!({"event":"tool_call", "tool":"ReadFiles", "batch_items":0, "response_bytes":50}),
            json!({"event":"tool_call", "tool":"ReadFiles", "read_request_schema":1,
                "read_whole_file_requests":0, "read_bounded_range_requests":"0",
                "read_open_ended_range_requests":0}),
            json!({"event":"tool_call", "tool":"BashCommand", "read_request_schema":1,
                "read_whole_file_requests":99, "read_bounded_range_requests":99,
                "read_open_ended_range_requests":99}),
        ])?;
        assert_eq!(value["reads"]["calls"], 3);
        assert_eq!(value["reads"]["classifiedCalls"], 1);
        assert_eq!(value["reads"]["unknownCalls"], 2);
        assert_eq!(value["reads"]["wholeFileRequests"], 2);
        assert_eq!(value["reads"]["boundedRangeRequests"], 3);
        assert_eq!(value["reads"]["openEndedRangeRequests"], 1);
        assert_eq!(value["reads"]["responseBytes"], 450);
        Ok(())
    }

    #[test]
    fn observe_counts_requests_and_returned_items_separately_from_partial_calls(
    ) -> anyhow::Result<()> {
        let value = report(&[
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":3, "observe_returned_items":2,
                "observe_items_with_issues":1, "result_status":"completed_with_issues",
                "error_code":"execution_observation_issues"}),
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "batch_items":0, "result_status":"completed"}),
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":0, "observe_returned_items":0,
                "observe_items_with_issues":0, "result_status":"completed"}),
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":1, "outcome":"protocol_error"}),
        ])?;
        let observed = &value["observations"];
        assert_eq!(observed["calls"], 4);
        assert_eq!(observed["requestCountsKnown"], 3);
        assert_eq!(observed["requestCountsUnknown"], 1);
        assert_eq!(observed["requestedItems"], 4);
        assert_eq!(observed["resultCountsKnown"], 2);
        assert_eq!(observed["resultCountsUnknown"], 2);
        assert_eq!(observed["returnedItems"], 2);
        assert_eq!(observed["itemsWithIssues"], 1);
        assert_eq!(observed["partialResults"], 1);
        // Existing semantics: errorResults counts a nonempty error_code,
        // including a partial result; it does not mean the whole call failed.
        assert_eq!(value["tools"]["Execution"]["errorResults"], 1);
        Ok(())
    }

    #[test]
    fn observation_counts_require_execution_observe_and_recognized_schema() -> anyhow::Result<()> {
        let value = report(&[
            json!({"event":"tool_call", "tool":"Agent", "action":"observe", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":99}),
            json!({"event":"tool_call", "tool":"Execution", "action":"read", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":99}),
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "observe_schema":2, "observe_requested_items":99, "observe_returned_items":99,
                "observe_items_with_issues":0}),
            json!({"event":"tool_call", "tool":"Execution", "action":"observe", "workflow_schema":1,
                "observe_schema":1, "observe_requested_items":"1", "observe_returned_items":1,
                "observe_items_with_issues":2}),
        ])?;
        let observed = &value["observations"];
        assert_eq!(observed["calls"], 2);
        assert_eq!(observed["requestCountsUnknown"], 2);
        assert_eq!(observed["resultCountsUnknown"], 2);
        assert_eq!(observed["requestedItems"], 0);
        assert_eq!(observed["returnedItems"], 0);
        Ok(())
    }

    #[test]
    fn read_and_observation_item_totals_saturate() -> anyhow::Result<()> {
        let read = json!({"event":"tool_call", "tool":"ReadFiles", "read_request_schema":1,
            "read_whole_file_requests":u64::MAX, "read_bounded_range_requests":1,
            "read_open_ended_range_requests":1});
        let observation = json!({"event":"tool_call", "tool":"Execution", "action":"observe",
            "workflow_schema":1, "observe_schema":1, "observe_requested_items":u64::MAX,
            "observe_returned_items":u64::MAX, "observe_items_with_issues":u64::MAX});
        let value = report(&[read.clone(), read, observation.clone(), observation])?;
        assert_eq!(value["reads"]["wholeFileRequests"], u64::MAX);
        assert_eq!(value["observations"]["requestedItems"], u64::MAX);
        assert_eq!(value["observations"]["returnedItems"], u64::MAX);
        assert_eq!(value["observations"]["itemsWithIssues"], u64::MAX);
        Ok(())
    }

    #[test]
    fn effective_wait_uses_bash_only_and_preserves_unknown_values() -> anyhow::Result<()> {
        let value = report(&[
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"adaptive",
                "effective_wait_mode":"synchronous", "effective_wait_ms":15000}),
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"until_complete",
                "effective_wait_mode":"task", "effective_wait_ms":0}),
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"adaptive",
                "effective_wait_mode":"task_inline", "effective_wait_ms":2000}),
            json!({"event":"tool_call", "tool":"BashCommand", "requested_wait_ms":15000}),
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"SECRET-POLICY",
                "effective_wait_mode":"SECRET-MODE", "effective_wait_ms":100}),
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"until_complete",
                "effective_wait_mode":"task", "effective_wait_ms":1}),
            json!({"event":"tool_call", "tool":"BashCommand", "effective_wait_policy":"adaptive",
                "effective_wait_mode":"synchronous", "effective_wait_ms":60001}),
            json!({"event":"tool_call", "tool":"VerifyEdit", "effective_wait_policy":"adaptive",
                "effective_wait_mode":"synchronous", "effective_wait_ms":15000}),
        ])?;
        let execution = &value["execution"];
        assert_eq!(execution["effectiveWaitObservations"], 7);
        assert_eq!(execution["effectiveWaitPolicies"], json!({"adaptive":2,"until_complete":1}));
        assert_eq!(execution["effectiveWaitPolicyUnknown"], 4);
        assert_eq!(
            execution["effectiveWaitModes"],
            json!({"synchronous":1,"task":1,"task_inline":1})
        );
        assert_eq!(execution["effectiveWaitModeUnknown"], 4);
        assert_eq!(execution["effectiveWaitMs"]["samples"], 3);
        assert_eq!(execution["effectiveWaitMs"]["averageMs"], 5666);
        assert_eq!(execution["effectiveWaitMs"]["maxMs"], 15000);
        assert!(!serde_json::to_string(execution)?.contains("SECRET-"));
        Ok(())
    }
}
