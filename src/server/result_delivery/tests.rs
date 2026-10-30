#![allow(clippy::expect_used)]

use std::fmt::Write as _;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Value};

use super::super::client_profile::ClientProfile;
use super::super::outcomes::{
    bash_success_result, decorate_success, finalize_tool_result, tool_failure,
};
use super::ResultTextDelivery;
use crate::errors::WinxError;
use crate::runtime::BashCommandRuntimeResult;
use crate::tools::bash_command::{BashCommandResult, BashCommandState, BashProcessStatus};

fn wire_bytes(result: &CallToolResult) -> usize {
    serde_json::to_vec(result).expect("serializable result").len()
}

fn texts(result: &CallToolResult) -> Vec<&str> {
    result.content.iter().filter_map(|block| block.as_text()).map(|t| t.text.as_str()).collect()
}

fn structured(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().expect("structured envelope")
}

fn exited_bash(output: String) -> BashCommandRuntimeResult {
    BashCommandRuntimeResult {
        result: BashCommandResult {
            output,
            state: BashCommandState {
                process_status: BashProcessStatus::Exited,
                background_id: None,
                running_for_seconds: None,
                exit_code: Some(0),
                cwd: "/workspace".into(),
                turn_state: None,
            },
        },
        compact_output: None,
        command_generation: Some(1),
        execution_token: None,
        generation_bound_actions: false,
        dropped_output_file: None,
        output_truncated: false,
        action_context: None,
        workflow: None,
    }
}

/// ~80 KB of numbered source lines, the shape `ReadFiles` delivers.
fn read_files_result() -> CallToolResult {
    let mut text = String::from("/workspace/src/lib.rs\n```rust\n");
    for line in 1..=1280 {
        let _ = writeln!(
            text,
            "{line:>5} pub fn handler_{line}(input: &str) -> usize {{ input.len() }}"
        );
    }
    text.push_str("```\n");
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(json!({
        "files": [{"path": "/workspace/src/lib.rs", "revision": "r1", "ranges": ["1-1280"]}]
    }));
    decorate_success("ReadFiles", Some(&json!({"thread_id": "thread"})), &mut result);
    finalize_tool_result(&mut result);
    result
}

/// ~80 KB of build log, the shape a long `BashCommand` delivers.
fn bash_command_result() -> CallToolResult {
    let mut output = String::new();
    for line in 1..=1400 {
        let _ =
            writeln!(output, "   Compiling crate-{line} v0.{line}.0 (/workspace/crates/{line})");
    }
    let mut result =
        bash_success_result(Some(&json!({"thread_id": "thread"})), exited_bash(output), false)
            .expect("typed BashCommand result");
    finalize_tool_result(&mut result);
    result
}

#[test]
fn only_chatgpt_drops_the_text_copy_by_default() {
    use ClientProfile::{ChatGpt, Claude, Codex, Other};
    let resolve = ResultTextDelivery::resolve;
    assert_eq!(resolve(ChatGpt, None), ResultTextDelivery::StructuredOnly);
    assert_eq!(resolve(ChatGpt, Some("auto")), ResultTextDelivery::StructuredOnly);
    assert_eq!(resolve(ChatGpt, Some("AUTO")), ResultTextDelivery::StructuredOnly);
    assert_eq!(resolve(ChatGpt, Some("full")), ResultTextDelivery::Mirrored);
    // An unrecognized value never strips content.
    assert_eq!(resolve(ChatGpt, Some("structured")), ResultTextDelivery::Mirrored);
    for profile in [Codex, Claude, Other] {
        assert_eq!(resolve(profile, None), ResultTextDelivery::Mirrored);
        assert_eq!(resolve(profile, Some("auto")), ResultTextDelivery::Mirrored);
    }
}

#[test]
fn chatgpt_gets_a_pointer_and_the_full_redacted_output() {
    let token = format!("ghp_{}", "a".repeat(40));
    let mut result = bash_success_result(
        Some(&json!({"thread_id": "thread"})),
        exited_bash(format!("export GITHUB_TOKEN={token}\nbuild ok")),
        false,
    )
    .expect("typed BashCommand result");
    finalize_tool_result(&mut result);
    ResultTextDelivery::StructuredOnly.apply(&mut result);

    assert_eq!(texts(&result), ["Result is in structuredContent.output (status: completed)."]);
    assert_eq!(
        structured(&result)["output"],
        "export GITHUB_TOKEN=[REDACTED:github-token]\nbuild ok"
    );
    assert_eq!(result.is_error, Some(false));
    assert!(!serde_json::to_string(&result).expect("json").contains(&token));
}

#[test]
fn other_clients_keep_text_and_mirror() {
    let before = bash_command_result();
    let mut after = before.clone();
    ResultTextDelivery::Mirrored.apply(&mut after);
    assert_eq!(after, before);
    assert_eq!(structured(&after)["output"], texts(&after).join("\n"));
}

#[test]
fn secret_straddling_blocks_never_reaches_the_wire() {
    let mut result = CallToolResult::success(vec![
        ContentBlock::text("-----BEGIN PRIVATE KEY-----\nsecret-head"),
        ContentBlock::text("secret-tail\n-----END PRIVATE KEY-----"),
    ]);
    result.structured_content = Some(json!({"status": "completed", "tool": "BashCommand"}));
    finalize_tool_result(&mut result);
    let output = structured(&result)["output"].as_str().expect("output").to_string();
    assert!(output.contains("[REDACTED:private-key]") && !output.contains("secret-head"));

    // Per-block redaction cannot see this key, so the mirrored client still gets
    // the raw blocks (unchanged behaviour); the structured-only client gets none.
    ResultTextDelivery::StructuredOnly.apply(&mut result);
    assert_eq!(texts(&result), ["Result is in structuredContent.output (status: completed)."]);
    let wire = serde_json::to_string(&result).expect("json");
    assert!(!wire.contains("secret-head") && !wire.contains("secret-tail"), "{wire}");
}

