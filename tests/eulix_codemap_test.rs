//! Optional live-backend smoke test; normal CI uses fixture parsers in unit tests.
#![cfg(target_os = "linux")]
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::code_map;
use winx_code_agent::types::CodeMap;

#[tokio::test]
#[ignore = "set WINX_EULIX_PARSER and WINX_EULIX_TEST_ROOT explicitly"]
async fn live_eulix_build_and_navigation() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(std::env::var("WINX_EULIX_TEST_ROOT")?).canonicalize()?;
    let mut state = BashState::new();
    state.workspace_root.clone_from(&root);
    state.cwd = root;
    let state = Arc::new(Mutex::new(Some(state)));
    let name =
        std::env::var("WINX_EULIX_TEST_NAME").unwrap_or_else(|_| "resolve_in_workspace".into());
    let request: CodeMap =
        serde_json::from_value(serde_json::json!({"operation":"callers","name":name}))?;
    let mut fresh = None;
    let mut worst = Duration::ZERO;
    for _ in 0..100 {
        let start = std::time::Instant::now();
        let (_, result) = code_map::handle_tool_call(&state, request.clone()).await?;
        worst = worst.max(start.elapsed());
        if result["index_status"] == "fresh" {
            fresh = Some(result);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = fresh.ok_or_else(|| anyhow::anyhow!("live index did not become fresh"))?;
    assert!(result["graph_hits"].as_array().is_some_and(|hits| !hits.is_empty()));
    assert!(worst < Duration::from_millis(250), "foreground call took {worst:?}");
    for operation in ["callees", "impact"] {
        let request: CodeMap =
            serde_json::from_value(serde_json::json!({"operation":operation,"name":name}))?;
        let (_, result) = code_map::handle_tool_call(&state, request).await?;
        assert_eq!(result["index_status"], "fresh");
    }
    Ok(())
}
