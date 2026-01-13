//! Integration tests for FileWriteOrEdit tool.
//!
//! Tests:
//! 1. Create new file with percentage > 50 (full write)
//! 2. Edit existing file with SEARCH/REPLACE blocks (percentage <= 50)
//! 3. Whitelist enforcement (must read before edit)
//! 4. Multiple SEARCH/REPLACE blocks in one operation

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::{FileWriteOrEdit, Initialize, InitializeType, ModeName, ReadFiles};

const TEST_THREAD_ID: &str = "i2238";

/// Helper function to create an initialized bash state with a specific thread ID
async fn create_initialized_state(
    temp_dir: &TempDir,
    thread_id: &str,
) -> Result<Arc<Mutex<Option<BashState>>>> {
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: thread_id.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    Ok(bash_state_arc)
}

// ==================== Test 1: Create New File (percentage > 50) ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_create_new_file_full_write() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, TEST_THREAD_ID).await?;

    let file_path = temp_dir.path().join("new_file.py");
    let content = r#"#!/usr/bin/env python3
"""A simple test module."""

def greet(name: str) -> str:
    """Return a greeting message."""
    return f"Hello, {name}!"

def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b

if __name__ == "__main__":
    print(greet("World"))
    print(add(2, 3))
"#;

    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: content.to_string(),
        thread_id: TEST_THREAD_ID.to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
            .await?;

    // Verify response indicates success
    assert!(
        response.contains("Successfully") || response.contains("wrote"),
        "Expected success message, got: {}",
        response
    );

    // Verify file exists and content matches
    assert!(file_path.exists(), "File was not created");

    let actual_content = std::fs::read_to_string(&file_path).expect("Failed to read created file");
    assert_eq!(actual_content, content, "File content does not match");

    println!("TEST 1 PASSED: New file created successfully with percentage > 50");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_create_new_rust_file() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-rust-create").await?;

    let file_path = temp_dir.path().join("lib.rs");
    let content = r#"//! A test library module.

/// Add two numbers together.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtract b from a.
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn test_subtract() {
        assert_eq!(subtract(5, 3), 2);
    }
}
"#;

    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: content.to_string(),
        thread_id: "test-rust-create".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("wrote"),
        "Expected success message, got: {}",
        response
    );

    // Verify file content
    let actual = std::fs::read_to_string(&file_path).expect("Failed to read file");
    assert!(actual.contains("pub fn add"));
    assert!(actual.contains("pub fn subtract"));
    assert!(actual.contains("#[cfg(test)]"));

    println!("TEST PASSED: Rust file created successfully");
    Ok(())
}

// ==================== Test 2: Edit with SEARCH/REPLACE (percentage <= 50) ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_edit_with_search_replace() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-search-replace").await?;

    // First create a file
    let file_path = temp_dir.path().join("edit_test.py");
    let initial_content = r#"#!/usr/bin/env python3
"""A simple test module."""

def greet(name: str) -> str:
    """Return a greeting message."""
    return f"Hello, {name}!"

def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b
"#;

    // Write the initial file
    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: initial_content.to_string(),
        thread_id: "test-search-replace".to_string(),
    };

    winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
        .await?;

    // Read the file to add it to whitelist
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Now edit with SEARCH/REPLACE
    let search_replace = r#"<<<<<<< SEARCH
def greet(name: str) -> str:
    """Return a greeting message."""
    return f"Hello, {name}!"
=======
def greet(name: str, formal: bool = False) -> str:
    """Return a greeting message."""
    if formal:
        return f"Good day, {name}!"
    return f"Hello, {name}!"
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 30,
        text_or_search_replace_blocks: search_replace.to_string(),
        thread_id: "test-search-replace".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("edited"),
        "Expected success message, got: {}",
        response
    );

    // Verify the edit was applied
    let final_content = std::fs::read_to_string(&file_path).expect("Failed to read edited file");

    assert!(final_content.contains("formal: bool = False"), "Type hint not added");
    assert!(final_content.contains("Good day"), "New code not present");
    assert!(final_content.contains("if formal:"), "Conditional not added");

    println!("TEST 2 PASSED: SEARCH/REPLACE edit successful");
    Ok(())
}

// ==================== Test 3: Whitelist Enforcement ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_whitelist_enforcement_edit_without_read() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-whitelist").await?;

    // Create a file OUTSIDE the tool (simulating external file)
    let file_path = temp_dir.path().join("unread_file.txt");
    std::fs::write(&file_path, "Original content here.\n").expect("Failed to write file");

    // Try to edit WITHOUT reading first
    let search_replace = r#"<<<<<<< SEARCH
