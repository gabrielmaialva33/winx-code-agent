//! Offline aggregation for privacy-safe `winx::usage` JSONL logs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};

use crate::errors::{Result, WinxError};

mod lifecycle;
mod observations;
mod selection;
mod workflows;

pub use lifecycle::WorkflowPhaseUsage;
use observations::ObservationAccumulator;
pub use observations::{EffectiveWaitUsage, ObserveUsage, ReadUsage};
use workflows::WorkflowAccumulator;
pub use workflows::{WorkflowOperationUsage, WorkflowUsage};

const MAX_REPORT_EVENTS: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub base_path: String,
    pub source_files: usize,
    pub lines_seen: u64,
    pub invalid_lines: u64,
    pub events: usize,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub tool_calls: usize,
    pub http_requests: usize,
    pub correlated_requests: usize,
    pub tool_calls_without_http: usize,
    pub http_without_tool_call: usize,
    pub duplicate_tool_request_ids: usize,
    pub http_statuses: BTreeMap<String, u64>,
    pub result_statuses: BTreeMap<String, u64>,
    pub error_codes: BTreeMap<String, u64>,
    pub command_kinds: BTreeMap<String, u64>,
    pub build_identities: BTreeMap<String, u64>,
    pub workspace_coherence: BTreeMap<String, u64>,
    pub tools: BTreeMap<String, ToolUsage>,
    pub latency: LatencySummary,
    pub http_overhead: LatencySummary,
    pub response_bytes: u64,
    pub initialize: InitializeUsage,
    pub recovery: RecoveryUsage,
    pub execution: ExecutionUsage,
    pub workflows: WorkflowUsage,
    pub reads: ReadUsage,
    pub observations: ObserveUsage,
    pub trace_audit: TraceAudit,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUsage {
    pub calls: u64,
    pub error_results: u64,
    pub response_bytes: u64,
    pub latency: LatencySummary,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySummary {
    pub samples: usize,
    pub average_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

/// Call-level execution observations, including polls and idempotent replays.
/// Unknown metadata must never be counted as exit code zero or complete output.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionUsage {
    pub observations: u64,
    pub exit_codes: BTreeMap<i64, u64>,
    pub exit_code_unknown: u64,
    pub output_truncated: u64,
    pub output_not_truncated: u64,
    pub output_truncation_unknown: u64,
    pub calls_with_execution_id: u64,
    pub calls_with_background_id: u64,
    pub calls_with_verification_id: u64,
    pub calls_with_mutation_receipt_id: u64,
    pub verification_replays: u64,
    pub requested_wait_policies: BTreeMap<String, u64>,
    pub requested_wait_policy_unknown: u64,
    pub requested_wait_ms: LatencySummary,
    #[serde(flatten)]
    pub effective_wait: EffectiveWaitUsage,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeUsage {
    pub calls: u64,
    pub reused: u64,
    pub response_bytes: u64,
    pub transitions: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryUsage {
    pub next_actions: u64,
    pub required_reads: u64,
    pub repeated_attempts: u64,
    pub escalated: u64,
    pub mutation_receipts_persisted: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceAudit {
    pub healthy: bool,
    pub violations: Vec<TraceViolation>,
    pub workspace_coherence_rejections: u64,
    pub audited_tool_events: u64,
    pub legacy_unverifiable_tool_events: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceViolation {
    pub request_id: String,
    pub rule: &'static str,
}

#[derive(Debug)]
struct UsageEvent {
    timestamp: Option<String>,
    fields: serde_json::Map<String, Value>,
}

#[derive(Default)]
struct ToolAccumulator {
    calls: u64,
    error_results: u64,
    response_bytes: u64,
    durations: Vec<u64>,
}

/// Aggregate recent usage events. `since_minutes=0` disables the time filter;
/// `build_filter` matches either the exact display identity or package version.
pub fn usage_report(
    path: Option<&Path>,
    last_events: usize,
    since_minutes: u64,
    build_filter: Option<&str>,
) -> Result<UsageReport> {
    if last_events == 0 || last_events > MAX_REPORT_EVENTS {
        return Err(WinxError::InvalidInput(format!(
            "--last must be between 1 and {MAX_REPORT_EVENTS}"
        )));
    }
    let base = path.map_or_else(default_usage_path, Path::to_path_buf);
    let sources = usage_sources(&base)?;
    let cutoff = (since_minutes > 0)
        .then(|| OffsetDateTime::now_utc() - Duration::minutes(i64_saturating(since_minutes)));
    let selected = selection::select(&sources, last_events, cutoff, build_filter)?;
    Ok(aggregate(
        &base,
        sources.len(),
        selected.lines_seen,
        selected.invalid_lines,
        &selected.events,
    ))
}

#[derive(Default)]
struct ReportAccumulator {
    result_statuses: BTreeMap<String, u64>,
    error_codes: BTreeMap<String, u64>,
    command_kinds: BTreeMap<String, u64>,
    build_identities: BTreeMap<String, u64>,
    workspace_coherence: BTreeMap<String, u64>,
    http_statuses: BTreeMap<String, u64>,
    tools: BTreeMap<String, ToolAccumulator>,
    initialize: InitializeUsage,
    recovery: RecoveryUsage,
    execution: ExecutionUsage,
    workflows: WorkflowAccumulator,
    observation_metrics: ObservationAccumulator,
    requested_wait_samples: Vec<u64>,
    trace_audit: TraceAudit,
    all_durations: Vec<u64>,
    tool_request_ids: BTreeSet<String>,
    duplicate_tool_request_ids: usize,
    http_request_ids: BTreeSet<String>,
    tool_durations: BTreeMap<String, u64>,
    http_durations: BTreeMap<String, u64>,
    tool_calls: usize,
    http_requests: usize,
    response_bytes: u64,
}

impl ReportAccumulator {
    fn record(&mut self, fields: &serde_json::Map<String, Value>) {
        match string(fields, "event") {
            "tool_call" => self.record_tool(fields),
            "http_request" => self.record_http(fields),
            _ => {}
        }
    }

    fn record_tool(&mut self, fields: &serde_json::Map<String, Value>) {
        self.tool_calls += 1;
        let tool = string(fields, "tool").to_string();
        increment_nonempty(&mut self.result_statuses, string(fields, "result_status"));
        increment_nonempty(&mut self.error_codes, string(fields, "error_code"));
        increment_nonempty(&mut self.command_kinds, string(fields, "command_kind"));
        increment_nonempty(&mut self.build_identities, string(fields, "build"));
        increment_nonempty(&mut self.workspace_coherence, string(fields, "workspace_coherence"));
        let bytes = number(fields, "response_bytes");
        let duration = number(fields, "duration_ms");
        self.response_bytes = self.response_bytes.saturating_add(bytes);
        self.all_durations.push(duration);

        let accumulator = self.tools.entry(tool.clone()).or_default();
        accumulator.calls = accumulator.calls.saturating_add(1);
        accumulator.error_results = accumulator
            .error_results
            .saturating_add(u64::from(!string(fields, "error_code").is_empty()));
        accumulator.response_bytes = accumulator.response_bytes.saturating_add(bytes);
        accumulator.durations.push(duration);

        let request_id = string(fields, "request_id");
        if !request_id.is_empty() {
            if !self.tool_request_ids.insert(request_id.to_string()) {
                self.duplicate_tool_request_ids += 1;
            }
            self.tool_durations.insert(request_id.to_string(), duration);
        }
        if tool == "Initialize" {
            self.initialize.calls = self.initialize.calls.saturating_add(1);
            self.initialize.response_bytes = self.initialize.response_bytes.saturating_add(bytes);
            self.initialize.reused = self
                .initialize
                .reused
                .saturating_add(u64::from(boolean(fields, "initialize_reused")));
            increment_nonempty(
                &mut self.initialize.transitions,
                string(fields, "initialize_transition"),
            );
        }
        self.record_recovery(fields);
        self.record_execution(fields);
        self.workflows.record(fields);
        self.observation_metrics.record(fields);
        audit_tool_event(fields, &mut self.trace_audit);
    }

    fn record_execution(&mut self, fields: &serde_json::Map<String, Value>) {
        let tool = string(fields, "tool");
        let edit_verification = tool == "EditFiles"
            && (!string(fields, "verification_status").is_empty()
                || !string(fields, "verification_id").is_empty());
        if !matches!(tool, "BashCommand" | "VerifyEdit") && !edit_verification {
            return;
        }
        let execution = &mut self.execution;
        execution.observations = execution.observations.saturating_add(1);
        match fields.get("exit_code").and_then(Value::as_i64) {
            Some(code) => {
                let count = execution.exit_codes.entry(code).or_default();
                *count = count.saturating_add(1);
            }
            None => execution.exit_code_unknown = execution.exit_code_unknown.saturating_add(1),
        }
        match fields.get("output_truncated").and_then(Value::as_bool) {
            Some(true) => execution.output_truncated = execution.output_truncated.saturating_add(1),
            Some(false) => {
                execution.output_not_truncated = execution.output_not_truncated.saturating_add(1);
            }
            None => {
                execution.output_truncation_unknown =
                    execution.output_truncation_unknown.saturating_add(1);
            }
        }
        execution.calls_with_execution_id = execution
            .calls_with_execution_id
            .saturating_add(u64::from(!string(fields, "execution_id").is_empty()));
        execution.calls_with_background_id = execution
            .calls_with_background_id
            .saturating_add(u64::from(!string(fields, "bg_command_id").is_empty()));
        execution.calls_with_verification_id = execution
            .calls_with_verification_id
            .saturating_add(u64::from(!string(fields, "verification_id").is_empty()));
        execution.calls_with_mutation_receipt_id = execution
            .calls_with_mutation_receipt_id
            .saturating_add(u64::from(!string(fields, "mutation_receipt_id").is_empty()));
        if fields.get("verification_replayed").and_then(Value::as_bool) == Some(true) {
            execution.verification_replays = execution.verification_replays.saturating_add(1);
        }
        match fields.get("requested_wait_policy").and_then(Value::as_str) {
            Some(policy @ ("adaptive" | "until_complete" | "return_early")) => {
                increment_nonempty(&mut execution.requested_wait_policies, policy);
            }
            _ => {
                execution.requested_wait_policy_unknown =
                    execution.requested_wait_policy_unknown.saturating_add(1);
            }
        }
        if let Some(wait) = fields.get("requested_wait_ms").and_then(Value::as_u64) {
            self.requested_wait_samples.push(wait);
        }
    }

    fn record_recovery(&mut self, fields: &serde_json::Map<String, Value>) {
        self.recovery.next_actions = self
            .recovery
            .next_actions
            .saturating_add(u64::from(!string(fields, "next_action_tool").is_empty()));
        self.recovery.required_reads =
            self.recovery.required_reads.saturating_add(number(fields, "required_read_count"));
        self.recovery.repeated_attempts = self
            .recovery
            .repeated_attempts
            .saturating_add(u64::from(number(fields, "recovery_attempt") > 1));
        self.recovery.escalated = self
            .recovery
            .escalated
            .saturating_add(u64::from(string(fields, "recovery_level") == "escalated"));
        self.recovery.mutation_receipts_persisted = self
            .recovery
            .mutation_receipts_persisted
            .saturating_add(u64::from(string(fields, "mutation_receipt_state") == "persisted"));
    }

    fn record_http(&mut self, fields: &serde_json::Map<String, Value>) {
        self.http_requests += 1;
        let status = fields.get("status").map(value_text).unwrap_or_default();
        increment_nonempty(&mut self.http_statuses, &status);
        let request_id = string(fields, "request_id");
        if !request_id.is_empty() {
            self.http_request_ids.insert(request_id.to_string());
            self.http_durations.insert(request_id.to_string(), number(fields, "duration_ms"));
        }
    }

    fn finish(mut self, window: ReportWindow<'_>) -> UsageReport {
        self.execution.requested_wait_ms = latency(self.requested_wait_samples);
        let (reads, observations, effective_wait) = self.observation_metrics.finish();
        self.execution.effective_wait = effective_wait;
        let correlated_requests =
            self.tool_request_ids.intersection(&self.http_request_ids).count();
        let overhead = self
            .tool_durations
            .iter()
            .filter_map(|(id, tool)| {
                self.http_durations.get(id).map(|http| http.saturating_sub(*tool))
            })
            .collect::<Vec<_>>();
        let tools = self
            .tools
            .into_iter()
            .map(|(name, accumulator)| {
                (
                    name,
                    ToolUsage {
                        calls: accumulator.calls,
                        error_results: accumulator.error_results,
                        response_bytes: accumulator.response_bytes,
                        latency: latency(accumulator.durations),
                    },
                )
            })
            .collect();
        self.trace_audit.healthy = self.trace_audit.violations.is_empty();

        UsageReport {
            base_path: window.base.to_string_lossy().into_owned(),
            source_files: window.source_files,
            lines_seen: window.lines_seen,
            invalid_lines: window.invalid_lines,
            events: window.events,
            first_timestamp: window.first_timestamp,
            last_timestamp: window.last_timestamp,
            tool_calls: self.tool_calls,
            http_requests: self.http_requests,
            correlated_requests,
            tool_calls_without_http: self
                .tool_request_ids
                .len()
                .saturating_sub(correlated_requests),
            http_without_tool_call: self.http_request_ids.len().saturating_sub(correlated_requests),
            duplicate_tool_request_ids: self.duplicate_tool_request_ids,
            http_statuses: self.http_statuses,
            result_statuses: self.result_statuses,
            error_codes: self.error_codes,
            command_kinds: self.command_kinds,
            build_identities: self.build_identities,
            workspace_coherence: self.workspace_coherence,
            tools,
            latency: latency(self.all_durations),
            http_overhead: latency(overhead),
            response_bytes: self.response_bytes,
            initialize: self.initialize,
            recovery: self.recovery,
            execution: self.execution,
            workflows: self.workflows.finish(),
            reads,
            observations,
            trace_audit: self.trace_audit,
        }
    }
}

struct ReportWindow<'a> {
    base: &'a Path,
    source_files: usize,
    lines_seen: u64,
    invalid_lines: u64,
    events: usize,
    first_timestamp: Option<String>,
    last_timestamp: Option<String>,
}

fn aggregate(
    base: &Path,
    source_files: usize,
    lines_seen: u64,
    invalid_lines: u64,
    events: &VecDeque<UsageEvent>,
) -> UsageReport {
    let mut accumulator = ReportAccumulator::default();
    for event in events {
        accumulator.record(&event.fields);
    }
    accumulator.finish(ReportWindow {
        base,
        source_files,
        lines_seen,
        invalid_lines,
        events: events.len(),
        first_timestamp: events.front().and_then(|event| event.timestamp.clone()),
        last_timestamp: events.back().and_then(|event| event.timestamp.clone()),
    })
}

fn audit_tool_event(fields: &serde_json::Map<String, Value>, audit: &mut TraceAudit) {
    if matches!(
        string(fields, "workspace_coherence"),
        "mismatch" | "rejected" | "binding_mismatch" | "thread_mismatch"
    ) {
        audit.workspace_coherence_rejections =
            audit.workspace_coherence_rejections.saturating_add(1);
    }
    if number(fields, "usage_schema") == 0 {
        // Older logs did not record the recovery booleans/actions needed to
        // prove these invariants. Treat them as unknown, never as violations.
        audit.legacy_unverifiable_tool_events =
            audit.legacy_unverifiable_tool_events.saturating_add(1);
        return;
    }
    audit.audited_tool_events = audit.audited_tool_events.saturating_add(1);
    let request_id = string(fields, "request_id");
    let mut violation = |rule| {
        if audit.violations.len() < 100 {
            audit.violations.push(TraceViolation { request_id: request_id.to_string(), rule });
        }
    };
    if boolean(fields, "retry_same_call") {
        violation("retry_same_call_must_be_false");
    }
    if boolean(fields, "edit_applied") && boolean(fields, "retry_same_call") {
        violation("committed_edit_must_never_be_retried");
    }
    if string(fields, "result_status") == "needs_read"
        && string(fields, "next_action_tool") != "ReadFiles"
    {
        violation("needs_read_must_point_to_read_files");
    }
    if matches!(string(fields, "error_code"), "search_block_not_found" | "search_block_ambiguous")
        && !boolean(fields, "fresh_read_required")
    {
        violation("search_conflict_must_invalidate_read_evidence");
    }
}

fn usage_sources(base: &Path) -> Result<Vec<PathBuf>> {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let name = base.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        WinxError::InvalidInput(format!("usage log path is not UTF-8: {}", base.display()))
    })?;
    let prefix = format!("{name}.");
    let mut sources = fs::read_dir(parent)
        .map_err(|error| WinxError::FileAccessError {
            path: parent.to_path_buf(),
            message: format!("listing usage logs: {error}"),
        })?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let entry_name = entry.file_name();
            let entry_name = entry_name.to_str()?;
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && (entry_name == name || entry_name.starts_with(&prefix)))
                .then_some(entry.path())
        })
        .collect::<Vec<_>>();
    sources.sort();
    if sources.is_empty() {
        return Err(WinxError::FileNotFound { path: base.to_path_buf() });
    }
    Ok(sources)
}

