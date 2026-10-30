use super::{captured_usage, captured_usage_request};
use serde_json::{json, Value};

const TEMP_FIELDS: [&str; 5] = [
    "temporary_session_files",
    "temporary_session_bytes",
    "temporary_stale_pruned_files",
    "temporary_stale_pruned_bytes",
    "temporary_over_budget",
];

#[test]
fn codemap_root_metadata_survives_a_real_data_envelope() -> anyhow::Result<()> {
    let fields = captured_usage(
        "CodeMap",
        "outline",
        json!({
            "tool":"CodeMap", "status":"completed", "canonical":true,
            "source_kind":"canonical_source", "payload_bytes":562,
            "files":[{"file":"SECRET.rs", "symbols":[]}],
            "data":{"operation":"outline", "output_bytes":0, "output_truncated":false}
        }),
    )?;
    assert_eq!(fields["source_kind"], "canonical_source");
    assert_eq!(fields["payload_bytes"], 562);
    assert!(!fields.to_string().contains("SECRET"));
    Ok(())
}

#[test]
fn codemap_canonical_root_wins_over_legacy_metadata() -> anyhow::Result<()> {
    let fields = captured_usage(
        "CodeMap",
        "outline",
        json!({
            "status":"completed", "source_kind":"canonical_source", "payload_bytes":562,
            "sourceKind":"legacy", "payloadBytes":7,
            "data":{"source_kind":"nested", "payload_bytes":3}
        }),
    )?;
    assert_eq!(fields["source_kind"], "canonical_source");
    assert_eq!(fields["payload_bytes"], 562);
    Ok(())
}

#[test]
fn result_metadata_keeps_legacy_and_nested_image_layouts() -> anyhow::Result<()> {
    for (tool, envelope, kind, bytes) in [
        ("CodeMap", json!({"sourceKind":"legacy","payloadBytes":17,"data":{}}), "legacy", 17),
        (
            "CodeMap",
            json!({"data":{"result":{"source_kind":"nested","payload_bytes":23}}}),
            "nested",
            23,
        ),
        ("ReadImage", json!({"data":{"result":{"delivered_bytes":31}}}), "", 31),
        (
            "CodeMap",
            json!({"source_kind":null,"payload_bytes":"bad","data":{"source_kind":"nested","payload_bytes":0}}),
            "nested",
            0,
        ),
    ] {
        let fields = captured_usage(tool, "", envelope)?;
        assert_eq!(fields["source_kind"], kind);
        assert_eq!(fields["payload_bytes"], bytes);
    }
    Ok(())
}

#[test]
fn temporary_metadata_absence_is_not_an_empty_budget() -> anyhow::Result<()> {
    for envelope in [
        None,
        Some(json!({"status":"completed","data":{}})),
        Some(json!({
            "status":"completed","data":{"temporary_artifact_budget":null}
        })),
    ] {
        let fields = captured_usage_request("ReadFiles", "", &json!({}), envelope)?;
        assert_eq!(fields["temporary_usage_schema"], 1);
        for key in TEMP_FIELDS {
            assert!(fields.get(key).is_none_or(Value::is_null), "fabricated {key}: {fields}");
        }
    }
    Ok(())
}

#[test]
fn temporary_metadata_preserves_measured_zero_and_false() -> anyhow::Result<()> {
    let fields = captured_usage(
        "BashCommand",
        "command",
        json!({"status":"completed","data":{
            "temporary_artifact_budget":{"session_files":0,"session_bytes":0,
                "stale_pruned_files":0,"stale_pruned_bytes":0,"over_budget":false}
        }}),
    )?;
    assert_eq!(fields["temporary_usage_schema"], 1);
    for key in &TEMP_FIELDS[..4] {
        assert_eq!(fields[*key], 0, "{key}");
    }
    assert_eq!(fields["temporary_over_budget"], false);
    Ok(())
}

#[test]
fn temporary_metadata_malformed_values_stay_unknown() -> anyhow::Result<()> {
    let fields = captured_usage(
        "BashCommand",
        "command",
        json!({"data":{
            "temporary_artifact_budget":{"session_files":"0","session_bytes":-1,
                "stale_pruned_files":null,"stale_pruned_bytes":[],"over_budget":"false"}
        }}),
    )?;
    for key in TEMP_FIELDS {
        assert!(fields.get(key).is_none_or(Value::is_null), "fabricated {key}: {fields}");
    }
    Ok(())
}

#[test]
fn temporary_metadata_cleanup_result_preserves_measured_overflow() -> anyhow::Result<()> {
    let fields = captured_usage(
        "BashCommand",
        "command",
        json!({"status":"completed_with_issues",
            "data":{"session_files":129,"session_bytes":4096,"stale_pruned_files":2,
                "stale_pruned_bytes":128,"temporary_artifact_cleanup_required":true}
        }),
    )?;
    assert_eq!(fields["temporary_session_files"], 129);
    assert_eq!(fields["temporary_session_bytes"], 4096);
    assert_eq!(fields["temporary_stale_pruned_files"], 2);
    assert_eq!(fields["temporary_stale_pruned_bytes"], 128);
    assert_eq!(fields["temporary_over_budget"], true);
    Ok(())
}
