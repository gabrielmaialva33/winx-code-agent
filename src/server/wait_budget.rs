//! Per-client wait budgets for remote (HTTP) clients.
//!
//! Every `running` result costs a remote model a full turn (about 25 s for
//! `ChatGPT`), which dwarfs the server-side cost of holding the request open.
//! A budget therefore lets one call wait for completion as long as the client
//! can safely keep a request open, and returns as soon as the command finishes
//! or is waiting for input. Budgets only select tuning from the self-reported
//! client name; they never grant authority, and stdio/local sessions and
//! unknown clients keep the unbudgeted behaviour.

use std::future::Future;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};

use super::client_profile::ClientProfile;
use crate::types::{BashCommand, BashCommandAction, BashWaitPolicy};

/// Requested waits at or below this are "return early" waits; promoting
/// budgets treat them as adaptive waits up to the full budget.
pub(super) const SHORT_WAIT_SECONDS: f32 = 5.0;
/// Execution wait ceiling for unbudgeted clients (the public schema range).
pub(super) const DEFAULT_EXECUTION_WAIT_MAX_SECONDS: f32 = 30.0;
/// Headroom between the longest budget and the HTTP request timeout for
/// session admission, guardian round trips and response serialization.
const REQUEST_SAFETY_MARGIN_SECONDS: u64 = 20;
const MAX_BUDGET_SECONDS: u64 =
    crate::http_server::REQUEST_TIMEOUT.as_secs() - REQUEST_SAFETY_MARGIN_SECONDS;
/// Lifecycle events an Execution wait defaults to under a promoting budget, so
/// output chatter does not end the wait early.
const LIFECYCLE_EVENTS: [&str; 4] = ["exited", "awaiting_input", "awaiting_approval", "unknown"];

/// Wait tuning for one client family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WaitBudget {
    /// Longest one foreground `BashCommand` command or `status_check` may block.
    pub sync_seconds: u16,
    /// Wait used when an adaptive call omits `wait_for_seconds`.
    pub adaptive_default_seconds: u16,
    /// Treat `return_early` and requested waits up to [`SHORT_WAIT_SECONDS`]
    /// as adaptive waits up to `sync_seconds` (and Execution waits up to
    /// `execution_wait_seconds`).
    pub promote_short_waits: bool,
    /// Longest one `Execution` wait may block.
    pub execution_wait_seconds: u16,
}

// ChatGPT re-sends an identical call 22-26% of the time once it takes 60 s or
// longer (1-2% below), and supports neither Tasks nor progress notifications:
// stay 10 s under that retry zone.
const CHATGPT: WaitBudget = WaitBudget {
    sync_seconds: 50,
    adaptive_default_seconds: 50,
    promote_short_waits: true,
    execution_wait_seconds: 50,
};
// Codex's MCP `tool_timeout_sec` defaults to 60 s; keep the same 10 s margin.
const CODEX: WaitBudget = CHATGPT;
// claude.ai allows 240 s per call, but the Cloudflare tunnel in front of the
// HTTP transport drops idle reads after 100-125 s: stay under the low end.
// Claude already chooses its own waits well, so short waits are honoured.
const CLAUDE: WaitBudget = WaitBudget {
    sync_seconds: 90,
    adaptive_default_seconds: 15,
    promote_short_waits: false,
    execution_wait_seconds: 90,
};

const _: () = {
    let budgets = [CHATGPT, CODEX, CLAUDE];
    let mut index = 0;
    while index < budgets.len() {
        let budget = budgets[index];
        assert!(budget.sync_seconds as u64 <= MAX_BUDGET_SECONDS);
        assert!(budget.execution_wait_seconds as u64 <= MAX_BUDGET_SECONDS);
        assert!(budget.adaptive_default_seconds <= budget.sync_seconds);
        index += 1;
    }
};

tokio::task_local! {
    static EXECUTION_WAIT_CEILING: f32;
}

