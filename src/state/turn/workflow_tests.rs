use super::*;

#[test]
fn native_workflow_explanation_preserves_manifest_rule_and_unknown_authority() {
    let screen = vec![
        "⏺ old tool output esc to interrupt".to_string(),
        "Showing detailed transcript".to_string(),
        "ctrl+o to toggle".to_string(),
    ];
    let recognizer = recognizer_for("claude");
    let explanation = recognizer.explain_input(&TurnInput::screen_only(&screen));
    assert_eq!(explanation.state, TurnState::Unknown);
    assert_eq!(explanation.source, "manifest");
    assert!(explanation.matched_rule.is_some());
    assert!(explanation.priority.is_some());
    assert!(explanation.reason.as_deref().is_some_and(|reason| reason.contains("explicitly")));
    assert_eq!(explanation.state, recognizer.detect(&screen));
}

#[test]
fn native_workflow_explanation_exposes_legacy_fallback() {
    let screen = vec!["● answered".to_string(), "❯".to_string()];
    let explanation = recognizer_for("claude").explain_input(&TurnInput::screen_only(&screen));
    assert_eq!(explanation.state, TurnState::AwaitingInput);
    assert_eq!(explanation.source, "fallback");
    assert!(explanation
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("no manifest rule")));
    assert!(explanation.matched_rule.is_none());
}

#[test]
fn native_workflow_explanation_uses_auto_winning_state() {
    let screen =
        vec!["❯ stale prompt".to_string(), "• Working (2s • esc to interrupt)".to_string()];
    let recognizer = recognizer_for("auto");
    let explanation = recognizer.explain_input(&TurnInput::screen_only(&screen));
    assert_eq!(explanation.state, TurnState::Busy);
    assert_eq!(explanation.state, recognizer.detect(&screen));
    assert!(matches!(explanation.recognizer.as_str(), "claude" | "codex"));
}

#[test]
fn native_workflow_external_recognizer_gets_default_explanation() {
    struct External;
    impl TurnRecognizer for External {
        fn name(&self) -> &'static str {
            "external"
        }
        fn detect(&self, _: &[String]) -> TurnState {
            TurnState::Unknown
        }
        fn detect_input(&self, _: &TurnInput<'_>) -> TurnState {
            TurnState::Busy
        }
    }
    let explanation = External.explain_input(&TurnInput::screen_only(&[]));
    assert_eq!(explanation.state, TurnState::Busy);
    assert_eq!(explanation.recognizer, "external");
    assert_eq!(explanation.source, "recognizer");
}
