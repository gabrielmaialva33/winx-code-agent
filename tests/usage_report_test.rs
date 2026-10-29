use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use winx_code_agent::report::usage_report;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn write_events(path: &Path, fields: &[Value]) -> TestResult {
    let mut file = File::create(path)?;
    for fields in fields {
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-10-27T12:00:00Z",
                "target": "winx::usage",
                "fields": fields,
            })
        )?;
    }
    Ok(())
}

#[test]
fn auxiliary_http_lifecycle_does_not_displace_the_last_tool_http_pair() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"http_request_received", "request_id":"selected"}),
            json!({"event":"tool_call", "tool":"ReadFiles", "usage_schema":1,
                "request_id":"selected", "principal":"principal-a", "duration_ms":10,
                "build":"0.2.356+fixture", "build_version":"0.2.356"}),
            json!({"event":"http_request", "request_id":"selected",
                "principal":"principal-a", "status":200, "duration_ms":17}),
            json!({"event":"http_response_body", "request_id":"selected", "outcome":"complete",
                "request_elapsed_ms":35, "body_duration_ms":18}),
            json!({"event":"http_response_body", "request_id":"older-stream", "outcome":"dropped",
                "request_elapsed_ms":5000, "body_duration_ms":4990}),
            json!({"event":"http_request_received", "request_id":"not-ready-yet"}),
        ],
    )?;
    for build in [None, Some("0.2.356+fixture")] {
        let report = usage_report(Some(&base), 2, 0, build)?;
        assert_eq!(report.events, 2, "{build:?}");
        assert_eq!(report.tool_calls, 1, "{build:?}");
        assert_eq!(report.http_requests, 1, "{build:?}");
        assert_eq!(report.correlated_requests, 1, "{build:?}");
        assert_eq!(report.tool_calls_without_http, 0, "{build:?}");
        assert_eq!(report.http_without_tool_call, 0, "{build:?}");
        assert_eq!(report.http_overhead.samples, 1, "{build:?}");
        assert_eq!(report.http_overhead.average_ms, 7, "{build:?}");
    }
    Ok(())
}

#[test]
fn exact_build_preserves_http_pair_and_overhead() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"selected", "principal":"principal-a", "duration_ms":10,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"selected",
                "principal":"principal-a", "status":200, "duration_ms":17}),
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"other-revision", "principal":"principal-a", "duration_ms":30,
                "build":"0.2.343+def456", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"other-revision",
                "principal":"principal-a", "status":500, "duration_ms":80}),
        ],
    )?;

    let report = usage_report(Some(&base), 100, 0, Some("0.2.343+abc123"))?;
    assert_eq!(report.events, 2);
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.http_requests, 1);
    assert_eq!(report.correlated_requests, 1);
    assert_eq!(report.tool_calls_without_http, 0);
    assert_eq!(report.http_without_tool_call, 0);
    assert_eq!(report.http_statuses.get("200"), Some(&1));
    assert_eq!(report.http_statuses.len(), 1);
    assert_eq!(report.build_identities.get("0.2.343+abc123"), Some(&1));
    assert_eq!(report.build_identities.len(), 1);
    assert_eq!(report.http_overhead.samples, 1);
    assert_eq!(report.http_overhead.average_ms, 7);
    assert_eq!(report.http_overhead.p95_ms, 7);
    Ok(())
}