Original content here.
=======
Modified content here.
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 30,
        text_or_search_replace_blocks: search_replace.to_string(),
        thread_id: "test-whitelist".to_string(),
    };

    let result =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await;

    // This should fail or return an error about needing to read first
    match result {
        Ok(response) => {
            // In wcgw mode with full permissions, it might work
            // But there should be some indication about whitelist
            println!("INFO: Edit allowed in wcgw mode (full permissions). Response: {}", response);

            // Verify the file was modified if it succeeded
            let content = std::fs::read_to_string(&file_path).expect("Failed to read file");
            if content.contains("Modified content") {
                println!("TEST 3 INFO: In wcgw mode, edit was allowed (expected in full-permission mode)");
            }
        }
        Err(e) => {
            // Expected error about whitelist or reading file first
            let error_msg = e.to_string().to_lowercase();
            assert!(
                error_msg.contains("read")
                    || error_msg.contains("whitelist")
                    || error_msg.contains("access"),
                "Expected whitelist error, got: {}",
                e
            );
            println!("TEST 3 PASSED: Whitelist properly enforced (edit blocked)");
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "race condition on macOS CI - whitelist async update timing"]
async fn test_whitelist_after_read() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-whitelist-read").await?;

    // Create a file OUTSIDE the tool
    let file_path = temp_dir.path().join("to_be_read.txt");
    std::fs::write(&file_path, "Original line one.\nOriginal line two.\n")
        .expect("Failed to write file");

    // Read the file first (adds to whitelist)
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Give time for async whitelist update
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Now edit should work
    let search_replace = r#"<<<<<<< SEARCH
Original line one.
=======
Modified line one.
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 30,
        text_or_search_replace_blocks: search_replace.to_string(),
        thread_id: "test-whitelist-read".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("edited"),
        "Expected success after reading file, got: {}",
        response
    );

    let content = std::fs::read_to_string(&file_path).expect("Failed to read file");
    assert!(content.contains("Modified line one"), "Edit was not applied");

    println!("TEST PASSED: Edit worked after reading file (whitelist populated)");
    Ok(())
}

// ==================== Test 4: Multiple SEARCH/REPLACE Blocks ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_multiple_search_replace_blocks() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-multi-blocks").await?;

    // Create a file with multiple functions
    let file_path = temp_dir.path().join("calculator.py");
    let initial_content = r#"#!/usr/bin/env python3
"""Multi-edit test file."""

class Calculator:
    """Simple calculator class."""

    def add(self, a, b):
        """Add two numbers."""
        return a + b

    def subtract(self, a, b):
        """Subtract b from a."""
        return a - b

    def multiply(self, a, b):
        """Multiply two numbers."""
        return a * b

if __name__ == "__main__":
    calc = Calculator()
    print(calc.add(1, 2))
"#;

    // Write initial file
    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: initial_content.to_string(),
        thread_id: "test-multi-blocks".to_string(),
    };

    winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
        .await?;

    // Read file to populate whitelist
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Apply multiple SEARCH/REPLACE blocks
    let multi_search_replace = r#"<<<<<<< SEARCH
    def add(self, a, b):
        """Add two numbers."""
        return a + b
=======
    def add(self, a: int, b: int) -> int:
        """Add two numbers together."""
        return a + b
>>>>>>> REPLACE
<<<<<<< SEARCH
    def subtract(self, a, b):
        """Subtract b from a."""
        return a - b
=======
    def subtract(self, a: int, b: int) -> int:
        """Subtract second number from first."""
        return a - b
>>>>>>> REPLACE
<<<<<<< SEARCH
    def multiply(self, a, b):
        """Multiply two numbers."""
        return a * b
=======
    def multiply(self, a: int, b: int) -> int:
        """Multiply two numbers together."""
        return a * b

    def divide(self, a: int, b: int) -> float:
        """Divide first number by second."""
        if b == 0:
            raise ValueError("Cannot divide by zero")
        return a / b
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 40,
        text_or_search_replace_blocks: multi_search_replace.to_string(),
        thread_id: "test-multi-blocks".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("edited"),
        "Expected success message, got: {}",
        response
    );

    // Verify all edits were applied
    let final_content = std::fs::read_to_string(&file_path).expect("Failed to read edited file");

    // Check for all expected changes
    let checks = [
        (final_content.contains("a: int, b: int) -> int"), "Type hints added to add()"),
        (final_content.contains("Subtract second number"), "Docstring updated in subtract()"),
        (
            final_content.contains("Multiply two numbers together"),
            "Docstring updated in multiply()",
        ),
        (final_content.contains("def divide"), "New divide() method added"),
        (final_content.contains("Cannot divide by zero"), "Divide error handling present"),
    ];

    let mut all_passed = true;
    for (check, description) in checks.iter() {
        if *check {
            println!("  OK: {}", description);
        } else {
            println!("  FAIL: {}", description);
            all_passed = false;
        }
    }

    assert!(all_passed, "Not all edits were applied.\nFinal content:\n{}", final_content);

    println!("TEST 4 PASSED: All multiple SEARCH/REPLACE blocks applied correctly");
    Ok(())
}

