use super::*;
use serde_json::{json, Value};

pub(super) async fn exercise(
    address: std::net::SocketAddr,
    workspace: &Path,
    thread: &str,
) -> anyhow::Result<u64> {
    let source = workspace.join("usage-symbol.rs");
    std::fs::write(&source, "fn privacy_marker_symbol() {}\n")?;
    let response = call_tool_json(
        address,
        &modern_request_meta("usage-log-client", false),
        "usage-codemap",
        "CodeMap",
        json!({
            "thread_id":thread, "operation":"outline", "path":source
        }),
    )
    .await?;
    let result = &response["result"]["structuredContent"];
    assert_eq!(result["status"], "completed", "{response}");
    assert_eq!(result["source_kind"], "canonical_source");
    assert!(result["data"].is_object(), "exercise the shared envelope");
    result["payload_bytes"]
        .as_u64()
        .filter(|size| *size > 0)
        .ok_or_else(|| anyhow::anyhow!("missing nonempty CodeMap payload: {response}"))
}

pub(super) fn assert_entries(contents: &str, payload_bytes: u64) -> anyhow::Result<()> {
    let entries: Vec<Value> =
        contents.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
    let fields = |tool: &str| -> anyhow::Result<&Value> {
        entries
            .iter()
            .rev()
            .find(|entry| {
                entry["fields"]["event"] == "tool_call" && entry["fields"]["tool"] == tool
            })
            .map(|entry| &entry["fields"])
            .ok_or_else(|| anyhow::anyhow!("missing {tool} usage"))
    };
    let map = fields("CodeMap")?;
    assert_eq!(map["source_kind"], "canonical_source");
    assert_eq!(map["payload_bytes"], payload_bytes);
    let read = fields("ReadFiles")?;
    assert_eq!(read["temporary_usage_schema"], 1);
    assert!(read.get("temporary_session_files").is_none_or(Value::is_null));
    assert!(read.get("temporary_over_budget").is_none_or(Value::is_null));
    let bash = fields("BashCommand")?;
    assert_eq!(bash["temporary_usage_schema"], 1);
    assert!(bash["temporary_session_files"].as_u64().is_some());
    assert_eq!(bash["temporary_over_budget"], false);
    assert!(!contents.contains("privacy_marker_symbol"));
    assert!(!contents.contains("usage-symbol.rs"));
    Ok(())
}
