use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;

use winx_code_agent::daemon::DaemonClient;
use winx_code_agent::runtime::{ensure_daemon_at, select_runtime_mode, RuntimeMode};

#[test]
fn daemon_is_default_and_embedded_is_an_explicit_kill_switch() {
    assert_eq!(select_runtime_mode(None, None, None).unwrap(), RuntimeMode::Daemon);
    assert_eq!(select_runtime_mode(Some("1"), None, None).unwrap(), RuntimeMode::Embedded);
    assert_eq!(select_runtime_mode(None, Some("embedded"), None).unwrap(), RuntimeMode::Embedded);
    assert_eq!(select_runtime_mode(None, None, Some("1")).unwrap(), RuntimeMode::Embedded);
    assert!(select_runtime_mode(None, Some("surprise"), None).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_default_can_auto_spawn_the_sibling_binary() -> anyhow::Result<()> {
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    ensure_daemon_at(&socket, Path::new(env!("CARGO_BIN_EXE_winxd"))).await?;

    let hello = DaemonClient::new(&socket).hello().await?;
    assert_eq!(hello.protocol_major, 1);
    assert_ne!(hello.daemon_pid, std::process::id());

    // The test owns this daemon process and terminates only that exact pid.
    unsafe { libc::kill(hello.daemon_pid as i32, libc::SIGTERM) };
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}