fn default_usage_path() -> PathBuf {
    std::env::var_os("WINX_USAGE_LOG").map_or_else(
        || {
            home::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state/winx/usage.jsonl")
        },
        PathBuf::from,
    )
}

fn parse_event(line: &str) -> Option<UsageEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    (value.get("target").and_then(Value::as_str) == Some("winx::usage")).then_some(())?;
    Some(UsageEvent {
        timestamp: value.get("timestamp").and_then(Value::as_str).map(ToString::to_string),
        fields: value.get("fields")?.as_object()?.clone(),
    })
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn latency(mut samples: Vec<u64>) -> LatencySummary {
    if samples.is_empty() {
        return LatencySummary::default();
    }
    samples.sort_unstable();
    let sum = samples.iter().fold(0_u128, |sum, value| sum.saturating_add(u128::from(*value)));
    let average = sum / samples.len() as u128;
    LatencySummary {
        samples: samples.len(),
        average_ms: u64::try_from(average).unwrap_or(u64::MAX),
        p50_ms: percentile(&samples, 50),
        p95_ms: percentile(&samples, 95),
        max_ms: samples.last().copied().unwrap_or_default(),
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted.len().saturating_mul(percentile).div_ceil(100).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn increment_nonempty(counts: &mut BTreeMap<String, u64>, value: &str) {
    if !value.is_empty() {
        counts.entry(value.to_string()).and_modify(|count| *count += 1).or_insert(1);
    }
}

fn string<'a>(fields: &'a serde_json::Map<String, Value>, key: &str) -> &'a str {
    fields.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn number(fields: &serde_json::Map<String, Value>, key: &str) -> u64 {
    fields.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn boolean(fields: &serde_json::Map<String, Value>, key: &str) -> bool {
    fields.get(key).and_then(Value::as_bool).unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), ToString::to_string)
}

