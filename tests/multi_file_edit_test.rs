//! Integration tests for the `MultiFileEdit` tool.
//!
//! Focus: the all-or-nothing batch behavior AND that the tool restores the
//! `BashState` into its slot after running the (now off-thread) plan+commit IO.
//! A regression where the state isn't put back would leave the session broken
//! ("Bash state not initialized") on the next call.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::{
    FileEditEntry, Initialize, InitializeType, ModeName, MultiFileEdit, ReadFiles,
};

const THREAD: &str = "mfe-test";

async fn init_state(dir: &TempDir) -> Result<Arc<Mutex<Option<BashState>>>> {
    let arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: std::fs::canonicalize(dir.path())?.to_string_lossy().to_string(),
        thread_id: THREAD.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&arc, init).await?;
    Ok(arc)
}

async fn read_file(arc: &Arc<Mutex<Option<BashState>>>, path: &std::path::Path) -> Result<()> {
    let rf = ReadFiles {
        file_paths: vec![path.to_string_lossy().to_string()],
        thread_id: String::new(),
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };
    winx_code_agent::tools::read_files::handle_tool_call(arc, rf).await?;
    Ok(())
}

#[tokio::test]
async fn multi_file_edit_applies_all_and_restores_state() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let a = root.join("a.txt");
    let b = root.join("b.txt");
    std::fs::write(&a, "alpha\n").unwrap();
    std::fs::write(&b, "beta\n").unwrap();

    let arc = init_state(&dir).await.unwrap();
    read_file(&arc, &a).await.unwrap();
    read_file(&arc, &b).await.unwrap();

    let multi = MultiFileEdit {
        thread_id: THREAD.to_string(),
        files: vec![
            FileEditEntry {
                file_path: a.to_string_lossy().to_string(),
                percentage_to_change: 20,
                text_or_search_replace_blocks:
                    "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n".to_string(),
            },
            FileEditEntry {
                file_path: b.to_string_lossy().to_string(),
                percentage_to_change: 20,
                text_or_search_replace_blocks:
                    "<<<<<<< SEARCH\nbeta\n=======\nBETA\n>>>>>>> REPLACE\n".to_string(),
            },
        ],
    };

    let out = winx_code_agent::tools::multi_file_edit::handle_tool_call(&arc, multi).await.unwrap();
    assert!(out.contains("applied all 2 edits"), "unexpected output: {out}");

    // Both files actually changed on disk.
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "BETA\n");

    // CRITICAL: the state must be back in its slot (take/restore around the
    // blocking IO), with the thread id intact — otherwise the session is dead.
    let guard = arc.lock().await;
    let state = guard.as_ref().unwrap();
    assert_eq!(state.current_thread_id, winx_code_agent::types::normalize_thread_id(THREAD));
}

#[tokio::test]
async fn multi_file_edit_aborts_atomically_on_bad_block_and_restores_state() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let a = root.join("a.txt");
    let b = root.join("b.txt");
    std::fs::write(&a, "alpha\n").unwrap();
    std::fs::write(&b, "beta\n").unwrap();

    let arc = init_state(&dir).await.unwrap();
    read_file(&arc, &a).await.unwrap();
    read_file(&arc, &b).await.unwrap();

    // Second file's SEARCH never matches -> the whole batch must abort BEFORE any
    // write (compute-stage all-or-nothing), leaving both files untouched.
    let multi = MultiFileEdit {
        thread_id: THREAD.to_string(),
        files: vec![
            FileEditEntry {
                file_path: a.to_string_lossy().to_string(),
                percentage_to_change: 20,
                text_or_search_replace_blocks:
                    "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n".to_string(),
            },
            FileEditEntry {
                file_path: b.to_string_lossy().to_string(),
                percentage_to_change: 20,
                text_or_search_replace_blocks:
                    "<<<<<<< SEARCH\nNOPE_NOT_PRESENT\n=======\nX\n>>>>>>> REPLACE\n".to_string(),
            },
        ],
    };

    let err = winx_code_agent::tools::multi_file_edit::handle_tool_call(&arc, multi).await;
    assert!(err.is_err(), "batch with an unmatched SEARCH should fail");

    // Neither file was touched (a.txt not written even though its block was valid).
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "beta\n");

    // State still restored after the error path too.
    let guard = arc.lock().await;
    assert!(guard.is_some(), "BashState must be restored even when the batch aborts");
}
