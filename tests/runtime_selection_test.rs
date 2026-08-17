#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;

use winx_code_agent::daemon::DaemonClient;
use winx_code_agent::runtime::{
    ensure_control_daemon_at, restart_control_daemon_at, select_runtime_mode, RuntimeMode,
};

#[test]
fn daemon_is_default_and_embedded_is_an_explicit_kill_switch() {
    assert!(matches!(select_runtime_mode(None, None, None), Ok(RuntimeMode::Daemon)));
    assert!(matches!(select_runtime_mode(Some("1"), None, None), Ok(RuntimeMode::Embedded)));
    assert!(matches!(select_runtime_mode(None, Some("embedded"), None), Ok(RuntimeMode::Embedded)));
    assert!(matches!(select_runtime_mode(None, None, Some("1")), Ok(RuntimeMode::Embedded)));
    assert!(select_runtime_mode(None, Some("surprise"), None).is_err());
}

fn terminate_test_daemon(pid: u32) -> anyhow::Result<()> {
    let pid = i32::try_from(pid)?;
    // SAFETY: the pid came from the authenticated same-UID daemon handshake.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.into())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_default_can_auto_spawn_the_sibling_binary() -> anyhow::Result<()> {
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    ensure_control_daemon_at(&socket, Path::new(env!("CARGO_BIN_EXE_winxd"))).await?;

    let hello = DaemonClient::new(&socket).hello().await?;
    let validation = if hello.protocol_major == 1
        && hello.capabilities.iter().any(|capability| capability == "guardian_quota")
        && hello.daemon_pid != std::process::id()
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("unexpected control hello: {hello:?}"))
    };
    terminate_test_daemon(hello.daemon_pid)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    validation
}

#[tokio::test(flavor = "multi_thread")]
async fn planned_control_restart_changes_epoch() -> anyhow::Result<()> {
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let binary = Path::new(env!("CARGO_BIN_EXE_winxd"));
    ensure_control_daemon_at(&socket, binary).await?;
    let previous = DaemonClient::new(&socket).hello().await?;

    let restarted = restart_control_daemon_at(&socket, binary).await?;
    let validation = if restarted.daemon_epoch != previous.daemon_epoch
        && restarted.capabilities.iter().any(|capability| capability == "guardian_quota")
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "control restart did not produce the expected hello: before={previous:?} after={restarted:?}"
        ))
    };
    terminate_test_daemon(restarted.daemon_pid)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    validation
}
