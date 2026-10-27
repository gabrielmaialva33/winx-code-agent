#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::*;

struct ControlledReader {
    chunk: Option<Vec<u8>>,
    reached_next_read: Option<mpsc::Sender<()>>,
    release_eof: mpsc::Receiver<()>,
    finished: mpsc::Sender<()>,
}

impl Read for ControlledReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if let Some(chunk) = self.chunk.take() {
            output[..chunk.len()].copy_from_slice(&chunk);
            return Ok(chunk.len());
        }
        if let Some(reached) = self.reached_next_read.take() {
            reached.send(()).map_err(std::io::Error::other)?;
            self.release_eof.recv_timeout(Duration::from_secs(3)).map_err(std::io::Error::other)?;
        }
        Ok(0)
    }
}

impl Drop for ControlledReader {
    fn drop(&mut self) {
        let _ = self.finished.send(());
    }
}

fn test_shell() -> (tempfile::TempDir, PtyShell) {
    let temp = tempfile::TempDir::new().unwrap();
    let mut shell = PtyShell::new(temp.path(), false).unwrap();
    // Isolate the controlled pump from the real shell's otherwise-idle reader.
    shell.activity_seq = Arc::new(AtomicU64::new(0));
    shell.queued_activity_seq = Arc::new(AtomicU64::new(0));
    shell.prompt_activity = Arc::new(StdMutex::new(None));
    shell.live = Arc::new(StdMutex::new(LiveTerminal::new(DEFAULT_ROWS, DEFAULT_COLS)));
    shell.output_buffer.clear();
    (temp, shell)
}

fn controlled_pump(
    shell: &mut PtyShell,
    chunk: &[u8],
) -> (mpsc::Receiver<()>, mpsc::Sender<()>, mpsc::Receiver<()>) {
    let (output_tx, output_rx) = mpsc::sync_channel(4);
    let (next_tx, next_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    shell.output_rx = output_rx;
    spawn_pty_reader(
        Box::new(ControlledReader {
            chunk: Some(chunk.to_vec()),
            reached_next_read: Some(next_tx),
            release_eof: release_rx,
            finished: finished_tx,
        }),
        output_tx,
        shell.live.clone(),
        shell.discard_throttle.clone(),
        shell.activity_seq.clone(),
        shell.queued_activity_seq.clone(),
        shell.prompt_activity.clone(),
    );
    (next_rx, release_tx, finished_rx)
}

#[test]
fn native_workflow_output_watermark_waits_for_the_queued_chunk() {
    let (_temp, mut shell) = test_shell();
    let gate = shell.prompt_activity.clone();
    let gate_guard = gate.lock().unwrap();
    let baseline = shell.activity_seq();
    let (next_read, release_eof, finished) = controlled_pump(&mut shell, b"LATE_QUEUED_OUTPUT");
    let deadline = Instant::now() + Duration::from_secs(3);
    while shell.activity_seq() == baseline {
        assert!(Instant::now() < deadline, "reader did not reach the held gate");
        thread::yield_now();
    }
    // The reader has fed the live screen and advanced its raw sequence, but the
    // held gate proves it has not reached the channel send yet.
    let before_enqueue = shell.poll_workflow_output();
    assert!(!shell.output_snapshot().contains("LATE_QUEUED_OUTPUT"));
    drop(gate_guard);
    next_read.recv_timeout(Duration::from_secs(3)).unwrap();
    let after_enqueue = shell.poll_workflow_output();
    assert!(shell.output_snapshot().contains("LATE_QUEUED_OUTPUT"));
    release_eof.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        after_enqueue > before_enqueue,
        "newly queued output must require a new full history capture"
    );
}

#[test]
fn native_workflow_eof_utf8_carry_advances_the_output_watermark() {
    let (_temp, mut shell) = test_shell();
    let (next_read, release_eof, finished) = controlled_pump(&mut shell, b"x\xe2");
    next_read.recv_timeout(Duration::from_secs(3)).unwrap();
    let prefix_sequence = shell.poll_workflow_output();
    assert_eq!(shell.output_snapshot(), "x");
    release_eof.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(3)).unwrap();
    let eof_sequence = shell.poll_workflow_output();
    assert_eq!(shell.output_snapshot(), "x\u{fffd}");
    assert!(
        eof_sequence > prefix_sequence,
        "EOF replacement bytes must not reuse the prefix's covered-output sequence"
    );
}
