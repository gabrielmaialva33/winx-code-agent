//! Finite request classifications and response observations, never workflow contents.

use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) struct UsageWorkflowRequestMetadata {
    pub action: &'static str,
    pub backend: &'static str,
    pub wait_kind: Option<&'static str>,
    pub requested_wait_ms: Option<u64>,
}

#[derive(Default)]
pub(super) struct UsageWorkflowResultMetadata {
    pub wait_outcome: Option<&'static str>,
    pub receipt_present: Option<bool>,
    pub delivery: Option<&'static str>,
    pub activity_observed: Option<bool>,
    pub prompt_outcome: Option<&'static str>,
    pub submission_status: Option<&'static str>,
    pub input_sent: Option<bool>,
    pub input_may_have_been_sent: Option<bool>,
}

impl UsageWorkflowRequestMetadata {
    pub fn from_request(tool: &str, arguments: Option<&JsonObject>) -> Option<Self> {
        if !matches!(tool, "Agent" | "Execution") {
            return None;
        }
        let argument = |key| arguments.and_then(|arguments| arguments.get(key));
        let action_value = argument("action");
        let action = if tool == "Execution" {
            action_value.and_then(|action| action.get("type"))
        } else {
            action_value
        };
        let action = match action.and_then(Value::as_str) {
            Some("list") => "list",
            Some("get") => "get",
            Some("read") => "read",
            Some("observe") if tool == "Execution" => "observe",
            Some("explain") => "explain",
            Some("wait") => "wait",
            Some("prompt") if tool == "Agent" => "prompt",
            _ => "unknown",
        };
        let backend = if tool == "Execution" {
            "native"
        } else {
            match argument("backend") {
                None => "native",
                Some(Value::String(backend)) if backend == "native" => "native",
                Some(Value::String(backend)) if backend == "herdr" => "herdr",
                _ => "unknown",
            }
        };
        let wait_kind = match (backend, action) {
            ("native", "wait") if tool == "Execution" => Some("catalog"),
            ("native", "wait") if argument("prompt_id").and_then(Value::as_str).is_none() => {
                Some("catalog")
            }
            ("native", "wait" | "prompt") => Some("receipt"),
            ("herdr", "wait") => Some("herdr"),
            _ => None,
        };
        let timeout = if tool == "Execution" {
            action_value.and_then(|action| action.get("timeout_seconds"))
        } else {
            argument("timeout_seconds")
        };
        let requested_wait_ms = wait_kind.and_then(|_| requested_timeout_ms(timeout));
        Some(Self { action, backend, wait_kind, requested_wait_ms })
    }

    pub fn result(
        self,
        tool: &str,
        result: Option<&CallToolResult>,
    ) -> UsageWorkflowResultMetadata {
        let data = result
            .and_then(|result| result.structured_content.as_ref())
            .and_then(|envelope| envelope.get("data"));
        let mut metadata = UsageWorkflowResultMetadata::default();
        if self.wait_kind == Some("catalog") {
            metadata.wait_outcome = catalog_outcome(data);
        }
        if self.wait_kind == Some("receipt") && result.is_some() {
            let receipt = data.and_then(|data| data.get("prompt_receipt"));
            metadata.receipt_present = Some(receipt.is_some_and(recognized_receipt));
            if let Some(receipt) = receipt.filter(|receipt| recognized_receipt(receipt)) {
                metadata.delivery = delivery(receipt.get("delivery"));
                metadata.activity_observed =
                    receipt.get("activity_observed").and_then(Value::as_bool);
                metadata.prompt_outcome = prompt_outcome(receipt.get("outcome"));
            }
        }
        if tool == "Agent" && matches!(self.backend, "native" | "herdr") {
            metadata.input_sent =
                data.and_then(|data| data.get("input_sent")).and_then(Value::as_bool);
            metadata.input_may_have_been_sent =
                data.and_then(|data| data.get("input_may_have_been_sent")).and_then(Value::as_bool);
            if self.backend == "herdr"
                && self.action == "prompt"
                && data.and_then(|data| data.get("submission_status")).and_then(Value::as_str)
                    == Some("accepted")
            {
                metadata.submission_status = Some("accepted");
            }
        }
        metadata
    }
}

fn requested_timeout_ms(value: Option<&Value>) -> Option<u64> {
    // Deserialize as f32 to match the typed request's rounding and finite bounds.
    let seconds = serde_json::from_value::<f32>(value?.clone()).ok()?;
    if !seconds.is_finite() || !(0.0..=30.0).contains(&seconds) {
        return None;
    }
    let duration = std::time::Duration::try_from_secs_f32(seconds).ok()?;
    u64::try_from(duration.as_millis()).ok()
}

fn catalog_outcome(data: Option<&Value>) -> Option<&'static str> {
    let data = data?;
    let changes = data.get("changes")?;
    let resync = changes.get("resync_required")?.as_bool()?;
    let executions = changes.get("executions")?.as_array()?;
    if resync {
        return Some("resync_required");
    }
    match data.get("timed_out")?.as_bool()? {
        true => Some("timed_out"),
        false if !executions.is_empty() => Some("changed"),
        false => None,
    }
}

fn recognized_receipt(receipt: &Value) -> bool {
    receipt.get("prompt_id").and_then(Value::as_str).is_some()
        && delivery(receipt.get("delivery")).is_some()
        && receipt.get("activity_observed").and_then(Value::as_bool).is_some()
        && prompt_outcome(receipt.get("outcome")).is_some()
        && receipt.get("generation").and_then(Value::as_u64).is_some()
        && receipt.get("baseline_activity_seq").and_then(Value::as_u64).is_some()
}

fn delivery(value: Option<&Value>) -> Option<&'static str> {
    match value.and_then(Value::as_str)? {
        "delivered" => Some("delivered"),
        "uncertain" => Some("uncertain"),
        _ => None,
    }
}

fn prompt_outcome(value: Option<&Value>) -> Option<&'static str> {
    match value.and_then(Value::as_str)? {
        "waiting" => Some("waiting"),
        "awaiting_input" => Some("awaiting_input"),
        "awaiting_approval" => Some("awaiting_approval"),
        "exited" => Some("exited"),
        "timed_out" => Some("timed_out"),
        "generation_changed" => Some("generation_changed"),
        _ => None,
    }
}