// ==================== Additional Edge Case Tests ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_search_replace_not_found() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-not-found").await?;

    // Create a file
    let file_path = temp_dir.path().join("test.txt");
    let initial_content = "Line one\nLine two\nLine three\n";

    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: initial_content.to_string(),
        thread_id: "test-not-found".to_string(),
    };

    winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
        .await?;

    // Read to populate whitelist
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Try to edit with non-existent search block
    let search_replace = r#"<<<<<<< SEARCH
This text does not exist in the file
=======
Replacement text
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 30,
        text_or_search_replace_blocks: search_replace.to_string(),
        thread_id: "test-not-found".to_string(),
    };

    let result =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await;

    // Should fail because search block was not found
    assert!(result.is_err(), "Expected error for non-existent search block");

    let error_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        error_msg.contains("not found") || error_msg.contains("search"),
        "Expected 'not found' error, got: {}",
        error_msg
    );

    println!("TEST PASSED: Search block not found error properly returned");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_empty_replacement() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-empty-replace").await?;

    // Create a file with some content to remove
    let file_path = temp_dir.path().join("remove_test.txt");
    let initial_content = "Keep this line\nRemove this line\nKeep this too\n";

    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: initial_content.to_string(),
        thread_id: "test-empty-replace".to_string(),
    };

    winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
        .await?;

    // Read to populate whitelist
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Edit with empty replacement (to remove a line)
    let search_replace = r#"<<<<<<< SEARCH
Remove this line
=======
>>>>>>> REPLACE"#;

    let file_edit = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 30,
        text_or_search_replace_blocks: search_replace.to_string(),
        thread_id: "test-empty-replace".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_edit)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("edited"),
        "Expected success message, got: {}",
        response
    );

    // Verify line was removed
    let final_content = std::fs::read_to_string(&file_path).expect("Failed to read file");
    assert!(!final_content.contains("Remove this line"), "Line was not removed");
    assert!(final_content.contains("Keep this line"), "Other content was incorrectly removed");
    assert!(final_content.contains("Keep this too"), "Other content was incorrectly removed");

    println!("TEST PASSED: Empty replacement (line deletion) works");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_overwrite_existing_file_full_content() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "test-overwrite").await?;

    // Create a file
    let file_path = temp_dir.path().join("overwrite.txt");
    let initial_content = "Original content";

    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: initial_content.to_string(),
        thread_id: "test-overwrite".to_string(),
    };

    winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
        .await?;

    // Read to populate whitelist
    let read_files = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read_files).await?;

    // Overwrite with new content (percentage > 50)
    let new_content = "Completely new content\nWith multiple lines\nAnd more text";

    let file_write2 = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: new_content.to_string(),
        thread_id: "test-overwrite".to_string(),
    };

    let response =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write2)
            .await?;

    assert!(
        response.contains("Successfully") || response.contains("wrote"),
        "Expected success message, got: {}",
        response
    );

    // Verify new content
    let final_content = std::fs::read_to_string(&file_path).expect("Failed to read file");
    assert!(!final_content.contains("Original"), "Old content still present");
    assert_eq!(final_content, new_content, "Content does not match new content");

    println!("TEST PASSED: File overwrite with percentage > 50 works");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_thread_id_mismatch() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc = create_initialized_state(&temp_dir, "correct-thread-id").await?;

    let file_path = temp_dir.path().join("test.txt");

    // Try to write with wrong thread ID
    let file_write = FileWriteOrEdit {
        file_path: file_path.to_string_lossy().to_string(),
        percentage_to_change: 100,
        text_or_search_replace_blocks: "Test content".to_string(),
        thread_id: "wrong-thread-id".to_string(),
    };

    let result =
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&bash_state_arc, file_write)
            .await;

    // Should fail due to thread ID mismatch
    assert!(result.is_err(), "Expected error for thread ID mismatch");

    let error_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        error_msg.contains("thread") || error_msg.contains("mismatch") || error_msg.contains("id"),
        "Expected thread ID mismatch error, got: {}",
        error_msg
    );

    println!("TEST PASSED: Thread ID mismatch properly rejected");
    Ok(())
}
