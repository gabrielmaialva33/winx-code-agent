#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::utils::agent_temp::release::{prepare, read_managed, TemporaryReleaseTarget};

#[tokio::test]
async fn release_rpc_rechecks_guardian_admission_and_preserves_new_generations(
) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().canonicalize()?;
    let thread = format!("release-guardian-{:032x}", rand::random::<u128>());
    let helper =
        crate::utils::agent_temp::session_info(&workspace, &thread).directory.join("helper");
    std::fs::create_dir_all(helper.parent().unwrap())?;
    std::fs::write(&helper, "helper")?;
    let (text, metadata) = read_managed(&workspace, &thread, &helper)?
        .ok_or_else(|| anyhow::anyhow!("managed read metadata missing"))?;
    let target = TemporaryReleaseTarget {
        file_path: helper.to_string_lossy().into_owned(),
        expected_revision: format!(
            "sha256:{}",
            crate::tools::file_write_or_edit::hash_content(&text)
        ),
        artifact_generation: metadata.artifact_generation,
    };
    let mut release = prepare(&workspace, &thread, &[target])?;
    release.expected_guardian_epoch = Some("owner".into());
    let params = serde_json::to_value(&release)?;
    let session = Arc::new(DaemonSession::new());
    let mut state = BashState::new();
    state.current_thread_id = thread.clone();
    state.workspace_root = workspace.clone();
    state.cwd = workspace;
    state.mode = crate::types::Modes::Wcgw;
    state.init_pty_shell().await?;
    *session.state.lock().await = Some(state);
    let sessions = Arc::new(Mutex::new(HashMap::from([(thread, session.clone())])));
    let request = || RpcRequest {
        jsonrpc: "2.0".into(),
        id: 1,
        method: "session.release_temporary".into(),
        params: params.clone(),
    };
    let held = session.operation_barrier.read().await;
    let blocked = dispatch(request(), &sessions, "owner", &AtomicBool::new(false)).await;
    assert!(blocked.error.is_some());
    assert_eq!(std::fs::read_to_string(&helper)?, "helper");
    drop(held);
    let wrong_owner = dispatch(request(), &sessions, "replacement", &AtomicBool::new(false)).await;
    assert!(wrong_owner.error.is_some());
    let result = dispatch(request(), &sessions, "owner", &AtomicBool::new(false)).await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.result.unwrap()["files"][0]["status"], "released");
    std::fs::write(&helper, "helper")?;
    let retry = dispatch(request(), &sessions, "owner", &AtomicBool::new(false)).await;
    assert!(retry.error.is_some());
    assert_eq!(std::fs::read_to_string(helper)?, "helper");
    Ok(())
}
