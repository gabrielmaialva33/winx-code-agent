//! Utility modules for the Winx application.
//!
//! This module contains various utility functions and types used throughout
//! the application, such as file and path handling, repository analysis, etc.

pub mod file_cache;
pub mod fuzzy_match;
pub mod mmap;
pub mod path;
pub mod path_analyzer;
pub mod repo;

use crate::types::Initialize;
use serde_json::Value;

/// Debug helper to test JSON parsing of an Initialize request
pub fn test_json_parsing(json_str: &str) -> Result<(), String> {
    // First, try to parse as raw JSON to see if the format is valid
    let raw_json_result = serde_json::from_str::<Value>(json_str);
    if let Err(e) = raw_json_result {
        return Err(format!("Invalid JSON format: {}", e));
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
        Err(e) => Err(format!(
            "Failed to parse JSON into Initialize struct: {}",
            e
        )),
    }
}

/// Test some sample JSON inputs
pub fn run_json_tests() -> Vec<String> {
    let mut results = Vec::new();

    // Test 1: Basic case
    let test1 = r#"
    {
        "type": "first_call",
        "mode_name": "wcgw",
        "any_workspace_path": "/tmp",
        "initial_files_to_read": [],
        "task_id_to_resume": "",
        "chat_id": "",
        "code_writer_config": null
    }
    "#;

    match test_json_parsing(test1) {
        Ok(_) => results.push("Test 1 (Basic case): PASSED".to_string()),
        Err(e) => results.push(format!("Test 1 (Basic case): FAILED - {}", e)),
    }

    // Test 2: String "null" case
    let test2 = r#"
    {
        "type": "first_call",
        "mode_name": "wcgw",
        "any_workspace_path": "/tmp",
        "initial_files_to_read": [],
        "task_id_to_resume": "",
        "chat_id": "",
        "code_writer_config": "null"
    }
    "#;

    // Add test for the format similar to what caused the error
    // Note: The original format wasn't valid JSON, so we're creating a similar but valid JSON version
    let _error_format = r#"{
        "any_workspace_path": "/Users/gabrielmaia/Documents/mcp/winx", 
        "chat_id": "", 
        "code_writer_config": "null", 
        "initial_files_to_read": [], 
        "mode_name": "wcgw", 
        "task_id_to_resume": null, 
        "type": null
    }"#;

    match test_json_parsing(test2) {
        Ok(_) => results.push("Test 2 (String null case): PASSED".to_string()),
        Err(e) => results.push(format!("Test 2 (String null case): FAILED - {}", e)),
    }

    // Test 3: Missing type
    let test3 = r#"
    {
        "mode_name": "wcgw",
        "any_workspace_path": "/tmp",
        "initial_files_to_read": [],
        "task_id_to_resume": "",
        "chat_id": "",
        "code_writer_config": null
    }
    "#;

    match test_json_parsing(test3) {
        Ok(_) => results.push("Test 3 (Missing type): PASSED".to_string()),
        Err(e) => results.push(format!("Test 3 (Missing type): FAILED - {}", e)),
    }

    // Test 4: code_writer case
    let test4 = r#"
    {
        "type": "first_call",
        "mode_name": "code_write",
        "any_workspace_path": "/tmp",
        "initial_files_to_read": [],
        "task_id_to_resume": "",
        "chat_id": "",
        "code_writer_config": {
            "allowed_globs": "all",
            "allowed_commands": "all"
        }
    }
    "#;

    match test_json_parsing(test4) {
        Ok(_) => results.push("Test 4 (code_write): PASSED".to_string()),
        Err(e) => results.push(format!("Test 4 (code_write): FAILED - {}", e)),
    }

    // Instead of using test_json_parsing, create a direct test for the specific error case
    // using the exact format from the error message
    let error_test_result = {
        // The exact format that caused the error
        let _raw_args = r#"{"any_workspace_path": String("/Users/gabrielmaia/Documents/mcp/winx"), "chat_id": String(""), "code_writer_config": String("null"), "initial_files_to_read": Array [], "mode_name": String("wcgw"), "task_id_to_resume": Null, "type": Null}"#;

        // Create a properly formatted JSON
        let proper_json = serde_json::json!({
            "any_workspace_path": "/Users/gabrielmaia/Documents/mcp/winx",
            "chat_id": "",
            "code_writer_config": "null",
            "initial_files_to_read": [],
            "mode_name": "wcgw",
            "task_id_to_resume": null,
            "type": null
        });

        // Try to parse the JSON directly
        match serde_json::from_value::<Initialize>(proper_json) {
            Ok(init) => {
                eprintln!("Successfully parsed JSON using proper json format:");
                eprintln!("  type: {:?}", init.init_type);
                eprintln!("  mode_name: {:?}", init.mode_name);
                eprintln!("  code_writer_config: {:?}", init.code_writer_config);
                eprintln!("  task_id_to_resume: {}", init.task_id_to_resume);
                "Test 5 (Exact error format simulation): PASSED".to_string()
            }
            Err(e) => {
                format!("Test 5 (Exact error format simulation): FAILED - {}", e)
            }
        }
    };

    results.push(error_test_result);

    results
}
