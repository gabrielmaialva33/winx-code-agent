//! Narrow public entry points used by `cargo-fuzz` targets.
//!
//! This module exists only with the `fuzzing` feature and is not enabled in
//! normal builds or release artifacts.

/// Exercise stable thread-ID normalization on arbitrary UTF-8 input.
pub fn normalize_thread_id(input: &[u8]) {
    if let Ok(input) = std::str::from_utf8(input) {
        let normalized = crate::types::normalize_thread_id(input);
        debug_assert!(normalized.len() <= crate::types::MAX_NORMALIZED_THREAD_ID_BYTES);
        debug_assert_eq!(crate::types::normalize_thread_id(&normalized), normalized);
    }
}

/// Exercise the terminal renderer and ANSI stripper on arbitrary bytes.
pub fn terminal(input: &[u8]) {
    let input = String::from_utf8_lossy(input);
    let rendered = crate::state::terminal::render_terminal_output_detailed(&input);
    debug_assert!(rendered.lines.len() <= crate::state::terminal::DEFAULT_MAX_SCREEN_LINES);
    debug_assert_eq!(rendered, crate::state::terminal::render_terminal_output_detailed(&input));
    let _ = crate::state::terminal::strip_ansi_codes(&rendered.lines.join("\n"));
}

/// Exercise tree-sitter command parsing without spawning a shell fallback.
pub fn bash_parser(input: &[u8]) {
    if let Ok(input) = std::str::from_utf8(input) {
        let _ = crate::utils::bash_parser::assert_single_statement(input, false);
        let _ = crate::utils::bash_parser::extract_command_texts(input);
        let _ = crate::utils::bash_parser::extract_static_write_paths(input);
    }
}

/// Exercise `BashCommand`'s tolerant JSON deserializer.
pub fn bash_command_json(input: &[u8]) {
    let _ = serde_json::from_slice::<crate::types::BashCommand>(input);
}

/// Exercise SEARCH/REPLACE parsing and matching. The input is split at the first
/// NUL byte into original file content and the edit payload.
pub fn edit_blocks(input: &[u8]) {
    let split = input.iter().position(|byte| *byte == 0).unwrap_or(input.len());
    let original = String::from_utf8_lossy(&input[..split]);
    let blocks = if split < input.len() {
        String::from_utf8_lossy(&input[split + 1..])
    } else {
        std::borrow::Cow::Borrowed("")
    };
    crate::tools::file_write_or_edit::fuzz_apply_blocks(&original, &blocks);
}

/// Exercise the hidden `EditFiles` wire parser and aggregate validators. The
/// target is intentionally filesystem-free: arbitrary input can reach every
/// typed mode/state transition without authorizing or committing a path.
pub fn edit_files_wire(input: &[u8]) {
    if let Ok(value) = serde_json::from_slice(input) {
        let _ = crate::tools::edit_files::normalize_edit_call(
            crate::tools::edit_files::EditSurface::EditFiles,
            value,
        );
    }
    // Always exercise the structured release branch, including bounded opaque
    // generations and payload exclusion, without touching the filesystem.
    let field = String::from_utf8_lossy(&input[..input.len().min(4096)]);
    let request = serde_json::json!({
        "thread_id": "fuzz", "workspace_root": "/fuzz", "operation": "release_temporary",
        "files": [{"file_path": field, "mode": "release_temporary",
            "expected_revision": format!("sha256:{}", "a".repeat(64)), "artifact_generation": field}]
    });
    let _ = crate::tools::edit_files::normalize_edit_call(
        crate::tools::edit_files::EditSurface::EditFiles,
        request.clone(),
    );
    let mut forbidden = request;
    forbidden["files"][0]["content"] = serde_json::json!(field);
    assert!(crate::tools::edit_files::normalize_edit_call(
        crate::tools::edit_files::EditSurface::EditFiles,
        forbidden
    )
    .is_err());
}

/// Exercise explicit launch-selector capture and deserialization without filesystem access.
pub fn agent_resume(input: &[u8]) {
    use crate::state::agent_resume::AgentSessionRecord;
    use std::path::Path;

    if input.len() > 16 * 1024 {
        return;
    }
    if let Ok(command) = std::str::from_utf8(input) {
        for backend in ["claude", "codex"] {
            let record = AgentSessionRecord::from_launch(
                backend,
                command,
                Path::new("/fuzz/workspace"),
                Path::new("/fuzz/workspace"),
                "fuzz_thread",
                None,
            );
            check_recovery_advice(&record);
        }
    }
    if let Ok(record) = serde_json::from_slice::<AgentSessionRecord>(input) {
        check_recovery_advice(&record);
    }
}

fn check_recovery_advice(record: &crate::state::agent_resume::AgentSessionRecord) {
    use crate::state::agent_resume::{RecoveryClassification, SelectorSource};

    let advice = record.recovery_advice_for("fuzz_thread", std::path::Path::new("/fuzz/workspace"));
    debug_assert!(!advice.current_conversation_verified);
    if let Some(argv) = advice.resume_argv {
        debug_assert_eq!(advice.classification, RecoveryClassification::ExplicitTarget);
        debug_assert_eq!(advice.source, Some(SelectorSource::LaunchArgument));
        debug_assert_eq!(argv.len(), 3);
        if let [backend, operation, selector] = argv.as_slice() {
            debug_assert!(matches!(
                (backend.as_str(), operation.as_str()),
                ("claude", "--resume") | ("codex", "resume")
            ));
            debug_assert_eq!(selector.len(), 36);
            debug_assert!(selector.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            }));
            debug_assert_eq!(record.resume_command(), argv.join(" "));
        }
    }
}
