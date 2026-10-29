use super::captured_usage_request;
use serde_json::{json, Value};

#[test]
fn observe_usage_counts_requested_returned_and_issue_bearing_items() -> anyhow::Result<()> {
    let fields = captured_usage_request(
        "Execution",
        "observe",
        &json!({"action":{"type":"observe","items":[
            {"execution_id":"SECRET-one"},{"execution_id":"SECRET-two"},
            {"execution_id":"SECRET-three"},{"execution_id":"SECRET-four"}
        ]}}),
        Some(json!({"tool":"Execution","status":"completed_with_issues",
            "errorCode":"execution_observation_issues","data":{"items":[
                {"errors":[]},{"errors":[]},
                {"errors":[{"stage":"refresh"},{"stage":"read"}]}
            ],"items_with_issues":1}})),
    )?;
    assert_eq!(fields["batch_items"], 4);
    assert_eq!(fields["observe_schema"], 1);
    assert_eq!(fields["observe_requested_items"], 4);
    assert_eq!(fields["observe_returned_items"], 3);
    assert_eq!(fields["observe_items_with_issues"], 1);
    assert_eq!(fields["result_status"], "completed_with_issues");
    assert!(!serde_json::to_string(&fields)?.contains("SECRET"));
    Ok(())
}

#[test]
fn observe_usage_preserves_unknown_result_counts() -> anyhow::Result<()> {
    for data in [json!({}), json!({"items":null}), json!({"items":[{"errors":"unknown"}]})] {
        let fields = captured_usage_request(
            "Execution",
            "observe",
            &json!({"action":{"type":"observe","items":[{"execution_id":"one"}]}}),
            Some(json!({"tool":"Execution","status":"failed","data":data})),
        )?;
        assert_eq!(fields["observe_requested_items"], 1);
        assert!(fields.get("observe_items_with_issues").is_none_or(Value::is_null));
    }
    let fields = captured_usage_request(
        "Execution",
        "observe",
        &json!({"action":{"type":"observe","items":[]}}),
        None,
    )?;
    assert_eq!(fields["observe_requested_items"], 0);
    assert!(fields.get("observe_returned_items").is_none_or(Value::is_null));
    Ok(())
}

#[test]
fn observation_usage_is_scoped_to_execution_observe() -> anyhow::Result<()> {
    for (tool, action) in [
        ("Agent", json!("observe")),
        ("Execution", json!({"type":"read"})),
        ("ReadFiles", json!({"type":"observe"})),
    ] {
        let fields = captured_usage_request(
            tool,
            "",
            &json!({"action":action,"items":[{"execution_id":"one"}]}),
            Some(
                json!({"tool":tool,"status":"completed","data":{"items":[{"errors":[]}],"items_with_issues":0}}),
            ),
        )?;
        for name in [
            "observe_schema",
            "observe_requested_items",
            "observe_returned_items",
            "observe_items_with_issues",
        ] {
            assert!(fields.get(name).is_none_or(Value::is_null), "{tool}: {name}");
        }
    }
    Ok(())
}

#[test]
fn read_usage_classifies_actual_range_grammar_without_paths() -> anyhow::Result<()> {
    let fields = captured_usage_request(
        "ReadFiles",
        "",
        &json!({"file_paths":[
            "/SECRET/a.rs","/SECRET/a.rs:10-20","/SECRET/a.rs:-20",
            "/SECRET/a.rs:20-","/SECRET/a.rs:20",
            "C:\\SECRET\\code.rs","/SECRET/name:notes","/SECRET/a.rs:10-20"
        ]}),
        Some(json!({"tool":"ReadFiles","status":"completed","data":{}})),
    )?;
    assert_eq!(fields["batch_items"], 8);
    assert_eq!(fields["read_request_schema"], 1);
    assert_eq!(fields["read_whole_file_requests"], 3);
    assert_eq!(fields["read_bounded_range_requests"], 3);
    assert_eq!(fields["read_open_ended_range_requests"], 2);
    assert!(!serde_json::to_string(&fields)?.contains("SECRET"));
    Ok(())
}

#[test]
fn read_usage_keeps_invalid_requests_unknown() -> anyhow::Result<()> {
    for arguments in [
        json!({}),
        json!({"file_paths":[]}),
        json!({"file_paths":"SECRET"}),
        json!({"file_paths":["SECRET",42]}),
    ] {
        let fields = captured_usage_request("ReadFiles", "", &arguments, None)?;
        assert_eq!(fields["read_request_schema"], 1);
        for name in [
            "read_whole_file_requests",
            "read_bounded_range_requests",
            "read_open_ended_range_requests",
        ] {
            assert!(fields.get(name).is_none_or(Value::is_null), "{arguments}: {name}");
        }
    }
    Ok(())
}

#[test]
fn read_usage_does_not_classify_other_tools() -> anyhow::Result<()> {
    let fields =
        captured_usage_request("CodeMap", "", &json!({"file_paths":["SECRET:10-20"]}), None)?;
    for name in [
        "read_request_schema",
        "read_whole_file_requests",
        "read_bounded_range_requests",
        "read_open_ended_range_requests",
    ] {
        assert!(fields.get(name).is_none_or(Value::is_null), "{name}");
    }
    Ok(())
}
