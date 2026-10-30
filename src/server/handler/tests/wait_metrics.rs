use super::super::*;
use crate::server::usage::tests::{captured_identity_usage, fixture_usage};
use serde_json::{json, Value};

#[test]
fn effective_wait_emission_follows_normalized_route_and_preserves_request() -> anyhow::Result<()> {
    let cases = [
        (None, false, None, "synchronous", 15_000, None),
        (Some("adaptive"), true, None, "task_inline", 2_000, None),
        (Some("return_early"), true, None, "synchronous", 250, None),
        (Some("until_complete"), false, None, "synchronous", 60_000, None),
        (Some("until_complete"), true, None, "task", 0, None),
        (Some("adaptive"), false, Some(500.0), "synchronous", 60_000, Some(500_000)),
        (Some("adaptive"), true, Some(500.0), "task_inline", 2_000, Some(500_000)),
        (Some("return_early"), true, Some(500.0), "synchronous", 5_000, Some(500_000)),
        (Some("until_complete"), true, Some(500.0), "task", 0, Some(500_000)),
        (Some("adaptive"), false, Some(0.0), "synchronous", 0, Some(0)),
        (Some("adaptive"), true, Some(0.0), "task_inline", 0, Some(0)),
        (Some("return_early"), true, Some(0.125), "synchronous", 125, Some(125)),
    ];
    for (requested_policy, tasks, seconds, expected_mode, expected_ms, requested_ms) in cases {
        let mut arguments = json!({"thread_id":"wait-fixture",
            "action_json":{"type":"command","command":"printf SECRET-COMMAND"}});
        if let Some(policy) = requested_policy {
            arguments["wait_policy"] = json!(policy);
        }
        if let Some(seconds) = seconds {
            arguments["wait_for_seconds"] = json!(seconds);
        }
        let mut usage = fixture_usage("BashCommand", "command", &arguments);
        let object = arguments.as_object().ok_or_else(|| anyhow::anyhow!("fixture object"))?;
        let request = CallToolRequestParams::new("BashCommand").with_arguments(object.clone());
        let policy = WinxService::bash_wait_policy(&request)?;
        let (normalized, mode) = match bash_task_route(tasks, tasks, true, policy) {
            BashTaskRoute::Immediate => (request, EffectiveWaitMode::Task),
            BashTaskRoute::Adaptive => {
                (normalized_wait_request(request, policy, true)?, EffectiveWaitMode::TaskInline)
            }
            BashTaskRoute::Synchronous => {
                (normalized_wait_request(request, policy, false)?, EffectiveWaitMode::Synchronous)
            }
        };
        let candidate = usage.effective_wait_for(&normalized, policy, mode);
        usage.record_effective_wait(candidate);
        let fields = captured_identity_usage(&usage)?;
        assert_eq!(fields["effective_wait_ms"], expected_ms, "{arguments}");
        assert_eq!(fields["effective_wait_mode"], expected_mode, "{arguments}");
        assert_eq!(fields["effective_wait_policy"], requested_policy.unwrap_or("adaptive"));
        assert_eq!(fields.get("requested_wait_ms").and_then(Value::as_u64), requested_ms);
        assert_eq!(fields.get("requested_wait_policy").and_then(Value::as_str), requested_policy);
        assert!(!serde_json::to_string(&fields)?.contains("SECRET"));
    }
    Ok(())
}

#[test]
fn effective_wait_emission_excludes_actions_with_other_timeout_semantics() -> anyhow::Result<()> {
    for (tool, action) in [
        ("BashCommand", "screen"),
        ("BashCommand", "inspect"),
        ("BashCommand", "wait_for_turn"),
        ("BashCommand", "prompt"),
        ("ReadFiles", "command"),
        ("VerifyEdit", "verification"),
    ] {
        let arguments = json!({"wait_for_seconds":0.125, "wait_policy":"return_early",
            "action_json":{"type":action,"timeout_seconds":3}});
        let mut usage = fixture_usage(tool, action, &arguments);
        let object = arguments.as_object().ok_or_else(|| anyhow::anyhow!("fixture object"))?;
        let request = CallToolRequestParams::new(tool.to_string()).with_arguments(object.clone());
        let candidate = usage.effective_wait_for(
            &request,
            BashWaitPolicy::ReturnEarly,
            EffectiveWaitMode::Synchronous,
        );
        usage.record_effective_wait(candidate);
        let fields = captured_identity_usage(&usage)?;
        for name in ["effective_wait_policy", "effective_wait_ms", "effective_wait_mode"] {
            assert!(fields.get(name).is_none_or(Value::is_null), "{tool}/{action}: {name}");
        }
    }
    Ok(())
}

#[test]
fn effective_wait_stays_unknown_before_routing_or_failed_normalization() -> anyhow::Result<()> {
    let arguments = json!({"action_json":{"type":"command","command":"true"},
        "wait_for_seconds":"SECRET-INVALID-WAIT"});
    let usage = fixture_usage("BashCommand", "command", &arguments);
    let object = arguments.as_object().ok_or_else(|| anyhow::anyhow!("fixture object"))?;
    let request = CallToolRequestParams::new("BashCommand").with_arguments(object.clone());
    assert!(normalized_wait_request(request, BashWaitPolicy::Adaptive, false).is_err());
    let fields = captured_identity_usage(&usage)?;
    for name in ["effective_wait_policy", "effective_wait_ms", "effective_wait_mode"] {
        assert!(fields.get(name).is_none_or(Value::is_null), "{name}");
    }
    assert!(!serde_json::to_string(&fields)?.contains("SECRET"));
    Ok(())
}
