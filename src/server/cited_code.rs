//! Attach the code a finished command's output cites, read through `ReadFiles`.
//!
//! Remote reasoning clients pay a full reasoning cycle per call, and their
//! most common follow-up to a command is reading what its output points at
//! (measured on `ChatGPT`: a third of `BashCommand` calls were followed by a
//! `ReadFiles`). The excerpts go through the ordinary `ReadFiles` path, so path
//! policy, mode, redaction and budgets apply unchanged, and the returned
//! revisions and visible ranges arm the `EditFiles` guard exactly like a read.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Map, Value};

use super::client_profile::ClientProfile;
use super::WinxService;
use crate::config::ReadResponseBudget;
use crate::tools::cited_code;
use crate::tools::read_files::ReadFilesOutcome;
use crate::types::ReadFiles;

/// `on` attaches for every client, `off` for none; unset or anything else
/// keeps the default (the `ChatGPT` and Codex profiles).
pub(crate) const CITED_CODE_ENV: &str = "WINX_BASH_CITED_CODE";

/// Whether results for `profile` get cited code. Presentation only: the read
/// itself is still subject to the caller's `ReadFiles` authority.
pub(super) fn enabled_for(profile: ClientProfile) -> bool {
    enabled(profile, crate::config::env_text(CITED_CODE_ENV).as_deref())
}

fn enabled(profile: ClientProfile, setting: Option<&str>) -> bool {
    match setting.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("on") => true,
        Some(value) if value.eq_ignore_ascii_case("off") => false,
        _ => matches!(profile, ClientProfile::ChatGpt | ClientProfile::Codex),
    }
}

impl WinxService {
    /// Append the code a completed `BashCommand` result cites. Best effort:
    /// any failure leaves the command result exactly as it was.
    pub(super) async fn attach_cited_code(&self, result: &mut CallToolResult, thread_id: &str) {
        let Some((output, cwd)) = completed_output(result) else { return };
        let Ok(Some(specs)) = tokio::task::spawn_blocking(move || {
            cited_code::read_specs(&output, std::path::Path::new(&cwd))
        })
        .await
        else {
            return;
        };
        let Ok(read_files) = serde_json::from_value::<ReadFiles>(json!({
            "file_paths": specs,
            "thread_id": thread_id,
        })) else {
            return;
        };
        let (slot, _session_guard) = self.tool_session_for(thread_id).await;
        let budget = ReadResponseBudget {
            call_bytes: Some(cited_code::BUDGET_BYTES),
            file_bytes: Some(cited_code::BUDGET_BYTES),
            unchanged_ttl: std::time::Duration::ZERO,
        };
        let Ok(read) =
            crate::tools::read_files::handle_tool_call_with_budget(&slot, read_files, budget).await
        else {
            return;
        };
        if read.successful_files == 0 {
            return;
        }
        self.persist_state(&slot).await;
        attach(result, &read);
    }
}

/// The output and working directory of a completed `BashCommand` result.
fn completed_output(result: &CallToolResult) -> Option<(String, String)> {
    let Some(Value::Object(envelope)) = result.structured_content.as_ref() else { return None };
    if envelope.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    let output = envelope.get("output").and_then(Value::as_str)?;
    let cwd = envelope.get("data")?.get("cwd").and_then(Value::as_str)?;
    (!output.is_empty()).then(|| (output.to_string(), cwd.to_string()))
}

/// Add the excerpts as a separate text block and record their revisions.
/// `finalize_tool_result` later redacts every block and mirrors them into
/// `structuredContent.output`, so structured-only clients receive them too.
fn attach(result: &mut CallToolResult, read: &ReadFilesOutcome) {
    let text = format!(
        "[cited code: excerpts of the location(s) the output above cites; \
         data.cited_code.files carries their revisions, so EditFiles line_patch \
         needs no extra ReadFiles for these lines]\n{}",
        read.text.trim_end()
    );
    result.content.push(ContentBlock::text(text));
    let Some(Value::Object(envelope)) = result.structured_content.as_mut() else { return };
    let data = envelope.entry("data").or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(data) = data {
        data.insert(
            "cited_code".to_string(),
            json!({ "files": read.files, "failed_files": read.errors.len() }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_and_overrides() {
        assert!(enabled(ClientProfile::ChatGpt, None));
        assert!(enabled(ClientProfile::Codex, None));
        assert!(!enabled(ClientProfile::Claude, None));
        assert!(!enabled(ClientProfile::Other, None));
        assert!(enabled(ClientProfile::Other, Some("ON")));
        assert!(!enabled(ClientProfile::ChatGpt, Some(" off ")));
        assert!(enabled(ClientProfile::ChatGpt, Some("unknown")));
    }

    #[test]
    fn only_completed_results_with_output_and_cwd_qualify() {
        let result = |status: &str, output: &str| {
            let mut result = CallToolResult::success(vec![ContentBlock::text(output)]);
            result.structured_content =
                Some(json!({ "status": status, "output": output, "data": { "cwd": "/w" } }));
            result
        };
        assert_eq!(
            completed_output(&result("completed", "a.rs:1:x")),
            Some(("a.rs:1:x".into(), "/w".into()))
        );
        assert_eq!(completed_output(&result("running", "a.rs:1:x")), None);
        assert_eq!(completed_output(&result("completed", "")), None);
    }
}
