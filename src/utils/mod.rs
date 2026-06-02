//! Utility modules for the Winx application.
//!
//! This module contains various utility functions and types used throughout
//! the application, such as file and path handling, repository analysis, etc.

pub mod bash_parser;
pub mod command_safety;
pub mod display_tree;
pub mod encoder;
pub mod file_cache;
pub mod mmap;
pub mod mode_prompts;
pub mod output_compress;
pub mod path;
pub mod path_prob;
pub mod repo;
pub mod syntax;
pub mod workspace_stats;

use crate::types::Initialize;
use serde_json::Value;
use tracing::debug;

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