impl WaitBudget {
    /// The single budget table. `None` keeps the unbudgeted behaviour.
    pub(super) const fn for_profile(profile: ClientProfile) -> Option<Self> {
        match profile {
            ClientProfile::ChatGpt => Some(CHATGPT),
            ClientProfile::Codex => Some(CODEX),
            ClientProfile::Claude => Some(CLAUDE),
            ClientProfile::Other => None,
        }
    }

    /// Budgets apply to the HTTP transport only; stdio requests carry no HTTP
    /// request parts.
    pub(super) fn from_context(context: &RequestContext<RoleServer>) -> Option<Self> {
        context.extensions.get::<axum::http::request::Parts>()?;
        Self::for_profile(ClientProfile::from_context(context))
    }

    /// Effective policy and wait for a foreground command or `status_check`,
    /// or `None` to keep the unbudgeted normalization.
    pub(super) fn bash_wait(
        self,
        policy: BashWaitPolicy,
        requested: Option<f32>,
    ) -> Option<(BashWaitPolicy, f32)> {
        let cap = f32::from(self.sync_seconds);
        let requested = requested.map(|seconds| seconds.max(0.0));
        let promoted =
            if policy == BashWaitPolicy::ReturnEarly { BashWaitPolicy::Adaptive } else { policy };
        match (policy, requested) {
            (BashWaitPolicy::ReturnEarly, _) if !self.promote_short_waits => None,
            (_, Some(seconds)) if seconds > SHORT_WAIT_SECONDS || !self.promote_short_waits => {
                Some((policy, seconds.min(cap)))
            }
            (BashWaitPolicy::ReturnEarly, None) | (_, Some(_)) => Some((promoted, cap)),
            (BashWaitPolicy::Adaptive, None) => {
                Some((policy, f32::from(self.adaptive_default_seconds)))
            }
            (BashWaitPolicy::UntilComplete, None) => Some((policy, cap)),
        }
    }

    /// Budgeted `Execution` wait timeout, or `None` to leave the request as
    /// sent. An explicit zero stays a catalog poll and invalid values still
    /// reach the validator.
    pub(super) fn execution_wait_timeout(self, requested: Option<f32>) -> Option<f32> {
        let cap = f32::from(self.execution_wait_seconds);
        match requested {
            Some(seconds) if seconds.is_nan() || seconds <= 0.0 => None,
            _ if self.promote_short_waits => Some(cap),
            None => None,
            Some(seconds) => Some(seconds.min(cap)),
        }
    }

    /// Rewrite a budget-eligible `BashCommand` request with its effective wait.
    /// Returns `None` for actions that keep the unbudgeted normalization:
    /// background launches and status checks (a background server never
    /// finishes, so a peek must stay quick), interactive input and verification.
    pub(super) fn normalize_bash_request(
        self,
        request: &CallToolRequestParams,
        bash: Option<&BashCommand>,
        policy: BashWaitPolicy,
    ) -> Result<Option<(CallToolRequestParams, BashWaitPolicy)>, McpError> {
        let eligible = bash.is_some_and(|bash| {
            matches!(
                bash.action_json,
                BashCommandAction::Command { is_background: false, .. }
                    | BashCommandAction::StatusCheck { bg_command_id: None, .. }
            )
        });
        if !eligible {
            return Ok(None);
        }
        let mut arguments = request
            .arguments
            .clone()
            .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
        let requested = arguments
            .get("wait_for_seconds")
            .cloned()
            .map(serde_json::from_value::<f32>)
            .transpose()
            .map_err(|error| {
                McpError::invalid_request(format!("Invalid wait_for_seconds: {error}"), None)
            })?;
        let Some((effective, seconds)) = self.bash_wait(policy, requested) else {
            return Ok(None);
        };
        arguments.insert("wait_for_seconds".to_string(), Value::from(seconds));
        let mut request = request.clone();
        request.arguments = Some(arguments);
        Ok(Some((request, effective)))
    }

