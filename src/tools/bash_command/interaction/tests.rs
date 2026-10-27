#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::*;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn native_workflow_legacy_submit_never_enters_a_replacement_pty() {
    let temp = tempfile::TempDir::new().unwrap();
    let mut state = BashState::new();
    state.cwd = temp.path().to_path_buf();
    let mut original = PtyShell::new(temp.path(), false).unwrap();
    original.send_command("stty -echo; cat >/dev/null").unwrap();
    let mut replacement = PtyShell::new(temp.path(), false).unwrap();
    replacement.send_command("stty -echo; printf REPLACEMENT_READY; IFS= read -r line; printf 'UNEXPECTED_ENTER\\n'; sleep 5").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    original.poll_output_nonblocking();
    replacement.poll_output_nonblocking();
    assert_eq!(
        original.command_generation(),
        replacement.command_generation(),
        "incarnation must distinguish generation collisions"
    );
    let shell = Arc::new(Mutex::new(Some(original)));
    let replacing = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        shell.lock().await.replace(replacement);
    };
    let sending = execute_send_text(
        &mut state,
        "hello",
        true,
        Some(shell.clone()),
        true,
        Some("replacement-test"),
        0.1,
        None,
        false,
    );
    let (result, ()) = tokio::join!(sending, replacing);
    assert!(result.is_err(), "a send bound to the prior incarnation must reject replacement");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut guard = shell.lock().await;
    let replacement = guard.as_mut().unwrap();
    replacement.poll_output_nonblocking();
    assert!(
        !replacement.output_snapshot().contains("UNEXPECTED_ENTER\r\n"),
        "Enter must not be written to the replacement command"
    );
}
