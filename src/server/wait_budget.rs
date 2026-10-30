//! Per-client wait budgets for remote (HTTP) clients.
//!
//! Returning a running result requires another client round trip. Profile
//! defaults can reduce those trips, but explicit observation allowances must
//! never grow. These deployment choices are not protocol-defined timeout
//! guarantees. Client names select tuning only, never authority; stdio and
//! unknown clients retain their existing defaults and ceilings.

use std::future::Future;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};

use super::client_profile::ClientProfile;
use crate::types::{BashCommand, BashCommandAction, BashWaitPolicy};

/// Execution wait ceiling for unbudgeted clients (the public schema range).
pub(super) const DEFAULT_EXECUTION_WAIT_MAX_SECONDS: f32 = 30.0;
/// Headroom between the longest budget and the HTTP request timeout for
/// session admission, guardian round trips and response serialization.
const REQUEST_SAFETY_MARGIN_SECONDS: u64 = 20;
const MAX_BUDGET_SECONDS: u64 =
    crate::http_server::REQUEST_TIMEOUT.as_secs() - REQUEST_SAFETY_MARGIN_SECONDS;
/// Lifecycle events an Execution wait defaults to under this profile, so
/// output chatter does not end the wait early.
const LIFECYCLE_EVENTS: [&str; 4] = ["exited", "awaiting_input", "awaiting_approval", "unknown"];

/// Wait tuning for one client family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WaitBudget {
    /// Longest one foreground `BashCommand` command or `status_check` may block.
    pub sync_seconds: u16,
    /// Wait used when an adaptive call omits `wait_for_seconds`.
    pub adaptive_default_seconds: u16,
    /// Select the profile Execution default and lifecycle event selection.
    pub profile_lifecycle_defaults: bool,
    /// Longest one `Execution` wait may block.
    pub execution_wait_seconds: u16,
}

