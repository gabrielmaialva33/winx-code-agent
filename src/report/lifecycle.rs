//! Inclusive phase totals: one duration sample per measured call, not per phase
//! invocation. Phases may overlap, nest and repeat; their sum is not overhead.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::{latency, LatencySummary};

const PHASE_FIELDS: [(&str, &str, &str); 10] = [
    ("workspace_check", "phase_workspace_check_ms", "phase_workspace_check_calls"),
    ("session_admission", "phase_session_admission_ms", "phase_session_admission_calls"),
    ("catalog", "phase_catalog_ms", "phase_catalog_calls"),
    ("runtime_inspect", "phase_runtime_inspect_ms", "phase_runtime_inspect_calls"),
    ("runtime_capture", "phase_runtime_capture_ms", "phase_runtime_capture_calls"),
    ("prompt_rpc", "phase_prompt_rpc_ms", "phase_prompt_rpc_calls"),
    ("herdr_adapter", "phase_herdr_adapter_ms", "phase_herdr_adapter_calls"),
    ("wait_idle", "phase_wait_idle_ms", "phase_wait_idle_calls"),
    ("temp_audit", "phase_temp_audit_ms", "phase_temp_audit_calls"),
    ("response_finalize", "phase_response_finalize_ms", "phase_response_finalize_calls"),
];

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPhaseUsage {
    pub calls: u64,
    pub latency: LatencySummary,
}

#[derive(Default)]
struct PhaseAccumulator {
    calls: u64,
    durations: Vec<u64>,
}

#[derive(Default)]
pub(super) struct LifecycleAccumulator {
    phases: BTreeMap<&'static str, PhaseAccumulator>,
    pub abandoned: u64,
    pub suppressed: u64,
}

impl LifecycleAccumulator {
    pub fn record(&mut self, fields: &Map<String, Value>) {
        for (name, duration_key, calls_key) in PHASE_FIELDS {
            let Some(calls) =
                fields.get(calls_key).and_then(Value::as_u64).filter(|calls| *calls > 0)
            else {
                continue;
            };
            let phase = self.phases.entry(name).or_default();
            phase.calls = phase.calls.saturating_add(calls);
            if let Some(duration) = fields.get(duration_key).and_then(Value::as_u64) {
                phase.durations.push(duration);
            }
        }
        self.abandoned = self.abandoned.saturating_add(
            fields.get("phase_abandoned_count").and_then(Value::as_u64).unwrap_or(0),
        );
        self.suppressed = self.suppressed.saturating_add(
            fields.get("phase_trace_suppressed_count").and_then(Value::as_u64).unwrap_or(0),
        );
    }

    pub fn finish(self) -> BTreeMap<String, WorkflowPhaseUsage> {
        self.phases
            .into_iter()
            .map(|(name, phase)| {
                (
                    name.to_string(),
                    WorkflowPhaseUsage { calls: phase.calls, latency: latency(phase.durations) },
                )
            })
            .collect()
    }
}