fn i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write as _;

    use super::*;

    #[test]
    fn aggregates_correlated_trace_without_reading_sensitive_arguments() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        let path = temp.path().join("usage.jsonl.2026-08-25");
        let mut file = File::create(path)?;
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "timestamp": "2026-08-25T12:00:00Z",
                "target": "winx::usage",
                "fields": {
                    "event":"tool_call", "usage_schema":1,
                    "tool":"BashCommand", "request_id":"req",
                    "result_status":"completed", "duration_ms":10, "response_bytes":20,
                    "command_kind":"rust_toolchain", "build":"0.2.343+abc"
                }
            })
        )?;
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "timestamp": "2026-08-25T12:00:00Z",
                "target": "winx::usage",
                "fields": {
                    "event":"http_request", "request_id":"req", "status":200,
                    "duration_ms":12
                }
            })
        )?;
        let report = usage_report(Some(&base), 100, 0, None)?;
        assert_eq!(report.tool_calls, 1);
        assert_eq!(report.http_requests, 1);
        assert_eq!(report.correlated_requests, 1);
        assert_eq!(report.http_overhead.p95_ms, 2);
        assert_eq!(report.command_kinds["rust_toolchain"], 1);
        Ok(())
    }

    #[test]
    fn legacy_missing_recovery_fields_are_unknown_not_violations() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        let mut file = File::create(&base)?;
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "timestamp": "2026-08-25T12:00:00Z",
                "target": "winx::usage",
                "fields": {
                    "event": "tool_call",
                    "tool": "MultiFileEdit",
                    "request_id": "legacy",
                    "result_status": "conflict",
                    "error_code": "search_block_not_found"
                }
            })
        )?;

        let report = usage_report(Some(&base), 100, 0, None)?;
        assert!(report.trace_audit.healthy);
        assert_eq!(report.trace_audit.audited_tool_events, 0);
        assert_eq!(report.trace_audit.legacy_unverifiable_tool_events, 1);
        assert!(report.trace_audit.violations.is_empty());
        Ok(())
    }

    fn report_execution_fields(fields: &[Value]) -> Result<UsageReport> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("usage.jsonl");
        let mut file = File::create(&base)?;
        for (index, fields) in fields.iter().enumerate() {
            let mut fields = fields
                .as_object()
                .ok_or_else(|| {
                    WinxError::SerializationError("expected object fixture".to_string())
                })?
                .clone();
            fields.insert("event".into(), serde_json::json!("tool_call"));
            fields.insert("request_id".into(), serde_json::json!(format!("request-{index}")));
            fields.entry("usage_schema").or_insert(serde_json::json!(1));
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "timestamp": "2026-10-27T12:00:00Z",
                    "target": "winx::usage",
                    "fields": fields,
                })
            )?;
        }
        usage_report(Some(&base), 100, 0, None)
    }

    #[test]
    fn execution_summary_keeps_v1_metadata_unknown() -> Result<()> {
        let report = report_execution_fields(&[
            serde_json::json!({"tool":"BashCommand", "result_status":"completed", "outcome":"ok"}),
            serde_json::json!({"tool":"ReadFiles", "result_status":"completed"}),
            serde_json::json!({"tool":"EditFiles", "result_status":"completed"}),
        ])?;
        assert!(report.trace_audit.healthy);
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(execution["observations"], 1);
        assert_eq!(execution["exitCodeUnknown"], 1);
        assert_eq!(execution["exitCodes"], serde_json::json!({}));
        assert_eq!(execution["outputTruncationUnknown"], 1);
        assert_eq!(execution["outputNotTruncated"], 0);
        assert_eq!(execution["requestedWaitMs"]["samples"], 0);
        Ok(())
    }

    #[test]
    fn execution_summary_preserves_typed_exit_codes_and_truncation() -> Result<()> {
        let report = report_execution_fields(&[
            serde_json::json!({"tool":"BashCommand", "exit_code":0, "output_truncated":false}),
            serde_json::json!({"tool":"BashCommand", "exit_code":7, "output_truncated":true}),
            serde_json::json!({"tool":"BashCommand", "exit_code":-1, "output_truncated":false}),
            serde_json::json!({"tool":"BashCommand", "exit_code":null, "output_truncated":null}),
            serde_json::json!({"tool":"BashCommand", "exit_code":"0", "output_truncated":"false"}),
        ])?;
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(execution["exitCodes"], serde_json::json!({"-1":1, "0":1, "7":1}));
        assert_eq!(execution["exitCodeUnknown"], 2);
        assert_eq!(execution["outputTruncated"], 1);
        assert_eq!(execution["outputNotTruncated"], 2);
        assert_eq!(execution["outputTruncationUnknown"], 2);
        Ok(())
    }

    #[test]
    fn execution_summary_includes_flattened_edit_verification() -> Result<()> {
        let report = report_execution_fields(&[serde_json::json!({
            "tool":"EditFiles", "result_status":"completed_with_issues",
            "verification_status":"failed", "error_code":"verification_failed",
            "exit_code":3, "output_truncated":false,
            "execution_id":"exec-fingerprint", "verification_id":"verify-fingerprint",
            "mutation_receipt_id":"receipt-fingerprint"
        })])?;
        assert_eq!(report.tools["EditFiles"].error_results, 1);
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(execution["observations"], 1);
        assert_eq!(execution["exitCodes"], serde_json::json!({"3":1}));
        assert_eq!(execution["callsWithExecutionId"], 1);
        assert_eq!(execution["callsWithVerificationId"], 1);
        assert_eq!(execution["callsWithMutationReceiptId"], 1);
        Ok(())
    }

    #[test]
    fn execution_summary_replay_does_not_invent_zero_exit() -> Result<()> {
        let report = report_execution_fields(&[serde_json::json!({
            "tool":"VerifyEdit", "result_status":"completed", "outcome":"ok",
            "verification_id":"verify-fingerprint", "verification_passed":true,
            "verification_replayed":true
        })])?;
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(execution["observations"], 1);
        assert_eq!(execution["verificationReplays"], 1);
        assert_eq!(execution["exitCodeUnknown"], 1);
        assert_eq!(execution["exitCodes"], serde_json::json!({}));
        Ok(())
    }

    #[test]
    fn execution_summary_counts_polls_as_observations() -> Result<()> {
        let report = report_execution_fields(&[
            serde_json::json!({"tool":"BashCommand", "action":"command", "result_status":"running", "execution_id":"same-execution", "bg_command_id":"same-background"}),
            serde_json::json!({"tool":"BashCommand", "action":"status_check", "result_status":"running", "execution_id":"same-execution", "bg_command_id":"same-background"}),
            serde_json::json!({"tool":"BashCommand", "action":"status_check", "result_status":"completed", "execution_id":"same-execution", "bg_command_id":"same-background", "exit_code":0}),
        ])?;
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(execution["observations"], 3);
        assert_eq!(execution["callsWithExecutionId"], 3);
        assert_eq!(execution["callsWithBackgroundId"], 3);
        assert_eq!(execution["exitCodes"], serde_json::json!({"0":1}));
        assert_eq!(execution["exitCodeUnknown"], 2);
        Ok(())
    }

    #[test]
    fn execution_summary_records_requested_wait_without_sensitive_content() -> Result<()> {
        let report = report_execution_fields(&[
            serde_json::json!({
                "tool":"BashCommand", "requested_wait_policy":"return_early", "requested_wait_ms":0,
                "duration_ms":17, "arguments":{"command":"SECRET-COMMAND"}, "output":"SECRET-OUTPUT"
            }),
            serde_json::json!({"tool":"BashCommand", "requested_wait_policy":"until_complete", "requested_wait_ms":5000, "duration_ms":17}),
            serde_json::json!({"tool":"BashCommand", "requested_wait_policy":"SECRET-POLICY", "requested_wait_ms":"5000", "duration_ms":17}),
        ])?;
        assert_eq!(report.latency.max_ms, 17);
        let value = serde_json::to_value(report)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        let execution = &value["execution"];
        assert_eq!(
            execution["requestedWaitPolicies"],
            serde_json::json!({"return_early":1,"until_complete":1})
        );
        assert_eq!(execution["requestedWaitPolicyUnknown"], 1);
        assert_eq!(execution["requestedWaitMs"]["samples"], 2);
        assert_eq!(execution["requestedWaitMs"]["averageMs"], 2500);
        assert_eq!(execution["requestedWaitMs"]["maxMs"], 5000);
        let serialized = value.to_string();
        assert!(!serialized.contains("SECRET-COMMAND"));
        assert!(!serialized.contains("SECRET-OUTPUT"));
        assert!(!serialized.contains("SECRET-POLICY"));
        Ok(())
    }
}
