#![allow(clippy::unwrap_used)]
use super::*;
use rmcp::model::CallToolRequestParams;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

async fn setup(source: &str) -> (tempfile::TempDir, WinxService, String) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("source.rs"), source).unwrap();
    let root = temp.path().canonicalize().map(crate::utils::path::simplified).unwrap();
    let service = WinxService::new();
    let thread = format!("rfsym_{:016x}", rand::random::<u64>());
    let (slot, _guard) = service.session_for(&thread).await;
    let mut state = BashState::new();
    state.cwd = root.clone();
    state.workspace_root = root;
    state.current_thread_id.clone_from(&thread);
    state.initialized = true;
    *slot.lock().await = Some(state);
    (temp, service, thread)
}

async fn call(service: &WinxService, thread: &str, tool: &'static str, mut args: Value) -> Value {
    args["thread_id"] = json!(thread);
    service
        .execute_tool_call(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            crate::runtime::ShellActionOptions::default(),
            crate::tool_policy::ToolPolicy::default().edit_permissions(),
        )
        .await
        .unwrap()
        .result
        .structured_content
        .unwrap()
}

#[tokio::test]
async fn symbol_read_immediately_authorizes_public_editfiles_line_patch() {
    let source =
        "fn hidden() {}\n/// Docs.\nfn target() {\n    let value = 1;\n}\nfn unseen() {}\n";
    let (temp, service, thread) = setup(source).await;
    let read =
        call(&service, &thread, "ReadFiles", json!({"file_paths":["source.rs#target"]})).await;
    assert_eq!(read["status"], "completed", "{read}");
    assert_eq!(read["data"]["files"][0]["visibleRanges"], json!([{"startLine":2,"endLine":5}]));
    let edited=call(&service,&thread,"EditFiles",json!({"files":[{
        "file_path":"source.rs","mode":"line_patch","expected_revision":read["data"]["files"][0]["revision"],
        "patches":[{"start_line":4,"delete_lines":1,"replacement":"    let value = 2;\n"}]
    }]})).await;
    assert_eq!(edited["status"], "completed", "{edited}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("source.rs")).unwrap(),
        source.replace("value = 1", "value = 2")
    );
}

#[tokio::test]
async fn symbol_read_never_authorizes_unseen_or_stale_lines() {
    for stale in [false, true] {
        let source = "fn target() {}\nfn unseen() {}\n";
        let (temp, service, thread) = setup(source).await;
        let read =
            call(&service, &thread, "ReadFiles", json!({"file_paths":["source.rs#target"]})).await;
        if stale {
            std::fs::write(temp.path().join("source.rs"), source.replace("target", "changed"))
                .unwrap();
        }
        let result=call(&service,&thread,"EditFiles",json!({"files":[{
            "file_path":"source.rs","mode":"line_patch","expected_revision":read["data"]["files"][0]["revision"],
            "patches":[{"start_line":if stale {1}else{2},"delete_lines":1,"replacement":"fn replaced() {}\n"}]
        }]})).await;
        assert_eq!(result["status"], "needs_read", "{result}");
        assert_eq!(
            result["errorCode"],
            if stale { "revision_mismatch" } else { "read_required" },
            "{result}"
        );
        assert!(!std::fs::read_to_string(temp.path().join("source.rs"))
            .unwrap()
            .contains("replaced"));
    }
}

#[tokio::test]
async fn symbol_errors_have_reasons_candidates_and_exact_plain_read_next_action() {
    let (temp, service, thread) = setup("fn target() {} // DO_NOT_EXPOSE\n").await;
    let result =
        call(&service, &thread, "ReadFiles", json!({"file_paths":["source.rs#missing"]})).await;
    assert_eq!(result["errorCode"], "symbol_not_found", "{result}");
    assert_eq!(result["data"]["reason"], "not_found");
    assert_eq!(result["nextAction"]["tool"], "ReadFiles");
    let expected =
        crate::utils::path::simplified(temp.path().join("source.rs").canonicalize().unwrap());
    assert_eq!(result["nextAction"]["arguments"]["file_paths"], json!([expected]));
    assert_eq!(result["nextAction"]["arguments"]["thread_id"], thread);
    assert!(!result.to_string().contains("DO_NOT_EXPOSE"));
    let source = (0..17).fold(String::new(), |mut out, i| {
        writeln!(&mut out, "impl P{i} {{ fn target(&self) {{}} }}").unwrap();
        out
    });
    std::fs::write(temp.path().join("source.rs"), source).unwrap();
    let result =
        call(&service, &thread, "ReadFiles", json!({"file_paths":["source.rs#target"]})).await;
    assert_eq!(result["errorCode"], "symbol_ambiguous", "{result}");
    assert_eq!(result["data"]["candidates"].as_array().unwrap().len(), 16);
    assert_eq!(result["data"]["candidatesTruncated"], true);
}

#[tokio::test]
async fn mixed_batches_preserve_successful_receipts_and_all_symbol_diagnostics() {
    let (temp, service, thread) = setup("fn target() {}\n").await;
    std::fs::write(temp.path().join("unsupported.txt"), "DO_NOT_EXPOSE").unwrap();
    let result = call(
        &service,
        &thread,
        "ReadFiles",
        json!({"file_paths":["source.rs#target","source.rs#missing","unsupported.txt#target"]}),
    )
    .await;
    assert_eq!(result["data"]["successful_files"], 1, "{result}");
    assert_eq!(result["data"]["failed_files"], 2);
    assert_eq!(result["data"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(result["data"]["symbol_errors"].as_array().unwrap().len(), 2);
    assert!(!result.to_string().contains("DO_NOT_EXPOSE"));
}

#[tokio::test]
async fn symbol_read_keeps_redaction_and_raw_source_revision() {
    let secret = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let source = format!("fn target() {{\n let key = \"{secret}\";\n}}\n");
    let (_temp, service, thread) = setup(&source).await;
    let result =
        call(&service, &thread, "ReadFiles", json!({"file_paths":["source.rs#target"]})).await;
    assert_eq!(result["status"], "completed", "{result}");
    assert!(!result.to_string().contains(secret));
    assert!(result["output"].as_str().unwrap().contains("[REDACTED:"));
    let hash = Sha256::digest(source.as_bytes());
    let expected = hash.iter().fold(String::from("sha256:"), |mut out, byte| {
        write!(&mut out, "{byte:02x}").unwrap();
        out
    });
    assert_eq!(result["data"]["files"][0]["revision"], expected);
}