// A conservative deployment default below common one-minute request budgets.
// Actual client timeout/retry behavior must be measured independently.
const CHATGPT: WaitBudget = WaitBudget {
    sync_seconds: 50,
    adaptive_default_seconds: 50,
    profile_lifecycle_defaults: true,
    execution_wait_seconds: 50,
};
// Share the conservative default; negotiated Tasks remain a separate decision.
const CODEX: WaitBudget = CHATGPT;
// Longer ceiling with transport headroom; explicit short waits stay short.
// This is a Winx policy, not a universal guarantee about hosts or tunnels.
const CLAUDE: WaitBudget = WaitBudget {
    sync_seconds: 90,
    adaptive_default_seconds: 15,
    profile_lifecycle_defaults: false,
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
        match (policy, requested) {
            (BashWaitPolicy::ReturnEarly, _) => None,
            (_, Some(seconds)) => Some((policy, seconds.min(cap))),
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
            Some(seconds) if !seconds.is_finite() || seconds < 0.0 => None,
            Some(seconds) => Some(seconds.min(cap)),
            None if self.profile_lifecycle_defaults => Some(cap),
            None => None,
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
            .filter(|value| !value.is_null())
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

    /// Select an omitted Execution wait's profile default, cap explicit waits
    /// down, and default only omitted events to lifecycle changes.
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
        let no_events = action.get("events").is_none_or(Value::is_null);
        if self.profile_lifecycle_defaults && no_events {
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

    /// Preserve the selected wait and event semantics in a follow-up.
    pub(super) fn guide_follow_up(result: &mut CallToolResult, request: &CallToolRequestParams) {
        let Some(Value::Object(envelope)) = result.structured_content.as_mut() else { return };
        match request.name.as_ref() {
            "BashCommand" => Self::guide_status_check(envelope, request.arguments.as_ref()),
            "Execution" => Self::guide_execution_wait(envelope, request.arguments.as_ref()),
            _ => {}
        }
    }

    fn guide_status_check(
        envelope: &mut Map<String, Value>,
        prior: Option<&rmcp::model::JsonObject>,
    ) {
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
        let Some(action) = arguments
            .get("action_json")
            .filter(|action| action.get("type").and_then(Value::as_str) == Some("status_check"))
        else {
            return;
        };
        let background_id = action.get("bg_command_id").and_then(Value::as_str);
        let background = background_id.is_some();
        let Some(prior) = prior else { return };
        if let Some(id) = background_id {
            // Preserve only this background launch/poll, never borrow a foreground allowance.
            let same_background = prior.get("action_json").is_some_and(|action| {
                match action.get("type").and_then(Value::as_str) {
                    Some("command") => {
                        action.get("is_background").and_then(Value::as_bool) == Some(true)
                    }
                    Some("status_check") => {
                        action.get("bg_command_id").and_then(Value::as_str) == Some(id)
                    }
                    _ => false,
                }
            });
            if !same_background {
                return;
            }
        }
        let Some(wait) = prior.get("wait_for_seconds") else { return };
        arguments.insert("wait_for_seconds".to_string(), wait.clone());
        let policy = prior.get("wait_policy").and_then(Value::as_str).unwrap_or("adaptive");
        arguments.insert(
            "wait_policy".to_string(),
            json!(if policy == "until_complete" { "adaptive" } else { policy }),
        );
        if background || wait.as_f64() == Some(0.0) || policy == "return_early" {
            return;
        }
        let instruction = format!(
            "Call this exact status_check now, without delay: it waits up to {wait} s and returns as soon as the command finishes or needs input. Do not repeat the original command or interleave Execution waits."
        );
        if let Some(action) = envelope.get_mut("nextAction").and_then(Value::as_object_mut) {
            action.insert("instruction".to_string(), Value::String(instruction));
        }
        envelope.remove("retryAfterMs");
    }

    fn guide_execution_wait(
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
        let seconds = action.get("timeout_seconds").and_then(Value::as_f64).unwrap_or(5.0);
        if seconds == 0.0 {
            envelope.insert("retryAfterMs".to_string(), json!(1000));
        }
        envelope.insert(
            "nextAction".to_string(),
            json!({
                "tool": "Execution",
                "instruction": format!(
                    "No watched execution changed yet. Respect retryAfterMs when present, then call this exact wait; its observation allowance is {} s.",
                    seconds
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
    fn contract_return_early_is_never_promoted() {
        assert_eq!(budget(ClientProfile::ChatGpt).bash_wait(RETURN_EARLY, Some(1.0)), None);
    }

    #[test]
    fn contract_explicit_short_execution_wait_is_preserved() {
        assert_eq!(budget(ClientProfile::ChatGpt).execution_wait_timeout(Some(0.25)), Some(0.25));
    }

    #[test]
    fn contract_follow_up_keeps_wait_and_events() {
        let mut result = CallToolResult::success(Vec::new());
        result.structured_content =
            Some(json!({"data":{"timed_out":true,"changes":{"cursor":"c"}}}));
        let request = request(
            "Execution",
            &json!({"action":{"type":"wait","timeout_seconds":0,"events":["output"]}}),
        );
        WaitBudget::guide_follow_up(&mut result, &request);
        let result = result.structured_content.expect("structured");
        assert_eq!(result["nextAction"]["arguments"]["action"]["timeout_seconds"], 0);
        assert_eq!(result["nextAction"]["arguments"]["action"]["events"], json!(["output"]));
        assert_eq!(result["retryAfterMs"], 1000);
    }

    #[test]
    fn contract_bash_follow_up_preserves_zero_and_uses_valid_policy() {
        for (policy, expected) in [("until_complete", "adaptive"), ("return_early", "return_early")]
        {
            let mut result = CallToolResult::success(Vec::new());
            result.structured_content = Some(json!({"status":"running","retryAfterMs":1000,
                "nextAction":{"tool":"BashCommand","arguments":{"action_json":{"type":"status_check"}}}}));
            let request =
                request("BashCommand", &json!({"wait_policy":policy,"wait_for_seconds":0}));
            WaitBudget::guide_follow_up(&mut result, &request);
            let result = result.structured_content.expect("structured");
            assert_eq!(result["nextAction"]["arguments"]["wait_for_seconds"], 0);
            assert_eq!(result["nextAction"]["arguments"]["wait_policy"], expected);
            assert_eq!(result["retryAfterMs"], 1000);
        }
    }

    #[test]
    fn contract_null_bash_wait_is_omission() {
        let request = request(
            "BashCommand",
            &json!({"thread_id":"t","action_json":{"type":"command","command":"true"},"wait_for_seconds":null}),
        );
        let bash = super::super::WinxService::parse_ordinary_bash_request(&request);
        let normalized = budget(ClientProfile::ChatGpt)
            .normalize_bash_request(&request, bash.as_ref(), ADAPTIVE)
            .expect("null is valid")
            .expect("eligible");
        assert_eq!(normalized.0.arguments.expect("arguments")["wait_for_seconds"], 50.0);
    }

    #[test]
    fn budget_table_values_stay_under_client_and_transport_timeouts() {
        assert_eq!(WaitBudget::for_profile(ClientProfile::Other), None);
        for profile in [ClientProfile::ChatGpt, ClientProfile::Codex] {
            let budget = budget(profile);
            assert_eq!(budget.sync_seconds, 50, "{profile:?} must stay under 60 s");
            assert_eq!(budget.execution_wait_seconds, 50);
            assert!(budget.profile_lifecycle_defaults);
        }
        let claude = budget(ClientProfile::Claude);
        assert_eq!((claude.sync_seconds, claude.execution_wait_seconds), (90, 90));
        assert!(!claude.profile_lifecycle_defaults);
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
    fn chatgpt_and_codex_preserve_short_waits_and_cap_long_ones() {
        for profile in [ClientProfile::ChatGpt, ClientProfile::Codex] {
            let budget = budget(profile);
            assert_eq!(budget.bash_wait(RETURN_EARLY, Some(1.0)), None);
            assert_eq!(budget.bash_wait(RETURN_EARLY, None), None);
            let cases = [
                (ADAPTIVE, None, ADAPTIVE, 50.0),
                (ADAPTIVE, Some(0.0), ADAPTIVE, 0.0),
                (ADAPTIVE, Some(2.0), ADAPTIVE, 2.0),
                (ADAPTIVE, Some(500.0), ADAPTIVE, 50.0),
                (UNTIL_COMPLETE, None, UNTIL_COMPLETE, 50.0),
                (UNTIL_COMPLETE, Some(3.0), UNTIL_COMPLETE, 3.0),
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
    fn execution_waits_keep_short_values_and_cap_long_ones() {
        let chatgpt = budget(ClientProfile::ChatGpt);
        assert_eq!(chatgpt.execution_wait_timeout(None), Some(50.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(20.0)), Some(20.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(500.0)), Some(50.0));
        assert_eq!(chatgpt.execution_wait_timeout(Some(0.0)), Some(0.0));
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
            let normalized = chatgpt.normalize_bash_request(&request, bash.as_ref(), ADAPTIVE)?;
            assert_eq!(normalized.is_some(), budgeted, "{action}");
            if let Some((request, policy)) = normalized {
                assert_eq!(policy, ADAPTIVE);
                let arguments = request.arguments.expect("arguments");
                assert_eq!(arguments["wait_for_seconds"], json!(1.0));
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
        assert_eq!(action["timeout_seconds"], json!(20.0));
        assert_eq!(action["events"], json!(LIFECYCLE_EVENTS));

        let explicit = chatgpt.normalize_execution_request(request("Execution", &json!({"thread_id":"t","workspace_root":"/w",
                "action":{"type":"wait","execution_ids":["e"],"events":["output"],"timeout_seconds":0}}),
        ));
        let action = &explicit.arguments.expect("arguments")["action"];
        assert_eq!(action["timeout_seconds"], json!(0.0));
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
        let request = request("BashCommand", &json!({"thread_id":"t", "wait_for_seconds":50}));
        WaitBudget::guide_follow_up(&mut result, &request);
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
        WaitBudget::guide_follow_up(&mut background, &request);
        let structured = background.structured_content.expect("structured");
        assert!(structured["nextAction"]["arguments"].get("wait_for_seconds").is_none());
        assert_eq!(structured["retryAfterMs"], 1000);
    }

    #[test]
    fn background_follow_up_preserves_explicit_observation_allowance() {
        for seconds in [0.0, 0.25, 1.0] {
            let mut result = CallToolResult::success(Vec::new());
            result.structured_content = Some(json!({
                "status":"running", "retryAfterMs":1000,
                "nextAction":{"tool":"BashCommand","arguments":{
                    "action_json":{"type":"status_check","bg_command_id":"bg1"}
                }}
            }));
            let prior = request(
                "BashCommand",
                &json!({
                    "action_json":{"type":"command","command":"long-running-fixture","is_background":true},
                    "wait_policy":"return_early", "wait_for_seconds":seconds
                }),
            );
            WaitBudget::guide_follow_up(&mut result, &prior);
            let envelope = result.structured_content.expect("structured");
            assert_eq!(envelope["nextAction"]["arguments"]["wait_for_seconds"], seconds);
            assert_eq!(envelope["nextAction"]["arguments"]["wait_policy"], "return_early");
            assert_eq!(envelope["nextAction"]["arguments"]["action_json"]["bg_command_id"], "bg1");
            assert_eq!(envelope["retryAfterMs"], 1000);
        }
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
        WaitBudget::guide_follow_up(&mut result, &request);
        let structured = result.structured_content.expect("structured");
        let next = &structured["nextAction"];
        assert_eq!(next["tool"], "Execution");
        assert_eq!(next["arguments"]["thread_id"], "t");
        assert_eq!(next["arguments"]["action"]["after"], "cursor-2");
        assert_eq!(next["arguments"]["action"]["timeout_seconds"], 50.0);

        let mut changed = CallToolResult::success(Vec::new());
        changed.structured_content = Some(json!({
            "status": "completed",
            "data": {"timed_out": false, "changes": {"cursor": "cursor-3"}}
        }));
        WaitBudget::guide_follow_up(&mut changed, &request);
        assert!(changed.structured_content.expect("structured").get("nextAction").is_none());
    }
}
