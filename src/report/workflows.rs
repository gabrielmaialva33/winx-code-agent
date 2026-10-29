//! Workflow call and receipt observations. Repeated receipt reads count repeatedly;
//! these aggregates make no claim about unique executions, deliveries or completion.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::lifecycle::{LifecycleAccumulator, WorkflowPhaseUsage};
use super::{increment_nonempty, latency, number, string, LatencySummary};

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUsage {
    pub calls: u64,
    pub operations: Vec<WorkflowOperationUsage>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOperationUsage {
    pub tool: &'static str,
    pub action: &'static str,
    pub backend: &'static str,
    pub calls: u64,
    pub error_results: u64,
    pub response_bytes: u64,
    pub latency: LatencySummary,
    pub requested_wait_ms: LatencySummary,
    pub wait_kinds: BTreeMap<String, u64>,
    pub wait_outcomes: BTreeMap<String, u64>,
    pub wait_outcome_unknown: u64,
    /// Observed receipts, including repeated observations of the same receipt.
    pub receipt_observations: u64,
    pub prompt_delivery: BTreeMap<String, u64>,
    pub prompt_delivery_unknown: u64,
    pub prompt_outcome: BTreeMap<String, u64>,
    pub prompt_outcome_unknown: u64,
    pub prompt_activity_observed_true: u64,
    pub prompt_activity_observed_false: u64,
    pub prompt_activity_observed_unknown: u64,
    pub agent_submission_accepted: u64,
    pub agent_input_sent_true: u64,
    pub agent_input_sent_false: u64,
    pub agent_input_may_have_been_sent_true: u64,
    pub agent_input_may_have_been_sent_false: u64,
    pub phases: BTreeMap<String, WorkflowPhaseUsage>,
    pub phase_abandoned_count: u64,
    pub phase_trace_suppressed_count: u64,
}

#[derive(Default)]
pub(super) struct WorkflowAccumulator {
    calls: u64,
    operations: BTreeMap<(&'static str, &'static str, &'static str), OperationAccumulator>,
}

#[derive(Default)]
struct OperationAccumulator {
    usage: WorkflowOperationUsage,
    durations: Vec<u64>,
    requested_wait: Vec<u64>,
    lifecycle: LifecycleAccumulator,
}

impl WorkflowAccumulator {
    pub fn record(&mut self, fields: &Map<String, Value>) {
        let tool = match string(fields, "tool") {
            "Agent" => "Agent",
            "Execution" => "Execution",
            _ => return,
        };
        let classified = fields.get("workflow_schema").and_then(Value::as_u64) == Some(1);
        let action = if classified { action(tool, string(fields, "action")) } else { "unknown" };
        let backend =
            if classified { backend(string(fields, "workflow_backend")) } else { "unknown" };
        self.calls = self.calls.saturating_add(1);
        let operation = self.operations.entry((tool, action, backend)).or_insert_with(|| {
            OperationAccumulator {
                usage: WorkflowOperationUsage { tool, action, backend, ..Default::default() },
                ..Default::default()
            }
        });
        operation.usage.calls = operation.usage.calls.saturating_add(1);
        operation.usage.error_results = operation
            .usage
            .error_results
            .saturating_add(u64::from(!string(fields, "error_code").is_empty()));
        operation.usage.response_bytes =
            operation.usage.response_bytes.saturating_add(number(fields, "response_bytes"));
        if let Some(duration) = fields.get("duration_ms").and_then(Value::as_u64) {
            operation.durations.push(duration);
        }
        if !classified {
            return;
        }
        if let Some(wait) =
            fields.get("requested_wait_ms").and_then(Value::as_u64).filter(|wait| *wait <= 30_000)
        {
            operation.requested_wait.push(wait);
        }
        operation.lifecycle.record(fields);
        operation.usage.record_wait(fields);
        operation.usage.record_receipt(fields);
        operation.usage.record_agent_input(fields);
    }

    pub fn finish(self) -> WorkflowUsage {
        let operations = self
            .operations
            .into_values()
            .map(|operation| {
                let mut usage = operation.usage;
                usage.latency = latency(operation.durations);
                usage.requested_wait_ms = latency(operation.requested_wait);
                usage.phase_abandoned_count = operation.lifecycle.abandoned;
                usage.phase_trace_suppressed_count = operation.lifecycle.suppressed;
                usage.phases = operation.lifecycle.finish();
                usage
            })
            .collect();
        WorkflowUsage { calls: self.calls, operations }
    }
}

impl WorkflowOperationUsage {
    fn record_wait(&mut self, fields: &Map<String, Value>) {
        let kind = match string(fields, "workflow_wait_kind") {
            "catalog" => "catalog",
            "receipt" => "receipt",
            "herdr" => "herdr",
            _ => return,
        };
        increment_nonempty(&mut self.wait_kinds, kind);
        if !matches!(kind, "catalog" | "herdr") {
            return;
        }
        let outcome = if kind == "catalog" {
            match string(fields, "workflow_wait_outcome") {
                "changed" => Some("changed"),
                "timed_out" => Some("timed_out"),
                "resync_required" => Some("resync_required"),
                _ => None,
            }
        } else {
            None
        };
        match outcome {
            Some(outcome) => increment_nonempty(&mut self.wait_outcomes, outcome),
            None => self.wait_outcome_unknown = self.wait_outcome_unknown.saturating_add(1),
        }
    }

    fn record_receipt(&mut self, fields: &Map<String, Value>) {
        if string(fields, "workflow_wait_kind") != "receipt"
            || fields.get("prompt_receipt_present").and_then(Value::as_bool) != Some(true)
        {
            return;
        }
        self.receipt_observations = self.receipt_observations.saturating_add(1);
        match string(fields, "prompt_delivery") {
            delivery @ ("delivered" | "uncertain") => {
                increment_nonempty(&mut self.prompt_delivery, delivery);
            }
            _ => self.prompt_delivery_unknown = self.prompt_delivery_unknown.saturating_add(1),
        }
        match string(fields, "prompt_outcome") {
            outcome @ ("waiting" | "awaiting_input" | "awaiting_approval" | "exited"
            | "timed_out" | "generation_changed") => {
                increment_nonempty(&mut self.prompt_outcome, outcome);
            }
            _ => self.prompt_outcome_unknown = self.prompt_outcome_unknown.saturating_add(1),
        }
        match fields.get("prompt_activity_observed").and_then(Value::as_bool) {
            Some(true) => {
                self.prompt_activity_observed_true =
                    self.prompt_activity_observed_true.saturating_add(1);
            }
            Some(false) => {
                self.prompt_activity_observed_false =
                    self.prompt_activity_observed_false.saturating_add(1);
            }
            None => {
                self.prompt_activity_observed_unknown =
                    self.prompt_activity_observed_unknown.saturating_add(1);
            }
        }
    }

    fn record_agent_input(&mut self, fields: &Map<String, Value>) {
        if self.tool != "Agent" {
            return;
        }
        if self.backend == "herdr"
            && self.action == "prompt"
            && string(fields, "agent_submission_status") == "accepted"
        {
            self.agent_submission_accepted = self.agent_submission_accepted.saturating_add(1);
        }
        match fields.get("agent_input_sent").and_then(Value::as_bool) {
            Some(true) => self.agent_input_sent_true = self.agent_input_sent_true.saturating_add(1),
            Some(false) => {
                self.agent_input_sent_false = self.agent_input_sent_false.saturating_add(1);
            }
            None => {}
        }
        match fields.get("agent_input_may_have_been_sent").and_then(Value::as_bool) {
            Some(true) => {
                self.agent_input_may_have_been_sent_true =
                    self.agent_input_may_have_been_sent_true.saturating_add(1);
            }
            Some(false) => {
                self.agent_input_may_have_been_sent_false =
                    self.agent_input_may_have_been_sent_false.saturating_add(1);
            }
            None => {}
        }
    }
}

fn action(tool: &str, value: &str) -> &'static str {
    match value {
        "list" => "list",
        "get" => "get",
        "read" => "read",
        "observe" if tool == "Execution" => "observe",
        "explain" => "explain",
        "wait" => "wait",
        "prompt" => "prompt",
        _ => "unknown",
    }
}

fn backend(value: &str) -> &'static str {
    match value {
        "native" => "native",
        "herdr" => "herdr",
        _ => "unknown",
    }
}