#[test]
fn errors_keep_is_error_and_a_readable_message() {
    let mut result = tool_failure(
        "BashCommand",
        &WinxError::BashStateNotInitialized,
        Some(&json!({"thread_id": "thread"})),
    )
    .expect("tool-level error");
    finalize_tool_result(&mut result);
    let message = structured(&result)["message"].as_str().expect("message").to_string();
    let output = structured(&result)["output"].clone();
    ResultTextDelivery::StructuredOnly.apply(&mut result);

    assert_eq!(result.is_error, Some(true));
    let pointers = texts(&result);
    assert_eq!(pointers.len(), 1, "{pointers:?}");
    let pointer = pointers[0];
    assert!(pointer.starts_with(message.trim()), "{pointer}");
    assert!(pointer.ends_with("Result is in structuredContent.output (status: needs_initialize)."));
    assert_eq!(structured(&result)["output"], output);
    assert_eq!(structured(&result)["status"], "needs_initialize");
}

#[test]
fn long_error_messages_are_capped_on_a_char_boundary() {
    let mut result = CallToolResult::error(vec![ContentBlock::text("é".repeat(400))]);
    result.structured_content = Some(json!({
        "status": "failed", "tool": "BashCommand", "message": "é".repeat(400)
    }));
    finalize_tool_result(&mut result);
    ResultTextDelivery::StructuredOnly.apply(&mut result);
    let pointer = texts(&result)[0];
    assert!(pointer.starts_with(&"é".repeat(256)), "{pointer}");
    assert!(pointer.contains("é… Result is in structuredContent.output (status: failed)."));
}

#[test]
fn images_stay_and_text_collapses_in_place() {
    let mut result = CallToolResult::success(vec![
        ContentBlock::image("AAAA", "image/png"),
        ContentBlock::text("caption"),
        ContentBlock::text("details"),
    ]);
    result.structured_content = Some(json!({"status": "completed", "tool": "ReadImage"}));
    finalize_tool_result(&mut result);
    ResultTextDelivery::StructuredOnly.apply(&mut result);
    assert_eq!(result.content.len(), 2);
    assert!(result.content[0].as_image().is_some());
    assert_eq!(texts(&result), ["Result is in structuredContent.output (status: completed)."]);
    assert_eq!(structured(&result)["output"], "caption\ndetails");

    let mut image_only = CallToolResult::success(vec![ContentBlock::image("AAAA", "image/png")]);
    image_only.structured_content = Some(json!({"status": "completed", "tool": "ReadImage"}));
    finalize_tool_result(&mut image_only);
    let before = image_only.clone();
    ResultTextDelivery::StructuredOnly.apply(&mut image_only);
    assert_eq!(image_only, before);
}

#[test]
fn code_map_symbols_point_at_the_structured_payload() {
    let mut symbols = CallToolResult::success(vec![ContentBlock::text("src/lib.rs\n  1 fn main")]);
    symbols.structured_content = Some(json!({
        "mode": "file",
        "files": [{"file": "src/lib.rs", "symbols": [{"name": "main", "kind": "fn", "line": 1}]}],
        "payload_bytes": 1
    }));
    decorate_success("CodeMap", None, &mut symbols);
    finalize_tool_result(&mut symbols);
    ResultTextDelivery::StructuredOnly.apply(&mut symbols);
    assert_eq!(texts(&symbols), ["Result is in structuredContent (status: completed)."]);
    assert_eq!(structured(&symbols)["files"][0]["symbols"][0]["name"], "main");
}

#[test]
fn text_not_mirrored_in_structured_content_is_left_alone() {
    // Manual envelope whose `output` disagrees with the text: collapsing would
    // lose information, so delivery keeps the text.
    let mut diverged = CallToolResult::success(vec![ContentBlock::text("real text")]);
    diverged.structured_content = Some(json!({"status": "completed", "output": "other"}));
    let before = diverged.clone();
    ResultTextDelivery::StructuredOnly.apply(&mut diverged);
    assert_eq!(diverged, before);

    let mut plain = CallToolResult::success(vec![ContentBlock::text("no envelope")]);
    ResultTextDelivery::StructuredOnly.apply(&mut plain);
    assert_eq!(texts(&plain), ["no envelope"]);
}

/// Measurement, not only a guard: `--nocapture` prints the wire sizes.
#[test]
#[allow(clippy::print_stderr)] // the printed byte counts are the point of this test
fn structured_only_delivery_roughly_halves_large_results() {
    for (name, mirrored) in
        [("ReadFiles", read_files_result()), ("BashCommand", bash_command_result())]
    {
        let text_bytes: usize = texts(&mirrored).iter().map(|text| text.len()).sum();
        assert!(text_bytes > 75_000, "{name} fixture is {text_bytes} bytes");
        let mut compact = mirrored.clone();
        ResultTextDelivery::StructuredOnly.apply(&mut compact);
        let (before, after) = (wire_bytes(&mirrored), wire_bytes(&compact));
        eprintln!(
            "{name}: text {text_bytes} B, wire mirrored {before} B -> structured-only {after} B \
             ({} per mille)",
            after * 1000 / before
        );
        assert!(after * 100 <= before * 55, "{name}: {before} -> {after}");
        assert_eq!(structured(&compact)["output"], structured(&mirrored)["output"]);
    }
}