    /// Stretch an `Execution` wait to the budget and, under a promoting budget,
    /// default its events to lifecycle changes.
    pub(super) fn normalize_execution_request(
        self,
        mut request: CallToolRequestParams,
    ) -> CallToolRequestParams {
        let Some(action) = request
            .arguments
            .as_mut()
            .and_then(|arguments| arguments.get_mut("action"))
            .and_then(Value::as_object_mut)
            .filter(|action| action.get("type").and_then(Value::as_str) == Some("wait"))
        else {
            return request;
        };
        // A non-numeric value is left for the typed validator to reject.
        let requested = match action.get("timeout_seconds") {
            None | Some(Value::Null) => None,
            Some(value) => Some(serde_json::from_value::<f32>(value.clone()).unwrap_or(f32::NAN)),
        };
        if let Some(seconds) = self.execution_wait_timeout(requested) {
            action.insert("timeout_seconds".to_string(), Value::from(seconds));
        }
        let no_events = action
            .get("events")
            .is_none_or(|events| events.is_null() || events.as_array().is_some_and(Vec::is_empty));
        if self.promote_short_waits && no_events {
            action.insert("events".to_string(), json!(LIFECYCLE_EVENTS));
        }
        request
    }

    /// Run an Execution call with this budget's wait ceiling in scope.
    pub(super) async fn scope_execution_ceiling<F: Future>(
        budget: Option<Self>,
        operation: F,
    ) -> F::Output {
        match budget {
            Some(budget) => {
                EXECUTION_WAIT_CEILING
                    .scope(f32::from(budget.execution_wait_seconds), operation)
                    .await
            }
            None => operation.await,
        }
    }

    /// When a budgeted wait ends still running, make the follow-up a single
    /// call that waits the full budget again.
    pub(super) fn guide_follow_up(
        self,
        result: &mut CallToolResult,
        request: &CallToolRequestParams,
    ) {
        let Some(Value::Object(envelope)) = result.structured_content.as_mut() else { return };
        match request.name.as_ref() {
            "BashCommand" => self.guide_status_check(envelope),
            "Execution" => self.guide_execution_wait(envelope, request.arguments.as_ref()),
            _ => {}
        }
    }

    fn guide_status_check(self, envelope: &mut Map<String, Value>) {
        if envelope.get("status").and_then(Value::as_str) != Some("running") {
            return;
        }
        let Some(arguments) = envelope
            .get_mut("nextAction")
            .filter(|action| action.get("tool").and_then(Value::as_str) == Some("BashCommand"))
            .and_then(|action| action.get_mut("arguments"))
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let foreground_status_check = arguments.get("action_json").is_some_and(|action| {
            action.get("type").and_then(Value::as_str) == Some("status_check")
                && action.get("bg_command_id").is_none_or(Value::is_null)
        });
        if !foreground_status_check {
            return;
        }
        arguments.insert("wait_for_seconds".to_string(), json!(self.sync_seconds));
        let instruction = format!(
            "Call this exact status_check now, without delay: it waits up to {} s and returns as soon as the command finishes or needs input. Do not repeat the original command or interleave Execution waits.",
            self.sync_seconds
        );
        if let Some(action) = envelope.get_mut("nextAction").and_then(Value::as_object_mut) {
            action.insert("instruction".to_string(), Value::String(instruction));
        }
        envelope.remove("retryAfterMs");
    }

