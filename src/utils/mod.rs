//! Utility modules for the Winx application.
//!
//! This module contains various utility functions and types used throughout
//! the application, such as file and path handling, repository analysis, etc.

pub mod bash_parser;
pub mod display_tree;
pub mod encoder;
pub mod mmap;
pub mod mode_prompts;
pub mod output_compress;
pub mod path;
pub mod path_prob;
pub mod redact;
pub mod repo;
pub mod scratch_file;
pub mod symbols;
pub mod syntax;
pub mod workspace_stats;

use crate::types::Initialize;
use serde_json::Value;
use tracing::debug;

/// Largest index `<= idx` that is a char boundary of `s`.
///
/// Offsets derived from byte arithmetic over possibly-multibyte text (UTF-8
/// CJK/emoji/box-drawing, the prompt glyphs, NBSP indentation) can land in the
/// middle of a code point. Slicing there panics. Snap down to the nearest
/// boundary so `&s[floor_char_boundary(s, idx)..]` is always valid. For ASCII
/// input this is the identity, so hot paths keep their existing behavior.
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Debug helper to test JSON parsing of an Initialize request
pub fn test_json_parsing(json_str: &str) -> Result<(), String> {
    // First, try to parse as raw JSON to see if the format is valid
    let raw_json_result = serde_json::from_str::<Value>(json_str);
    if let Err(e) = raw_json_result {
        return Err(format!("Invalid JSON format: {e}"));
    }

    // Now try to parse into our Initialize struct
    let init_result = serde_json::from_str::<Initialize>(json_str);
    match init_result {
        Ok(init) => {
            debug!(
                init_type = ?init.init_type,
                mode_name = ?init.mode_name,
                code_writer_config = ?init.code_writer_config,
                task_id_to_resume = init.task_id_to_resume,
                "Successfully parsed JSON into Initialize struct"
            );
            Ok(())
        }
        Err(e) => Err(format!("Failed to parse JSON into Initialize struct: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_snaps_into_multibyte() {
        let s = "a\u{3000}b"; // 'a' (1B) + ideographic space (3B) + 'b' (1B)
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 1); // boundary before the wide char
        assert_eq!(floor_char_boundary(s, 2), 1); // mid code point -> snap back to 1
        assert_eq!(floor_char_boundary(s, 3), 1); // still mid -> 1
        assert_eq!(floor_char_boundary(s, 4), 4); // boundary after the wide char
        assert_eq!(floor_char_boundary(s, 999), s.len()); // past end -> len
                                                          // every returned index must be sliceable without panicking
        for i in 0..=s.len() + 5 {
            let cut = floor_char_boundary(s, i);
            let _ = &s[cut..];
        }
    }
}
