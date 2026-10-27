use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use winx_code_agent::report::usage_report;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

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
