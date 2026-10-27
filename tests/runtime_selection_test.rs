use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use winx_code_agent::daemon::{
    DaemonClient, COMPACT_ACTION_OUTPUT_CAPABILITY, TYPED_ACTION_RESULT_CAPABILITY,
};
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

/// Stop a test daemon: SIGTERM on Unix, the graceful `winx.shutdown` RPC on
/// Windows (guardians are separate detached processes and keep their PTYs).
#[cfg_attr(unix, allow(clippy::unused_async))] // only the Windows branch awaits
async fn terminate_test_daemon(socket: &Path, pid: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let _ = socket;
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
    #[cfg(not(unix))]
    {
        let _ = pid;
        // The daemon may already be gone (e.g. after a control restart); a
        // failed shutdown is not a test failure.
        let _ = DaemonClient::new(socket).shutdown().await;
        Ok(())
    }
}

/// Stop the winner of a concurrent claim and every late claimant. A loser keeps
/// retrying the socket lock for a short window (`CLAIM_RETRY_WINDOW` in the
/// transport); one still retrying when the winner dies inherits the dead socket
/// and becomes a daemon nobody asked for, outliving the test's temp dir.
async fn terminate_test_daemon_and_late_claimants(socket: &Path, pid: u32) -> anyhow::Result<()> {
    terminate_test_daemon(socket, pid).await?;
    let mut terminated = BTreeSet::from([pid]);
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        if let Ok(hello) = DaemonClient::new(socket).hello().await {
            if terminated.insert(hello.daemon_pid) {
                terminate_test_daemon(socket, hello.daemon_pid).await?;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_default_can_auto_spawn_the_sibling_binary() -> anyhow::Result<()> {
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    ensure_control_daemon_at(&socket, Path::new(env!("CARGO_BIN_EXE_winxd"))).await?;

    let hello = DaemonClient::new(&socket).hello().await?;
    let validation = if hello.protocol_major == 1
        && hello.protocol_minor >= 5
        && hello.capabilities.iter().any(|capability| capability == "guardian_activity_clock")
        && hello.capabilities.iter().any(|capability| capability == TYPED_ACTION_RESULT_CAPABILITY)
        && hello
            .capabilities
            .iter()
            .any(|capability| capability == COMPACT_ACTION_OUTPUT_CAPABILITY)
        && hello.daemon_pid != std::process::id()
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("unexpected control hello: {hello:?}"))
    };
    terminate_test_daemon(&socket, hello.daemon_pid).await?;
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
        && restarted.protocol_minor >= 5
        && restarted.capabilities.iter().any(|capability| capability == "guardian_activity_clock")
        && restarted
            .capabilities
            .iter()
            .any(|capability| capability == TYPED_ACTION_RESULT_CAPABILITY)
        && restarted
            .capabilities
            .iter()
            .any(|capability| capability == COMPACT_ACTION_OUTPUT_CAPABILITY)
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "control restart did not produce the expected hello: before={previous:?} after={restarted:?}"
        ))
    };
    terminate_test_daemon(&socket, restarted.daemon_pid).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    validation
}

/// Several MCP clients often start at the same moment (one editor, many
/// sessions). Every adapter that finds no daemon spawns `winxd`; exactly one
/// claim must win, the others must attach to it, and no adapter may fail.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_auto_spawns_converge_on_one_daemon() -> anyhow::Result<()> {
    let runtime_dir = TempDir::new()?;
    let socket = runtime_dir.path().join("winxd.sock");
    let binary = Path::new(env!("CARGO_BIN_EXE_winxd"));

    // Spawn all starters before awaiting any, so the launches really overlap.
    let starters: Vec<_> = (0..4)
        .map(|_| {
            let socket = socket.clone();
            tokio::spawn(async move { ensure_control_daemon_at(&socket, binary).await })
        })
        .collect();
    for starter in starters {
        starter.await??;
    }

    let hello = DaemonClient::new(&socket).hello().await?;
    let mut pids = std::collections::BTreeSet::new();
    for _ in 0..4 {
        pids.insert(DaemonClient::new(&socket).hello().await?.daemon_pid);
    }
    let validation = if pids.len() == 1 && pids.contains(&hello.daemon_pid) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("expected one daemon behind the socket, saw pids {pids:?}"))
    };
    terminate_test_daemon_and_late_claimants(&socket, hello.daemon_pid).await?;
    validation
}
