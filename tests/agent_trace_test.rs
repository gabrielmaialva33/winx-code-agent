//! Data-driven orchestration contract checks over privacy-safe usage traces.

use std::path::Path;

#[test]
fn agent_trace_eval_detects_unsafe_retry_and_recovery_sequences() -> anyhow::Result<()> {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_trace_contract.jsonl");
    let report = winx_code_agent::report::usage_report(Some(&fixture), 100, 0, None)?;
    assert!(!report.trace_audit.healthy);
    assert_eq!(report.trace_audit.audited_tool_events, 3);
    assert_eq!(report.trace_audit.legacy_unverifiable_tool_events, 0);
    let rules =
        report.trace_audit.violations.iter().map(|violation| violation.rule).collect::<Vec<_>>();
    assert!(rules.contains(&"retry_same_call_must_be_false"));
    assert!(rules.contains(&"committed_edit_must_never_be_retried"));
    assert!(rules.contains(&"needs_read_must_point_to_read_files"));
    Ok(())
}
