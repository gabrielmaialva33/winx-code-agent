//! Utility modules for the Winx application.
//!
//! This module contains various utility functions and types used throughout
//! the application, such as file and path handling, repository analysis, etc.

pub mod command_safety;
pub mod file_cache;
pub mod mmap;
pub mod path;
pub mod repo;

use crate::types::Initialize;
use serde_json::Value;

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
            // Log the successfully parsed values
            eprintln!("Successfully parsed JSON into Initialize struct:");
            eprintln!("  type: {:?}", init.init_type);
            eprintln!("  mode_name: {:?}", init.mode_name);
            eprintln!("  code_writer_config: {:?}", init.code_writer_config);
            eprintln!("  task_id_to_resume: {}", init.task_id_to_resume);
            Ok(())
        }
        Err(e) => Err(format!("Failed to parse JSON into Initialize struct: {e}")),
    }
}