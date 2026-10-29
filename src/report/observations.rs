//! Call-level request and response counts, never paths or output contents.
//! Repeated reads and observations count repeatedly. Unknown means unavailable
//! or not applicable, never a measured zero or an execution failure.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::{increment_nonempty, latency, number, string, LatencySummary};

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadUsage {
    pub calls: u64,
    pub classified_calls: u64,
    pub unknown_calls: u64,
    pub whole_file_requests: u64,
    pub bounded_range_requests: u64,
    pub open_ended_range_requests: u64,
    pub response_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveUsage {
    pub calls: u64,
    pub request_counts_known: u64,
    pub request_counts_unknown: u64,
    pub requested_items: u64,
    pub result_counts_known: u64,
    pub result_counts_unknown: u64,
    pub returned_items: u64,
    pub items_with_issues: u64,
    /// Calls reporting `completed_with_issues`, not failed executions.
    pub partial_results: u64,
}

/// Flattened into `ExecutionUsage`; its denominator is only `BashCommand` calls.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveWaitUsage {
    pub effective_wait_observations: u64,
    pub effective_wait_policies: BTreeMap<String, u64>,
    pub effective_wait_policy_unknown: u64,
    pub effective_wait_modes: BTreeMap<String, u64>,
    pub effective_wait_mode_unknown: u64,
    pub effective_wait_ms: LatencySummary,
}

#[derive(Default)]
pub(super) struct ObservationAccumulator {
    reads: ReadUsage,
    observations: ObserveUsage,
    effective_wait: EffectiveWaitUsage,
    effective_wait_samples: Vec<u64>,
}

impl ObservationAccumulator {
    pub fn record(&mut self, fields: &Map<String, Value>) {
        match string(fields, "tool") {
            "ReadFiles" => self.record_reads(fields),
            "Execution"
                if optional_number(fields, "workflow_schema") == Some(1)
                    && string(fields, "action") == "observe" =>
            {
                self.record_observe(fields);
            }
            "BashCommand" => self.record_effective_wait(fields),
            _ => {}
        }
    }

    pub fn finish(mut self) -> (ReadUsage, ObserveUsage, EffectiveWaitUsage) {
        self.effective_wait.effective_wait_ms = latency(self.effective_wait_samples);
        (self.reads, self.observations, self.effective_wait)
    }

    fn record_reads(&mut self, fields: &Map<String, Value>) {
        let reads = &mut self.reads;
        reads.calls = reads.calls.saturating_add(1);
        reads.response_bytes =
            reads.response_bytes.saturating_add(number(fields, "response_bytes"));
        let counts = (
            optional_number(fields, "read_whole_file_requests"),
            optional_number(fields, "read_bounded_range_requests"),
            optional_number(fields, "read_open_ended_range_requests"),
        );
        if let (Some(whole), Some(bounded), Some(open)) = counts {
            if optional_number(fields, "read_request_schema") == Some(1) {
                reads.classified_calls = reads.classified_calls.saturating_add(1);
                reads.whole_file_requests = reads.whole_file_requests.saturating_add(whole);
                reads.bounded_range_requests = reads.bounded_range_requests.saturating_add(bounded);
                reads.open_ended_range_requests =
                    reads.open_ended_range_requests.saturating_add(open);
                return;
            }
        }
        reads.unknown_calls = reads.unknown_calls.saturating_add(1);
    }

    fn record_observe(&mut self, fields: &Map<String, Value>) {
        let observed = &mut self.observations;
        observed.calls = observed.calls.saturating_add(1);
        observed.partial_results = observed
            .partial_results
            .saturating_add(u64::from(string(fields, "result_status") == "completed_with_issues"));
        let classified = optional_number(fields, "observe_schema") == Some(1);
        // Old batch_items=0 did not describe real observe batches. Never use it
        // as a fallback for the independently versioned request counters.
        match optional_number(fields, "observe_requested_items").filter(|_| classified) {
            Some(count) => {
                observed.request_counts_known = observed.request_counts_known.saturating_add(1);
                observed.requested_items = observed.requested_items.saturating_add(count);
            }
            None => {
                observed.request_counts_unknown = observed.request_counts_unknown.saturating_add(1);
            }
        }
        let returned = optional_number(fields, "observe_returned_items").filter(|_| classified);
        let issues = optional_number(fields, "observe_items_with_issues").filter(|_| classified);
        if let (Some(returned), Some(issues)) = (returned, issues) {
            if issues <= returned {
                observed.result_counts_known = observed.result_counts_known.saturating_add(1);
                observed.returned_items = observed.returned_items.saturating_add(returned);
                observed.items_with_issues = observed.items_with_issues.saturating_add(issues);
                return;
            }
        }
        observed.result_counts_unknown = observed.result_counts_unknown.saturating_add(1);
    }

    fn record_effective_wait(&mut self, fields: &Map<String, Value>) {
        let wait = &mut self.effective_wait;
        wait.effective_wait_observations = wait.effective_wait_observations.saturating_add(1);
        let policy = match string(fields, "effective_wait_policy") {
            policy @ ("adaptive" | "until_complete" | "return_early") => Some(policy),
            _ => None,
        };
        let mode = match string(fields, "effective_wait_mode") {
            mode @ ("synchronous" | "task_inline" | "task") => Some(mode),
            _ => None,
        };
        let milliseconds = optional_number(fields, "effective_wait_ms")
            .filter(|milliseconds| *milliseconds <= 60_000)
            .filter(|milliseconds| mode != Some("task") || *milliseconds == 0);
        if let (Some(policy), Some(mode), Some(milliseconds)) = (policy, mode, milliseconds) {
            increment_nonempty(&mut wait.effective_wait_policies, policy);
            increment_nonempty(&mut wait.effective_wait_modes, mode);
            self.effective_wait_samples.push(milliseconds);
        } else {
            // Includes legacy calls and actions without applicable metadata;
            // these unknown observations are not failed executions.
            wait.effective_wait_policy_unknown =
                wait.effective_wait_policy_unknown.saturating_add(1);
            wait.effective_wait_mode_unknown = wait.effective_wait_mode_unknown.saturating_add(1);
        }
    }
}

fn optional_number(fields: &Map<String, Value>, key: &str) -> Option<u64> {
    fields.get(key).and_then(Value::as_u64)
}