#[cfg(test)]
mod observe_tests {
    use anyhow::Context;
    use serde_json::json;

    use super::*;

    #[test]
    fn observe_report_preserves_action_and_counts_partial_results() -> anyhow::Result<()> {
        let mut accumulator = WorkflowAccumulator::default();
        for (status, error_code, bytes) in
            [("completed", "", 512), ("completed_with_issues", "execution_observation_issues", 768)]
        {
            let fields = json!({
                "usage_schema":1,"workflow_schema":1,"tool":"Execution","action":"observe",
                "workflow_backend":"native","status":status,"error_code":error_code,
                "duration_ms":12,"response_bytes":bytes,
                "data":{"cursor":"SECRET-CURSOR","items":[{
                    "execution_id":"SECRET-EXECUTION","page":{"text":"SECRET-OUTPUT"},
                    "errors":[{"message":"SECRET-ERROR"}]
                }]}
            });
            accumulator.record(fields.as_object().context("usage fields")?);
        }
        let report = accumulator.finish();
        assert_eq!(report.calls, 2);
        assert_eq!(report.operations.len(), 1);
        let operation = report.operations.first().context("observe operation")?;
        assert_eq!(operation.tool, "Execution");
        assert_eq!(operation.action, "observe");
        assert_eq!(operation.backend, "native");
        assert_eq!(operation.calls, 2);
        assert_eq!(operation.error_results, 1);
        assert_eq!(operation.response_bytes, 1280);
        assert_eq!(
            serde_json::to_value(&operation.requested_wait_ms)?,
            serde_json::to_value(latency(Vec::new()))?
        );
        assert!(operation.wait_kinds.is_empty());
        assert!(operation.wait_outcomes.is_empty());
        assert_eq!(operation.wait_outcome_unknown, 0);
        assert_eq!(operation.receipt_observations, 0);
        assert!(operation.prompt_delivery.is_empty());
        assert!(operation.prompt_outcome.is_empty());
        assert_eq!(operation.agent_submission_accepted, 0);
        assert_eq!(operation.agent_input_sent_true, 0);
        assert_eq!(operation.agent_input_may_have_been_sent_true, 0);
        assert!(!serde_json::to_string(&report)?.contains("SECRET-"));
        Ok(())
    }

    #[test]
    fn observe_report_keeps_agent_and_unversioned_actions_unknown() -> anyhow::Result<()> {
        for (tool, schema, requested_backend, expected_backend) in [
            ("Agent", Some(1), "native", "native"),
            ("Agent", Some(1), "herdr", "herdr"),
            ("Execution", None, "native", "unknown"),
        ] {
            let fields = json!({
                "usage_schema":1,"workflow_schema":schema,"tool":tool,"action":"observe",
                "workflow_backend":requested_backend,"duration_ms":9,"response_bytes":42
            });
            let mut accumulator = WorkflowAccumulator::default();
            accumulator.record(fields.as_object().context("usage fields")?);
            let report = accumulator.finish();
            assert_eq!(report.calls, 1);
            assert_eq!(report.operations.len(), 1);
            let operation = report.operations.first().context("unknown operation")?;
            assert_eq!(operation.tool, tool);
            assert_eq!(operation.action, "unknown", "{tool} {schema:?}");
            assert_eq!(operation.backend, expected_backend);
            assert_eq!(operation.calls, 1);
            assert_eq!(operation.response_bytes, 42);
        }
        Ok(())
    }
}