#[test]
fn package_version_includes_both_revisions_and_excludes_unrelated_http() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"revision-a", "principal":"principal-a", "duration_ms":10,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"revision-a",
                "principal":"principal-a", "status":200, "duration_ms":13}),
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"revision-b", "principal":"principal-a", "duration_ms":20,
                "build":"0.2.343+def456", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"revision-b",
                "principal":"principal-a", "status":201, "duration_ms":25}),
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"other-version", "principal":"principal-a", "duration_ms":100,
                "build":"0.2.344+fed321", "build_version":"0.2.344"}),
            json!({"event":"http_request", "request_id":"other-version",
                "principal":"principal-a", "status":500, "duration_ms":130}),
            json!({"event":"http_request", "request_id":"http-orphan",
                "principal":"principal-a", "status":418, "duration_ms":999}),
        ],
    )?;

    let report = usage_report(Some(&base), 100, 0, Some("0.2.343"))?;
    assert_eq!(report.events, 4);
    assert_eq!(report.tool_calls, 2);
    assert_eq!(report.http_requests, 2);
    assert_eq!(report.correlated_requests, 2);
    assert_eq!(report.tool_calls_without_http, 0);
    assert_eq!(report.http_without_tool_call, 0);
    assert_eq!(report.http_statuses.get("200"), Some(&1));
    assert_eq!(report.http_statuses.get("201"), Some(&1));
    assert_eq!(report.http_statuses.len(), 2);
    assert_eq!(report.build_identities.get("0.2.343+abc123"), Some(&1));
    assert_eq!(report.build_identities.get("0.2.343+def456"), Some(&1));
    assert_eq!(report.build_identities.len(), 2);
    assert_eq!(report.http_overhead.samples, 2);
    assert_eq!(report.http_overhead.average_ms, 4);
    assert_eq!(report.http_overhead.p50_ms, 3);
    assert_eq!(report.http_overhead.p95_ms, 5);
    Ok(())
}

#[test]
fn build_filter_correlates_http_before_tool_across_rotated_logs() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &temp.path().join("usage.jsonl.2026-10-26"),
        &[json!({"event":"http_request", "request_id":"rotated",
            "principal":"principal-a", "status":200, "duration_ms":18})],
    )?;
    write_events(
        &temp.path().join("usage.jsonl.2026-10-27"),
        &[json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
            "request_id":"rotated", "principal":"principal-a", "duration_ms":11,
            "build":"0.2.343+abc123", "build_version":"0.2.343"})],
    )?;

    let report = usage_report(Some(&base), 100, 0, Some("0.2.343+abc123"))?;
    assert_eq!(report.source_files, 2);
    assert_eq!(report.events, 2);
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.http_requests, 1);
    assert_eq!(report.correlated_requests, 1);
    assert_eq!(report.http_overhead.samples, 1);
    assert_eq!(report.http_overhead.average_ms, 7);
    Ok(())
}

#[test]
fn build_filter_rejects_unidentified_and_explicitly_incompatible_http() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"selected", "principal":"principal-a", "duration_ms":10,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"selected",
                "principal":"principal-a", "status":200, "duration_ms":15}),
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "principal":"principal-a", "duration_ms":20,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}),
            json!({"event":"http_request", "principal":"principal-a",
                "status":400, "duration_ms":100}),
            json!({"event":"http_request", "request_id":"", "principal":"principal-a",
                "status":401, "duration_ms":200}),
            json!({"event":"http_request", "request_id":"selected",
                "principal":"principal-a", "status":500, "duration_ms":300,
                "build":"0.2.344+fed321"}),
            json!({"event":"http_request", "request_id":"selected",
                "principal":"principal-a", "status":503, "duration_ms":400,
                "build_version":"0.2.344"}),
        ],
    )?;

    let report = usage_report(Some(&base), 100, 0, Some("0.2.343+abc123"))?;
    assert_eq!(report.events, 3);
    assert_eq!(report.tool_calls, 2);
    assert_eq!(report.http_requests, 1);
    assert_eq!(report.correlated_requests, 1);
    assert_eq!(report.http_without_tool_call, 0);
    assert_eq!(report.http_statuses.get("200"), Some(&1));
    assert_eq!(report.http_statuses.len(), 1);
    assert_eq!(report.http_overhead.samples, 1);
    assert_eq!(report.http_overhead.average_ms, 5);
    Ok(())
}

