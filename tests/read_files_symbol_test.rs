//! Environment-isolated parity checks for plain and symbol read path policy.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use winx_code_agent::config::ReadResponseBudget;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::read_files::handle_tool_call_with_budget;
use winx_code_agent::types::ReadFiles;

#[tokio::test]
async fn plain_and_symbol_reads_share_workspace_allowlist_and_symlink_policy() -> anyhow::Result<()>
{
    const CASE: &str = "WINX_TEST_SYMBOL_POLICY";
    if let Ok(case) = std::env::var(CASE) {
        let root = PathBuf::from(std::env::var("WINX_TEST_SYMBOL_ROOT")?).canonicalize()?;
        let outside = PathBuf::from(std::env::var("WINX_TEST_SYMBOL_OUTSIDE")?).canonicalize()?;
        let mut state = BashState::new();
        state.cwd = root.clone();
        state.workspace_root = root.clone();
        state.initialized = true;
        let state = Arc::new(Mutex::new(Some(state)));
        let mut paths = vec![outside.join("outside.rs")];
        if cfg!(unix) {
            paths.push(root.join("escape.rs"));
        }
        for path in paths {
            for suffix in ["", "#target"] {
                let request: ReadFiles = serde_json::from_value(
                    serde_json::json!({"file_paths":[format!("{}{suffix}",path.display())]}),
                )?;
                let result = handle_tool_call_with_budget(
                    &state,
                    request,
                    ReadResponseBudget::parse(None, None, None),
                )
                .await?;
                assert_eq!(
                    result.successful_files > 0,
                    case == "allowed",
                    "{case}: {}",
                    result.text
                );
                if case != "allowed" {
                    assert!(matches!(
                        result.errors.first(),
                        Some(winx_code_agent::WinxError::PathSecurityError { .. })
                    ));
                    assert!(!result.text.contains("OUTSIDE_SOURCE_MARKER"));
                }
            }
        }
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root)?;
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("outside.rs"), "fn target() { /* OUTSIDE_SOURCE_MARKER */ }\n")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("outside.rs"), root.join("escape.rs"))?;
    for case in ["denied", "allowed"] {
        let mut child = std::process::Command::new(std::env::current_exe()?);
        child
            .args([
                "--exact",
                "plain_and_symbol_reads_share_workspace_allowlist_and_symlink_policy",
            ])
            .env(CASE, case)
            .env("WINX_TEST_SYMBOL_ROOT", &root)
            .env("WINX_TEST_SYMBOL_OUTSIDE", &outside)
            .env_remove("WINX_ALLOW_PATHS");
        if case == "allowed" {
            child.env("WINX_ALLOW_PATHS", Path::new(&outside));
        }
        assert!(child.status()?.success(), "{case} child failed");
    }
    Ok(())
}
