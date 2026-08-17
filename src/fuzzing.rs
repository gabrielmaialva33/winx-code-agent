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
    let rendered = crate::state::terminal::render_terminal_output(&input);
    let _ = crate::state::terminal::strip_ansi_codes(&rendered.join("\n"));
}

/// Exercise tree-sitter command parsing without spawning a shell fallback.
pub fn bash_parser(input: &[u8]) {
    if let Ok(input) = std::str::from_utf8(input) {
        let _ = crate::utils::bash_parser::assert_single_statement(input, false);
        let _ = crate::utils::bash_parser::extract_command_texts(input);
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