#[test]
fn no_build_filter_preserves_pairs_and_orphans() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"paired", "principal":"principal-a", "duration_ms":10,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}),
            json!({"event":"http_request", "request_id":"paired",
                "principal":"principal-a", "status":200, "duration_ms":14}),
            json!({"event":"tool_call", "tool":"BashCommand", "usage_schema":1,
                "request_id":"tool-orphan", "principal":"principal-a", "duration_ms":20,
                "build":"0.2.344+fed321", "build_version":"0.2.344"}),
            json!({"event":"http_request", "request_id":"http-orphan",
                "principal":"principal-a", "status":503, "duration_ms":99}),
            json!({"event":"http_request", "principal":"principal-a",
                "status":418, "duration_ms":200}),
        ],
    )?;

    let report = usage_report(Some(&base), 100, 0, None)?;
    assert_eq!(report.events, 5);
    assert_eq!(report.tool_calls, 2);
    assert_eq!(report.http_requests, 3);
    assert_eq!(report.correlated_requests, 1);
    assert_eq!(report.tool_calls_without_http, 1);
    assert_eq!(report.http_without_tool_call, 1);
    assert_eq!(report.http_statuses.get("200"), Some(&1));
    assert_eq!(report.http_statuses.get("503"), Some(&1));
    assert_eq!(report.http_statuses.get("418"), Some(&1));
    assert_eq!(report.build_identities.len(), 2);
    assert_eq!(report.http_overhead.samples, 1);
    assert_eq!(report.http_overhead.average_ms, 4);
    Ok(())
}