    fn guide_execution_wait(
        self,
        envelope: &mut Map<String, Value>,
        arguments: Option<&rmcp::model::JsonObject>,
    ) {
        let Some(arguments) = arguments else { return };
        let data = envelope.get("data");
        let timed_out =
            data.and_then(|data| data.get("timed_out")).and_then(Value::as_bool) == Some(true);
        let cursor = data
            .and_then(|data| data.get("changes"))
            .and_then(|changes| changes.get("cursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let (true, Some(cursor)) = (timed_out, cursor) else { return };
        if envelope.contains_key("nextAction") {
            return;
        }
        let mut next = arguments.clone();
        let Some(action) = next
            .get_mut("action")
            .and_then(Value::as_object_mut)
            .filter(|action| action.get("type").and_then(Value::as_str) == Some("wait"))
        else {
            return;
        };
        action.insert("after".to_string(), Value::String(cursor));
        action.insert("timeout_seconds".to_string(), json!(self.execution_wait_seconds));
        envelope.insert(
            "nextAction".to_string(),
            json!({
                "tool": "Execution",
                "instruction": format!(
                    "No watched execution changed yet. Call this exact wait now: it waits up to {} s and returns as soon as a watched execution exits or needs input.",
                    self.execution_wait_seconds
                ),
                "arguments": Value::Object(next),
            }),
        );
    }
}

/// Longest Execution wait the current request may use.
pub(super) fn execution_wait_ceiling() -> f32 {
    EXECUTION_WAIT_CEILING
        .try_with(|ceiling| *ceiling)
        .unwrap_or(DEFAULT_EXECUTION_WAIT_MAX_SECONDS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::float_cmp)]

    use super::*;

    const RETURN_EARLY: BashWaitPolicy = BashWaitPolicy::ReturnEarly;
    const ADAPTIVE: BashWaitPolicy = BashWaitPolicy::Adaptive;
    const UNTIL_COMPLETE: BashWaitPolicy = BashWaitPolicy::UntilComplete;

    fn budget(profile: ClientProfile) -> WaitBudget {
        WaitBudget::for_profile(profile).expect("profile has a budget")
    }

    #[test]
    fn budget_table_values_stay_under_client_and_transport_timeouts() {
        assert_eq!(WaitBudget::for_profile(ClientProfile::Other), None);
        for profile in [ClientProfile::ChatGpt, ClientProfile::Codex] {
            let budget = budget(profile);
            assert_eq!(budget.sync_seconds, 50, "{profile:?} must stay under 60 s");
            assert_eq!(budget.execution_wait_seconds, 50);
            assert!(budget.promote_short_waits);
        }
        let claude = budget(ClientProfile::Claude);
        assert_eq!((claude.sync_seconds, claude.execution_wait_seconds), (90, 90));
        assert!(!claude.promote_short_waits);
        let timeout = crate::http_server::REQUEST_TIMEOUT.as_secs();
        for profile in [ClientProfile::ChatGpt, ClientProfile::Codex, ClientProfile::Claude] {
            let budget = budget(profile);
            assert!(u64::from(budget.sync_seconds) + REQUEST_SAFETY_MARGIN_SECONDS <= timeout);
            assert!(
                u64::from(budget.execution_wait_seconds) + REQUEST_SAFETY_MARGIN_SECONDS <= timeout
            );
        }
    }

    #[test]
    fn chatgpt_and_codex_promote_short_waits_and_cap_long_ones() {
        for profile in [ClientProfile::ChatGpt, ClientProfile::Codex] {
            let budget = budget(profile);
            let cases = [
                (RETURN_EARLY, Some(1.0), ADAPTIVE, 50.0),
                (RETURN_EARLY, Some(5.0), ADAPTIVE, 50.0),
                (RETURN_EARLY, Some(0.0), ADAPTIVE, 50.0),
                (RETURN_EARLY, None, ADAPTIVE, 50.0),
                (RETURN_EARLY, Some(10.0), RETURN_EARLY, 10.0),
                (ADAPTIVE, None, ADAPTIVE, 50.0),
                (ADAPTIVE, Some(2.0), ADAPTIVE, 50.0),
                (ADAPTIVE, Some(-3.0), ADAPTIVE, 50.0),
                (ADAPTIVE, Some(30.0), ADAPTIVE, 30.0),
                (ADAPTIVE, Some(500.0), ADAPTIVE, 50.0),
                (UNTIL_COMPLETE, None, UNTIL_COMPLETE, 50.0),
                (UNTIL_COMPLETE, Some(3.0), UNTIL_COMPLETE, 50.0),
                (UNTIL_COMPLETE, Some(120.0), UNTIL_COMPLETE, 50.0),
            ];
            for (policy, requested, expected_policy, expected_seconds) in cases {
                assert_eq!(
                    budget.bash_wait(policy, requested),
                    Some((expected_policy, expected_seconds)),
                    "{profile:?} {policy:?} {requested:?}"
                );
            }
        }
    }

    #[test]
    fn claude_honours_short_waits_and_raises_only_the_cap() {
        let budget = budget(ClientProfile::Claude);
        assert_eq!(budget.bash_wait(RETURN_EARLY, Some(1.0)), None);
        assert_eq!(budget.bash_wait(RETURN_EARLY, None), None);
        assert_eq!(budget.bash_wait(ADAPTIVE, None), Some((ADAPTIVE, 15.0)));
        assert_eq!(budget.bash_wait(ADAPTIVE, Some(2.0)), Some((ADAPTIVE, 2.0)));
        assert_eq!(budget.bash_wait(ADAPTIVE, Some(500.0)), Some((ADAPTIVE, 90.0)));
        assert_eq!(budget.bash_wait(UNTIL_COMPLETE, None), Some((UNTIL_COMPLETE, 90.0)));
    }

    #[test]
    fn execution_waits_stretch_to_the_budget_but_keep_polls_and_invalid_values() {
        let chatgpt = budget(ClientProfile::ChatGpt);
        assert_eq!(chatgpt.execution_wait_timeout(None), Some(50.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(20.0)), Some(50.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(500.0)), Some(50.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(0.0)), None);
        assert_eq!(chatgpt.execution_wait_timeout(Some(-1.0)), None);
        assert_eq!(chatgpt.execution_wait_timeout(Some(f32::NAN)), None);
        let claude = budget(ClientProfile::Claude);
        assert_eq!(claude.execution_wait_timeout(None), None);
        assert_eq!(claude.execution_wait_timeout(Some(20.0)), Some(20.0));
        assert_eq!(claude.execution_wait_timeout(Some(500.0)), Some(90.0));
    }

    fn request(name: &'static str, arguments: &Value) -> CallToolRequestParams {
        CallToolRequestParams::new(name)
            .with_arguments(arguments.as_object().expect("object").clone())
    }

    #[test]
    fn only_foreground_commands_and_status_checks_are_budgeted() -> anyhow::Result<()> {
        let chatgpt = budget(ClientProfile::ChatGpt);
        for (action, budgeted) in [
            (json!({"type":"command","command":"sleep 2"}), true),
            (json!({"type":"status_check","status_check":true}), true),
            (json!({"type":"command","command":"serve","is_background":true}), false),
            (json!({"type":"status_check","status_check":true,"bg_command_id":"bg1"}), false),
            (json!({"type":"send_text","send_text":"y"}), false),
        ] {
            let request = request(
                "BashCommand",
                &json!({"thread_id":"t","action_json":action,"wait_for_seconds":1}),
            );
            let bash = super::super::WinxService::parse_ordinary_bash_request(&request);
            let normalized =
                chatgpt.normalize_bash_request(&request, bash.as_ref(), RETURN_EARLY)?;
            assert_eq!(normalized.is_some(), budgeted, "{action}");
            if let Some((request, policy)) = normalized {
                assert_eq!(policy, ADAPTIVE);
                let arguments = request.arguments.expect("arguments");
                assert_eq!(arguments["wait_for_seconds"], json!(50.0));
            }
        }
        Ok(())
    }

    #[test]
    fn execution_request_normalization_defaults_lifecycle_events() {
        let chatgpt = budget(ClientProfile::ChatGpt);
        let normalized = chatgpt.normalize_execution_request(request(
            "Execution",
            &json!({"thread_id":"t","workspace_root":"/w",
                "action":{"type":"wait","execution_ids":["e"],"timeout_seconds":20}}),
        ));
        let action = &normalized.arguments.expect("arguments")["action"];
        assert_eq!(action["timeout_seconds"], json!(50.0));
        assert_eq!(action["events"], json!(LIFECYCLE_EVENTS));

        let explicit = chatgpt.normalize_execution_request(request("Execution", &json!({"thread_id":"t","workspace_root":"/w",
                "action":{"type":"wait","execution_ids":["e"],"events":["output"],"timeout_seconds":0}}),
        ));
        let action = &explicit.arguments.expect("arguments")["action"];
        assert_eq!(action["timeout_seconds"], json!(0));
        assert_eq!(action["events"], json!(["output"]));

        let claude = budget(ClientProfile::Claude).normalize_execution_request(request(
            "Execution",
            &json!({"thread_id":"t","workspace_root":"/w",
                "action":{"type":"wait","execution_ids":["e"]}}),
        ));
        let action = &claude.arguments.expect("arguments")["action"];
        assert!(action.get("timeout_seconds").is_none());
        assert!(action.get("events").is_none());
    }

    #[tokio::test]
    async fn execution_ceiling_is_scoped_to_budgeted_requests() {
        assert_eq!(execution_wait_ceiling(), DEFAULT_EXECUTION_WAIT_MAX_SECONDS);
        let scoped = WaitBudget::scope_execution_ceiling(
            WaitBudget::for_profile(ClientProfile::Claude),
            async { execution_wait_ceiling() },
        )
        .await;
        assert_eq!(scoped, 90.0);
        let unscoped =
            WaitBudget::scope_execution_ceiling(None, async { execution_wait_ceiling() }).await;
        assert_eq!(unscoped, DEFAULT_EXECUTION_WAIT_MAX_SECONDS);
    }

    #[test]
    fn running_status_check_follow_up_carries_the_full_budget() {
        let mut result = CallToolResult::success(Vec::new());
        result.structured_content = Some(json!({
            "status": "running",
            "retryAfterMs": 1000,
            "nextAction": {"tool": "BashCommand", "instruction": "Wait.", "arguments": {
                "thread_id": "t", "action_json": {"type": "status_check", "status_check": true}
            }}
        }));
        let request = request("BashCommand", &json!({"thread_id":"t"}));
        budget(ClientProfile::ChatGpt).guide_follow_up(&mut result, &request);
        let structured = result.structured_content.expect("structured");
        assert_eq!(structured["nextAction"]["arguments"]["wait_for_seconds"], 50);
        assert!(structured.get("retryAfterMs").is_none());
        assert!(structured["nextAction"]["instruction"]
            .as_str()
            .is_some_and(|text| text.contains("50 s")));

        let mut background = CallToolResult::success(Vec::new());
        background.structured_content = Some(json!({
            "status": "running",
            "retryAfterMs": 1000,
            "nextAction": {"tool": "BashCommand", "arguments": {
                "action_json": {"type": "status_check", "bg_command_id": "bg1"}
            }}
        }));
        budget(ClientProfile::ChatGpt).guide_follow_up(&mut background, &request);
        let structured = background.structured_content.expect("structured");
        assert!(structured["nextAction"]["arguments"].get("wait_for_seconds").is_none());
        assert_eq!(structured["retryAfterMs"], 1000);
    }

    #[test]
    fn timed_out_execution_wait_points_to_one_resumed_wait() {
        let mut result = CallToolResult::success(Vec::new());
        result.structured_content = Some(json!({
            "status": "completed",
            "data": {"timed_out": true, "changes": {"cursor": "cursor-2"}}
        }));
        let request = request(
            "Execution",
            &json!({"thread_id":"t","workspace_root":"/w",
                "action":{"type":"wait","execution_ids":["e"],"timeout_seconds":50.0}}),
        );
        budget(ClientProfile::ChatGpt).guide_follow_up(&mut result, &request);
        let structured = result.structured_content.expect("structured");
        let next = &structured["nextAction"];
        assert_eq!(next["tool"], "Execution");
        assert_eq!(next["arguments"]["thread_id"], "t");
        assert_eq!(next["arguments"]["action"]["after"], "cursor-2");
        assert_eq!(next["arguments"]["action"]["timeout_seconds"], 50);

        let mut changed = CallToolResult::success(Vec::new());
        changed.structured_content = Some(json!({
            "status": "completed",
            "data": {"timed_out": false, "changes": {"cursor": "cursor-3"}}
        }));
        budget(ClientProfile::ChatGpt).guide_follow_up(&mut changed, &request);
        assert!(changed.structured_content.expect("structured").get("nextAction").is_none());
    }
}
