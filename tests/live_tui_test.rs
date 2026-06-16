//! Live-TUI piloting integration tests.
//!
//! These exercise the full wiring added for interactive-terminal piloting:
//! the PTY reader thread feeding a persistent live emulator, `live_snapshot`
//! returning a stable consolidated screen, and alternate-screen detection —
//! all through a real `PtyShell`. Like the other PTY tests they need a real
//! pseudo-terminal and are `#[ignore]`d in CI; run locally with:
//!   cargo test --test `live_tui_test` -- --ignored --nocapture

use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

use winx_code_agent::errors::Result;
use winx_code_agent::state::pty::PtyShell;

/// Run a command and wait for it to finish (prompt returns), then settle so the
/// reader thread has fed every byte into the live emulator.
async fn run_and_settle(shell: &mut PtyShell, command: &str) -> Result<()> {
    shell.send_command(command)?;
    let _ = shell.read_output(5.0)?;
    sleep(Duration::from_millis(250)).await;
    Ok(())
}

/// Boot a shell and turn off terminal echo in a *separate* command, so the
/// command lines we run afterwards aren't echoed back onto the screen — letting
/// the asserts inspect program output alone, not the typed command.
async fn shell_no_echo(dir: &TempDir) -> Result<PtyShell> {
    let mut shell = PtyShell::new(dir.path(), false)?;
    sleep(Duration::from_millis(300)).await;
    run_and_settle(&mut shell, "stty -echo").await?;
    Ok(shell)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn live_snapshot_wires_through_pty() -> Result<()> {
    let dir = TempDir::new()?;
    let mut shell = PtyShell::new(dir.path(), false)?;
    sleep(Duration::from_millis(300)).await;

    run_and_settle(&mut shell, "echo LIVESNAPSHOTWORKS").await?;

    let snap = shell.live_snapshot(0).join("\n");
    assert!(
        snap.contains("LIVESNAPSHOTWORKS"),
        "live emulator should reflect command output; got:\n{snap}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn live_snapshot_consolidates_redraw() -> Result<()> {
    // The whole point of the live emulator: a program that rewrites the screen
    // (carriage-return + erase-line, the way a spinner/progress bar does) must
    // yield only the FINAL frame, never the stacked "soup".
    let dir = TempDir::new()?;
    let mut shell = shell_no_echo(&dir).await?;

    run_and_settle(&mut shell, "printf 'STALEFRAME\\r\\033[KFRESHFRAME\\n'").await?;

    let snap = shell.live_snapshot(0).join("\n");
    assert!(snap.contains("FRESHFRAME"), "final frame missing; got:\n{snap}");
    assert!(
        !snap.contains("STALEFRAME"),
        "stale frame was NOT overwritten — looks like stacked soup; got:\n{snap}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn cha_absolute_column_through_pty() -> Result<()> {
    // CHA (ESC[<col>G) positioning must survive end-to-end: the two tokens land
    // on the same line separated by spaces, not concatenated.
    let dir = TempDir::new()?;
    let mut shell = shell_no_echo(&dir).await?;

    run_and_settle(&mut shell, "printf 'COL1\\033[20GCOL2\\n'").await?;

    let snap = shell.live_snapshot(0).join("\n");
    assert!(snap.contains("COL1"), "COL1 missing; got:\n{snap}");
    assert!(snap.contains("COL2"), "COL2 missing; got:\n{snap}");
    assert!(
        !snap.contains("COL1COL2"),
        "CHA was ignored — tokens collapsed together; got:\n{snap}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn alternate_screen_detected_through_pty() -> Result<()> {
    let dir = TempDir::new()?;
    let mut shell = shell_no_echo(&dir).await?;

    // Enter the alternate screen (smcup) and draw — like vim/htop/less.
    run_and_settle(&mut shell, "printf '\\033[?1049hALTSCREENVIEW'").await?;
    assert!(shell.live_in_alt_screen(), "should be on the alternate screen after ?1049h");
    let alt = shell.live_snapshot(0).join("\n");
    assert!(alt.contains("ALTSCREENVIEW"), "alt-screen content missing; got:\n{alt}");

    // Leave the alternate screen (rmcup) — back to the primary buffer.
    run_and_settle(&mut shell, "printf '\\033[?1049l'").await?;
    assert!(!shell.live_in_alt_screen(), "should be back on the primary screen after ?1049l");
    Ok(())
}