#[test]
fn last_limits_events_without_removing_build_membership_evidence() -> TestResult {
    let first_tool = json!({"event":"tool_call", "tool":"ReadFiles",
        "request_id":"first", "principal":"principal-a", "duration_ms":10,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let first_http = json!({"event":"http_request", "request_id":"first",
        "principal":"principal-a", "status":201, "duration_ms":15});
    let second_tool = json!({"event":"tool_call", "tool":"BashCommand",
        "request_id":"second", "principal":"principal-a", "duration_ms":20,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let second_http = json!({"event":"http_request", "request_id":"second",
        "principal":"principal-a", "status":202, "duration_ms":27});
    let old_tool = json!({"event":"tool_call", "tool":"Initialize",
        "request_id":"old", "principal":"principal-a", "duration_ms":5,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let old_http = json!({"event":"http_request", "request_id":"old",
        "principal":"principal-a", "status":203, "duration_ms":8});
    let standalone_http = json!({"event":"http_request", "request_id":"standalone",
        "principal":"principal-a", "status":204, "duration_ms":3,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let cases = [
        (
            "tool then HTTP",
            vec![first_tool.clone(), first_http.clone()],
            1,
            (1, 0, 1, 0),
            vec![],
            json!({"201":1}),
        ),
        (
            "HTTP then tool",
            vec![first_http.clone(), first_tool.clone()],
            1,
            (1, 1, 0, 0),
            vec!["ReadFiles"],
            json!({}),
        ),
        (
            "delayed old counterpart",
            vec![old_tool, first_tool.clone(), second_tool.clone(), old_http],
            2,
            (2, 1, 1, 0),
            vec!["BashCommand"],
            json!({"203":1}),
        ),
        (
            "two pairs with a three-event limit",
            vec![first_tool, first_http, second_tool, second_http],
            3,
            (3, 1, 2, 1),
            vec!["BashCommand"],
            json!({"201":1,"202":1}),
        ),
        (
            "explicitly matching standalone HTTP",
            vec![standalone_http],
            1,
            (1, 0, 1, 0),
            vec![],
            json!({"204":1}),
        ),
    ];

    for (name, fields, last, counts, tools, statuses) in cases {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        write_events(&base, &fields)?;
        let report = usage_report(Some(&base), last, 0, Some("0.2.343+abc123"))?;
        assert_eq!(
            (report.events, report.tool_calls, report.http_requests, report.correlated_requests),
            counts,
            "{name}"
        );
        assert_eq!(report.tools.keys().map(String::as_str).collect::<Vec<_>>(), tools, "{name}");
        assert_eq!(serde_json::to_value(&report.http_statuses)?, statuses, "{name}");
    }
    Ok(())
}

#[test]
fn since_excludes_expired_events_without_losing_build_membership() -> TestResult {
    let now = OffsetDateTime::now_utc();
    let [recent, expired] = [now, now - Duration::hours(2)].map(|timestamp| {
        format!(
            "{}T{:02}:{:02}:{:02}Z",
            timestamp.date(),
            timestamp.hour(),
            timestamp.minute(),
            timestamp.second()
        )
    });
    let cases = [
        ("expired tool", expired.as_str(), recent.as_str(), (1, 0, 1, 0)),
        ("expired HTTP", recent.as_str(), expired.as_str(), (1, 1, 0, 0)),
    ];

    for (name, tool_timestamp, http_timestamp, counts) in cases {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        let mut file = File::create(&base)?;
        for record in [
            json!({"timestamp":tool_timestamp, "target":"winx::usage", "fields":{
                "event":"tool_call", "tool":"ReadFiles", "request_id":"timed",
                "principal":"principal-a", "duration_ms":10,
                "build":"0.2.343+abc123", "build_version":"0.2.343"}}),
            json!({"timestamp":http_timestamp, "target":"winx::usage", "fields":{
                "event":"http_request", "request_id":"timed",
                "principal":"principal-a", "status":200, "duration_ms":15}}),
        ] {
            writeln!(file, "{record}")?;
        }
        drop(file);

        let report = usage_report(Some(&base), 100, 60, Some("0.2.343+abc123"))?;
        assert_eq!(
            (report.events, report.tool_calls, report.http_requests, report.correlated_requests),
            counts,
            "{name}"
        );
        assert_eq!(report.first_timestamp.as_deref(), Some(recent.as_str()), "{name}");
        assert_eq!(report.last_timestamp.as_deref(), Some(recent.as_str()), "{name}");
        assert_eq!(report.http_overhead.samples, 0, "{name}");
    }
    Ok(())
}

#[test]
fn inferred_http_requires_a_compatible_principal_and_unambiguous_build() -> TestResult {
    let selected = json!({"event":"tool_call", "tool":"ReadFiles",
        "request_id":"reused", "principal":"principal-a", "duration_ms":10,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let other_build = json!({"event":"tool_call", "tool":"ReadFiles",
        "request_id":"reused", "principal":"principal-a", "duration_ms":20,
        "build":"0.2.344+fed321", "build_version":"0.2.344"});
    let legacy_tool = json!({"event":"tool_call", "tool":"ReadFiles",
        "request_id":"reused", "duration_ms":10,
        "build":"0.2.343+abc123", "build_version":"0.2.343"});
    let matching_http = json!({"event":"http_request", "request_id":"reused",
        "principal":"principal-a", "status":200, "duration_ms":15});
    let other_principal_http = json!({"event":"http_request", "request_id":"reused",
        "principal":"principal-b", "status":500, "duration_ms":50});
    let legacy_http = json!({"event":"http_request", "request_id":"reused",
        "status":200, "duration_ms":15});
    let cases = [
        ("different principals", vec![selected.clone(), other_principal_http], (1, 1, 0, 0)),
        ("missing HTTP principal", vec![selected.clone(), legacy_http.clone()], (1, 1, 0, 0)),
        (
            "request and principal reused across builds",
            vec![selected, other_build, matching_http],
            (1, 1, 0, 0),
        ),
        ("legacy principals both absent", vec![legacy_tool, legacy_http], (2, 1, 1, 1)),
    ];

    for (name, fields, counts) in cases {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        write_events(&base, &fields)?;
        let report = usage_report(Some(&base), 100, 0, Some("0.2.343+abc123"))?;
        assert_eq!(
            (report.events, report.tool_calls, report.http_requests, report.correlated_requests),
            counts,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn unrelated_http_cannot_hide_an_older_pair_or_multiply_scan_counts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    let mut fields = vec![
        json!({"event":"tool_call", "tool":"ReadFiles", "request_id":"selected",
            "principal":"principal-a", "duration_ms":10,
            "build":"0.2.343+abc123", "build_version":"0.2.343"}),
        json!({"event":"http_request", "request_id":"selected",
            "principal":"principal-a", "status":200, "duration_ms":15}),
    ];
    for index in 0..4100 {
        fields.push(json!({"event":"http_request", "request_id":format!("orphan-{index}"),
            "principal":"principal-a", "status":503, "duration_ms":100}));
    }
    write_events(&base, &fields)?;
    let mut file = OpenOptions::new().append(true).open(&base)?;
    writeln!(file, "not-json")?;
    writeln!(file, "{}", json!({"target":"unrelated::target", "fields":{}}))?;
    drop(file);

    let report = usage_report(Some(&base), 2, 0, Some("0.2.343+abc123"))?;
    assert_eq!(report.source_files, 1);
    assert_eq!(report.lines_seen, 4104);
    assert_eq!(report.invalid_lines, 2);
    assert_eq!(report.events, 2);
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.http_requests, 1);
    assert_eq!(report.correlated_requests, 1);
    assert_eq!(report.http_without_tool_call, 0);
    assert_eq!(report.http_statuses.get("200"), Some(&1));
    assert_eq!(report.http_statuses.len(), 1);
    assert_eq!(report.http_overhead.samples, 1);
    assert_eq!(report.http_overhead.average_ms, 5);
    Ok(())
}

fn workflow_fixture_events() -> Vec<Value> {
    let mut records = vec![
        json!({"tool":"BashCommand","action":"command","duration_ms":7,"exit_code":0}),
        json!({"tool":"Execution","workflow_schema":1,"action":"wait","workflow_backend":"native",
            "workflow_wait_kind":"catalog","requested_wait_ms":30_000,"duration_ms":30_010,
            "workflow_wait_outcome":"timed_out"}),
        json!({"tool":"Execution","workflow_schema":1,"action":"read","workflow_backend":"native",
            "duration_ms":3,"response_bytes":10}),
        json!({"tool":"Agent","workflow_schema":1,"action":"wait","workflow_backend":"native",
            "workflow_wait_kind":"receipt","requested_wait_ms":0,"duration_ms":4,
            "prompt_receipt_present":true,"prompt_delivery":"delivered","prompt_activity_observed":false,
            "prompt_outcome":"timed_out","agent_input_sent":false,"execution_id":"same-fingerprint"}),
        json!({"tool":"Agent","workflow_schema":1,"action":"wait","workflow_backend":"native",
            "workflow_wait_kind":"receipt","duration_ms":9,"response_bytes":12,
            "prompt_receipt_present":true,"prompt_delivery":"delivered","prompt_activity_observed":true,
            "prompt_outcome":"exited","execution_id":"same-fingerprint"}),
        json!({"tool":"Agent","workflow_schema":1,"action":"prompt","workflow_backend":"herdr",
            "duration_ms":8,"agent_submission_status":"accepted","agent_input_sent":true}),
        json!({"tool":"Agent","workflow_schema":1,"action":"prompt","workflow_backend":"herdr",
            "duration_ms":11,"agent_input_may_have_been_sent":true,"error_code":"herdr_delivery_uncertain"}),
        json!({"tool":"Agent","workflow_schema":1,"action":"wait","workflow_backend":"herdr",
            "workflow_wait_kind":"herdr","duration_ms":5000,"result_status":"completed"}),
        json!({"tool":"Agent","duration_ms":2}),
        json!({"tool":"Execution","duration_ms":1}),
    ];
    for (index, record) in records.iter_mut().enumerate() {
        record["event"] = json!("tool_call");
        record["usage_schema"] = json!(1);
        record["request_id"] = json!(format!("workflow-{index}"));
        record["principal"] = json!("principal-a");
        record["build"] = json!("0.2.343+abc123");
        record["build_version"] = json!("0.2.343");
    }
    let mut events = Vec::new();
    for record in records {
        events.push(json!({"event":"http_request","request_id":record["request_id"],
            "principal":"principal-a","status":200,
            "duration_ms":record["duration_ms"].as_u64().unwrap_or_default() + 5}));
        events.push(record);
    }
    events
}

fn workflow_operation<'a>(
    report: &'a Value,
    tool: &str,
    action: &str,
    backend: &str,
) -> TestResult<&'a Value> {
    report["workflows"]["operations"]
        .as_array()
        .ok_or("workflow operations missing")?
        .iter()
        .find(|operation| {
            operation["tool"] == tool
                && operation["action"] == action
                && operation["backend"] == backend
        })
        .ok_or_else(|| "expected operation group missing".into())
}

#[test]
fn workflow_report_repeated_receipts_are_observations_and_waits_keep_separate_latency() -> TestResult
{
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(&base, &workflow_fixture_events())?;
    let report = serde_json::to_value(usage_report(Some(&base), 100, 0, None)?)?;
    assert_eq!(report["toolCalls"], 10);
    assert_eq!(report["httpRequests"], 10);
    assert_eq!(report["correlatedRequests"], 10);
    assert_eq!(report["httpOverhead"]["averageMs"], 5);
    assert_eq!(report["execution"]["observations"], 1);
    assert_eq!(report["execution"]["exitCodes"], json!({"0":1}));
    assert_eq!(report["workflows"]["calls"], 9);
    let receipt = workflow_operation(&report, "Agent", "wait", "native")?;
    assert_eq!(receipt["calls"], 2);
    assert_eq!(receipt["receiptObservations"], 2);
    assert_eq!(receipt["promptDelivery"], json!({"delivered":2}));
    assert_eq!(receipt["promptOutcome"], json!({"exited":1,"timed_out":1}));
    assert_eq!(receipt["promptActivityObservedTrue"], 1);
    assert_eq!(receipt["promptActivityObservedFalse"], 1);
    assert_eq!(receipt["promptActivityObservedUnknown"], 0);
    assert_eq!(receipt["agentInputSentFalse"], 1);
    assert_eq!(receipt["requestedWaitMs"]["samples"], 1);
    assert_eq!(receipt["requestedWaitMs"]["maxMs"], 0);
    assert_eq!(receipt["waitOutcomeUnknown"], 0);
    let read = workflow_operation(&report, "Execution", "read", "native")?;
    assert_eq!(read["latency"]["p95Ms"], 3);
    assert_eq!(read["responseBytes"], 10);
    let wait = workflow_operation(&report, "Execution", "wait", "native")?;
    assert_eq!(wait["latency"]["p95Ms"], 30_010);
    assert_eq!(wait["waitKinds"], json!({"catalog":1}));
    assert_eq!(wait["waitOutcomes"], json!({"timed_out":1}));
    let herdr = workflow_operation(&report, "Agent", "prompt", "herdr")?;
    assert_eq!(herdr["agentSubmissionAccepted"], 1);
    assert_eq!(herdr["agentInputSentTrue"], 1);
    assert_eq!(herdr["agentInputMayHaveBeenSentTrue"], 1);
    assert_eq!(herdr["errorResults"], 1);
    assert_eq!(workflow_operation(&report, "Agent", "wait", "herdr")?["waitOutcomeUnknown"], 1);
    assert_eq!(workflow_operation(&report, "Agent", "unknown", "unknown")?["calls"], 1);
    assert_eq!(workflow_operation(&report, "Execution", "unknown", "unknown")?["calls"], 1);
    for invented in ["uniqueExecutions", "promptsDelivered", "tasksCompleted"] {
        assert!(!report.to_string().contains(invented), "unsupported metric: {invented}");
    }
    Ok(())
}

#[test]
fn workflow_report_cli_preserves_exact_build_package_and_http_denominators() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    let mut events = workflow_fixture_events();
    events.extend([
        json!({"event":"tool_call","tool":"Agent","action":"read","workflow_schema":1,
            "workflow_backend":"native","request_id":"other-revision","principal":"principal-a",
            "build":"0.2.343+def456","build_version":"0.2.343","duration_ms":2}),
        json!({"event":"http_request","request_id":"other-revision","principal":"principal-a",
            "status":201,"duration_ms":7}),
    ]);
    write_events(&base, &events)?;
    for (build, calls, workflow_calls) in [("0.2.343+abc123", 10, 9), ("0.2.343", 11, 10)] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_winx-code-agent"))
            .args(["report", "--log"])
            .arg(&base)
            .args(["--last", "100", "--since-minutes", "0", "--build", build])
            .env_remove("WINX_USAGE_LOG")
            .env_remove("WINX_USAGE_LOG_ROTATION")
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let report: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["toolCalls"], calls);
        assert_eq!(report["httpRequests"], calls);
        assert_eq!(report["correlatedRequests"], calls);
        assert_eq!(report["httpOverhead"]["averageMs"], 5);
        assert_eq!(report["execution"]["observations"], 1);
        assert_eq!(report["workflows"]["calls"], workflow_calls);
    }
    Ok(())
}

#[test]
fn workflow_report_allowlists_external_metadata_and_preserves_unknown_receipt_state() -> TestResult
{
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_call","tool":"Agent","workflow_schema":1,"action":"SECRET-ACTION",
            "workflow_backend":"SECRET-BACKEND","workflow_wait_kind":"SECRET-KIND",
            "prompt_delivery":"SECRET-DELIVERY","prompt_outcome":"SECRET-OUTCOME",
            "arguments":{"text":"SECRET-TEXT"},"output":"SECRET-OUTPUT"}),
            json!({"event":"tool_call","tool":"Agent","workflow_schema":1,"action":"wait",
            "workflow_backend":"native","workflow_wait_kind":"receipt","prompt_receipt_present":true,
            "prompt_delivery":"SECRET-DELIVERY","prompt_outcome":"SECRET-OUTCOME",
            "prompt_activity_observed":"false"}),
        ],
    )?;
    let report = serde_json::to_value(usage_report(Some(&base), 100, 0, None)?)?;
    assert_eq!(report["workflows"]["calls"], 2);
    assert_eq!(workflow_operation(&report, "Agent", "unknown", "unknown")?["calls"], 1);
    let receipt = workflow_operation(&report, "Agent", "wait", "native")?;
    assert_eq!(receipt["receiptObservations"], 1);
    assert_eq!(receipt["promptDelivery"], json!({}));
    assert_eq!(receipt["promptOutcome"], json!({}));
    assert_eq!(receipt["promptOutcomeUnknown"], 1);
    assert_eq!(receipt["promptActivityObservedUnknown"], 1);
    assert_eq!(receipt["promptActivityObservedFalse"], 0);
    assert_eq!(receipt["latency"]["samples"], 0);
    assert!(!report.to_string().contains("SECRET-"));
    Ok(())
}

fn write_lifecycle_fixture(path: &Path, fields: &[Value], timestamp: &str) -> TestResult {
    let mut file = File::create(path)?;
    for fields in fields {
        writeln!(
            file,
            "{}",
            json!({"timestamp":timestamp,"target":"winx::usage","fields":fields})
        )?;
    }
    Ok(())
}

#[test]
fn lifecycle_report_auxiliaries_never_evict_terminal_http_or_change_since_build_selection(
) -> TestResult {
    let temp = tempfile::tempdir()?;
    let plain_path = temp.path().join("plain.jsonl");
    let trace_path = temp.path().join("trace.jsonl");
    let now = OffsetDateTime::now_utc();
    let timestamp =
        format!("{}T{:02}:{:02}:{:02}Z", now.date(), now.hour(), now.minute(), now.second());
    let tool = json!({"event":"tool_call","tool":"Agent","workflow_schema":1,"action":"wait",
        "workflow_backend":"native","workflow_wait_kind":"catalog","request_id":"same-id",
        "principal":"principal-a","duration_ms":10,"build":"0.2.343+abc123","build_version":"0.2.343"});
    let http = json!({"event":"http_request","request_id":"same-id","principal":"principal-a",
        "duration_ms":17,"status":200});
    let legacy_extra =
        json!({"event":"legacy_extra","build":"0.2.343+abc123","build_version":"0.2.343"});
    let plain = vec![legacy_extra.clone(), tool.clone(), http.clone()];
    let mut traced = vec![legacy_extra, tool];
    for index in 0..3000 {
        traced.push(
            json!({"event": (["tool_started","tool_phase","tool_phase_trace_truncated"][index % 3]),
            "lifecycle_schema":1,"request_id":"same-id","usage_call_id":"call-id",
            "principal":"principal-a","tool":"Agent","phase":"catalog","phase_edge":"start",
            "build":"0.2.343+abc123","build_version":"0.2.343"}),
        );
    }
    traced.push(http);
    write_lifecycle_fixture(&plain_path, &plain, &timestamp)?;
    write_lifecycle_fixture(&trace_path, &traced, &timestamp)?;
    for build in [None, Some("0.2.343+abc123"), Some("0.2.343")] {
        for since in [0, 60] {
            for last in [1, 2, 3, 100] {
                let mut plain =
                    serde_json::to_value(usage_report(Some(&plain_path), last, since, build)?)?;
                let mut traced =
                    serde_json::to_value(usage_report(Some(&trace_path), last, since, build)?)?;
                assert_eq!(
                    traced["linesSeen"],
                    plain["linesSeen"].as_u64().unwrap_or_default() + 3000
                );
                for report in [&mut plain, &mut traced] {
                    let object = report.as_object_mut().ok_or("report must be an object")?;
                    object.remove("basePath");
                    object.remove("linesSeen");
                }
                assert_eq!(traced, plain, "build={build:?}, since={since}, last={last}");
            }
        }
    }
    Ok(())
}

#[test]
fn lifecycle_report_auxiliaries_cannot_attribute_orphan_http_to_a_build() -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    write_events(
        &base,
        &[
            json!({"event":"tool_started","lifecycle_schema":1,"tool":"Agent","request_id":"orphan",
            "principal":"principal-a","build":"0.2.343+abc123","build_version":"0.2.343"}),
            json!({"event":"tool_phase","lifecycle_schema":1,"tool":"Agent","request_id":"orphan",
            "principal":"principal-a","build":"0.2.343+abc123","build_version":"0.2.343"}),
            json!({"event":"http_request","request_id":"orphan","principal":"principal-a","status":200,"duration_ms":50}),
        ],
    )?;
    let report = usage_report(Some(&base), 100, 0, Some("0.2.343+abc123"))?;
    assert_eq!(report.events, 0);
    assert_eq!(report.tool_calls, 0);
    assert_eq!(report.http_requests, 0);
    assert_eq!(report.correlated_requests, 0);
    assert_eq!(report.invalid_lines, 0);
    assert_eq!(report.lines_seen, 3);
    Ok(())
}

#[test]
fn lifecycle_report_phase_totals_sample_each_call_and_preserve_submillisecond_observations(
) -> TestResult {
    let temp = tempfile::tempdir()?;
    let base = temp.path().join("usage.jsonl");
    let mut records = Vec::new();
    for summary in [
        json!({"phase_catalog_ms":3,"phase_catalog_calls":2,"phase_wait_idle_ms":100,
            "phase_wait_idle_calls":4,"phase_abandoned_count":1,"phase_trace_suppressed_count":5}),
        json!({"phase_catalog_ms":0,"phase_catalog_calls":1}),
        json!({"phase_SECRET_ms":999,"phase_SECRET_calls":1}),
    ] {
        let mut record = json!({"event":"tool_call","tool":"Execution","workflow_schema":1,
            "action":"wait","workflow_backend":"native","workflow_wait_kind":"catalog","duration_ms":110});
        record
            .as_object_mut()
            .ok_or("record must be an object")?
            .extend(summary.as_object().ok_or("summary must be an object")?.clone());
        records.push(record);
    }
    write_events(&base, &records)?;
    let report = serde_json::to_value(usage_report(Some(&base), 100, 0, None)?)?;
    let operation = workflow_operation(&report, "Execution", "wait", "native")?;
    assert_eq!(operation["calls"], 3);
    assert_eq!(operation["phases"]["catalog"]["calls"], 3);
    assert_eq!(operation["phases"]["catalog"]["latency"]["samples"], 2);
    assert_eq!(operation["phases"]["catalog"]["latency"]["averageMs"], 1);
    assert_eq!(operation["phases"]["catalog"]["latency"]["maxMs"], 3);
    assert_eq!(operation["phases"]["wait_idle"]["calls"], 4);
    assert_eq!(operation["phases"]["wait_idle"]["latency"]["samples"], 1);
    assert_eq!(operation["phases"]["wait_idle"]["latency"]["maxMs"], 100);
    assert_eq!(operation["phaseAbandonedCount"], 1);
    assert_eq!(operation["phaseTraceSuppressedCount"], 5);
    assert!(operation["phases"].get("runtime_capture").is_none());
    assert!(!report.to_string().contains("SECRET"));
    Ok(())
}
