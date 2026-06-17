//! Real PTY implementation using portable-pty
//!
//! This module provides a true pseudo-terminal interface for interactive
//! shell sessions, enabling proper handling of:
//! - ANSI escape sequences and colors
//! - Interactive programs (sudo, vim, less, etc.)
//! - Terminal resize events
//! - Job control signals (Ctrl+C, Ctrl+Z, etc.)

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::state::line_ring::LineRing;
use crate::state::live_terminal::{LiveTerminal, ScreenUpdate};

/// Default terminal dimensions (columns x rows)
pub const DEFAULT_COLS: u16 = 200;
pub const DEFAULT_ROWS: u16 = 50;

/// Maximum output buffer size to prevent memory issues
const MAX_OUTPUT_SIZE: usize = 1_000_000;

/// Cap on bytes streamed to one command's output-offload scratch file, so a
/// command emitting gigabytes cannot fill the disk. The head beyond this is
/// dropped (the agent still gets the first 50 MB plus the live tail).
const SCRATCH_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// How many fully-formed lines to keep in the per-shell ringbuffer. Callers can
/// ask for at most this many lines of historical context via
/// `StatusCheck.scrollback_lines`.
pub const RING_BUFFER_LINES: usize = 2_000;

/// Cap on the unterminated tail held in `line_ring_partial`. Bounds memory when
/// a program emits megabytes without ever printing a newline (a `\r`-redrawn
/// progress bar, a binary blob). Generous enough to hold any real terminal line.
const MAX_PARTIAL_LINE_BYTES: usize = 64 * 1024;

/// WCGW-style prompt pattern for command completion detection
const WCGW_PROMPT_PATTERN: &str = "◉";
const WCGW_PROMPT_END: &str = "──➤";

fn attachable_command(restricted_mode: bool) -> (CommandBuilder, Option<String>, bool) {
    let requested = std::env::var("WINX_ATTACH_TERMINAL")
        .or_else(|_| std::env::var("WINX_USE_SCREEN"))
        .unwrap_or_default();
    if !requested.is_empty() && requested != "0" && requested != "false" {
        let session = format!("winx-{}-{}", std::process::id(), timestamp_millis());
        if requested == "tmux" && command_available("tmux") {
            let mut cmd = CommandBuilder::new("tmux");
            cmd.args(["new-session", "-A", "-s", &session, "bash"]);
            if restricted_mode {
                cmd.arg("-r");
            }
            return (cmd, Some(format!("tmux attach -t {session}")), false);
        }
        if command_available("screen") {
            // Parity with wcgw: ensure a sane ~/.screenrc and reap sessions whose
            // creating winx process has died before spawning a fresh one.
            ensure_screenrc();
            cleanup_orphaned_screens();
            let mut cmd = CommandBuilder::new("screen");
            cmd.args(["-q", "-S", &session, "bash"]);
            if restricted_mode {
                cmd.arg("-r");
            }
            return (cmd, Some(format!("screen -x {session}")), false);
        }
    }

    let shell = preferred_shell(restricted_mode);
    let is_zsh = shell == "zsh";
    let mut cmd = CommandBuilder::new(&shell);
    // zsh's restricted mode isn't the `-r` flag, so restricted always uses bash -r.
    if restricted_mode && !is_zsh {
        cmd.arg("-r");
    }
    (cmd, None, is_zsh)
}

/// Shell to spawn directly. Defaults to bash; honors `WINX_SHELL=zsh` when zsh is
/// on PATH and we're not in restricted mode (zsh's restricted mode differs from
/// `bash -r`, so restricted falls back to bash).
fn preferred_shell(restricted_mode: bool) -> String {
    if !restricted_mode {
        if let Ok(requested) = std::env::var("WINX_SHELL") {
            if requested == "zsh" && command_available("zsh") {
                return "zsh".to_string();
            }
        }
    }
    "bash".to_string()
}

fn command_available(command: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {command}")])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Create `~/.screenrc` with a large scrollback if the user has none, matching
/// wcgw's `check_if_screen_command_available`. Never overwrites an existing file.
fn ensure_screenrc() {
    let Some(home) = home::home_dir() else {
        return;
    };
    let screenrc = home.join(".screenrc");
    if screenrc.exists() {
        return;
    }
    let _ = std::fs::write(
        &screenrc,
        "defscrollback 10000\ntermcapinfo xterm* ti@:te@\nstartup_message off\n",
    );
}

/// Reap detached `winx-*` screen sessions whose creating process is gone.
///
/// The session name embeds the creator PID (`winx-<pid>-<ts>`), so an orphan is
/// simply a session whose `<pid>` no longer exists — the wcgw equivalent of
/// detecting `parent_pid == 1`. Best-effort: any failure is silently ignored.
fn cleanup_orphaned_screens() {
    let Ok(output) = Command::new("screen").arg("-ls").output() else {
        return;
    };
    // `screen -ls` exits non-zero when sessions exist, so we parse stdout regardless.
    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines() {
        let Some(session) = line.split_whitespace().next() else {
            continue;
        };
        // session token looks like "<screen_pid>.winx-<creator_pid>-<ts>"
        let Some((_, name)) = session.split_once('.') else {
            continue;
        };
        if let Some(creator_pid) = winx_creator_pid(name) {
            if !process_exists(creator_pid) {
                let _ = Command::new("screen").args(["-S", session, "-X", "quit"]).output();
            }
        }
    }
}

/// Extract the creator PID from a `winx-<pid>-<ts>` screen session name.
fn winx_creator_pid(name: &str) -> Option<u32> {
    name.strip_prefix("winx-")?.split('-').next()?.parse::<u32>().ok()
}

/// Whether a process with `pid` is currently alive (Linux `/proc` check).
fn process_exists(pid: u32) -> bool {
    std::path::Path::new("/proc").join(pid.to_string()).exists()
}

fn timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

/// Real PTY-based interactive shell
///
/// Uses portable-pty for true pseudo-terminal functionality,
/// enabling proper handling of interactive programs like sudo, vim, etc.
pub struct PtyShell {
    /// The PTY master handle for resize operations
    master: Box<dyn MasterPty + Send>,
    /// Child process running the shell
    child: Box<dyn Child + Send + Sync>,
    /// Writer for PTY input (taken from master)
    writer: Box<dyn Write + Send>,
    /// Channel receiver for output from reader thread
    output_rx: mpsc::Receiver<String>,
    /// Current terminal size
    size: PtySize,
    /// Last command executed
    pub last_command: String,
    /// Accumulated output buffer
    pub output_buffer: String,
    /// Whether a command is currently running
    pub command_running: bool,
    /// Exit code of the last completed foreground command, parsed from the prompt
    /// marker (`──➤<nonce>:<code>`). `None` until a command finishes, and reset
    /// to `None` while one is running.
    pub last_exit_code: Option<i32>,
    /// Maximum output size before truncation
    max_output_size: usize,
    /// Flag for output truncation
    pub output_truncated: bool,
    /// Bounded scrollback ring of raw PTY lines (newest at the back) plus the
    /// in-flight partial line, with drop accounting so a truncated scrollback is
    /// reported to the caller instead of silently shrinking. See `LineRing`.
    ring: LineRing,
    /// Hash of the last rendered output we shipped to the caller. Used by the
    /// delta path in `status_check` to elide repeats when the screen is idle.
    pub last_returned_hash: Option<u64>,
    /// Optional command a human can run to attach to the same terminal session.
    pub attach_hint: Option<String>,
    /// The exact suffix that ends this shell's prompt: `──➤<nonce>`. The nonce is
    /// a per-shell random value embedded in `PROMPT_COMMAND`, so command output
    /// can't impersonate the prompt (printing `◉ x──➤` no longer ends a command
    /// early). Anchoring completion on this instead of the bare glyphs removes
    /// the false-positive that truncated output when a program echoed them.
    prompt_end_marker: String,
    /// Live terminal emulator (fixed-viewport, `vt100`-backed) fed continuously
    /// by the reader thread. Unlike the scrollback ring (raw lines), it keeps a
    /// consolidated screen grid — cursor moves, redraws, alternate-screen
    /// applied — so a TUI yields a stable, non-stacked snapshot. The fixed
    /// viewport is what lets inline-rendering TUIs (the `agy`/Antigravity CLI,
    /// which redraws with cursor-up + erase-line in the main screen) consolidate
    /// instead of ghosting. See `live_snapshot`.
    live: Arc<StdMutex<LiveTerminal>>,
    /// Workspace root under which output-offload scratch files are written.
    /// `None` until a caller sets it (the workspace is only known after
    /// Initialize, and `execute_command` sets it per command). See
    /// `crate::utils::scratch_file`.
    scratch_workspace_root: Option<PathBuf>,
    /// Scratch file holding the CURRENT command's dropped output head, created
    /// lazily on the first truncation and reset per command.
    scratch_path: Option<PathBuf>,
    /// Bytes already streamed to `scratch_path`, used to enforce `SCRATCH_MAX_BYTES`.
    scratch_bytes: u64,
}

impl std::fmt::Debug for PtyShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyShell")
            .field("size", &format!("{}x{}", self.size.cols, self.size.rows))
            .field("last_command", &self.last_command)
            .field("command_running", &self.command_running)
            .field("output_truncated", &self.output_truncated)
            .field("output_buffer_len", &self.output_buffer.len())
            .field("attach_hint", &self.attach_hint)
            .finish_non_exhaustive()
    }
}

/// Resolve the process group to signal for a PTY child, but ONLY when it is
/// safe: the pid must be a real child (`> 1`) that leads its own group
/// (`getpgid(pid) == pid`). portable-pty `setsid`s the shell, so a live PTY
/// child is always its own group leader; if that ever stopped holding,
/// `kill(-pgid, ...)` would hit some unrelated group, so we refuse instead of
/// guessing.
///
/// The `pid > 1` check is load-bearing: `kill(-1, ...)` means "every process I
/// may signal" and `kill(0, ...)` means "my own group" - either would be
/// catastrophic from a teardown path.
#[cfg(unix)]
fn killable_group(pid: u32) -> Option<i32> {
    let pid = i32::try_from(pid).ok()?;
    if pid <= 1 {
        return None;
    }
    // SAFETY: getpgid(2) reads the group of an existing pid; integer-only, no
    // memory touched. Returns -1 for a dead/invalid pid, which fails the check.
    let group = unsafe { libc::getpgid(pid) };
    (group == pid).then_some(group)
}

/// Send `signal` to the whole process group `pgid` (group-kill via negative
/// pid). `pgid` must be one vetted by [`killable_group`].
#[cfg(unix)]
fn signal_group(pgid: i32, signal: i32) {
    // SAFETY: kill(2) with a negative pid signals a process group; integer-only,
    // no memory touched. `pgid` came from `killable_group`, so it is `> 1`.
    unsafe {
        libc::kill(-pgid, signal);
    }
}

impl Drop for PtyShell {
    /// Kill and reap the shell child **and its whole process group** so neither
    /// the shell nor anything it spawned leaks.
    ///
    /// `std::process::Child::drop` neither kills nor waits, and even
    /// `Child::kill` signals only the shell itself: a background job it started
    /// (`npm run dev &`, a `nohup`'d server) would be reparented to init and live
    /// on. Because portable-pty runs the shell as a session/group leader, we can
    /// signal the whole group with a negative pid - SIGTERM first so well-behaved
    /// children can clean up, then `Child::kill` (SIGKILL on the leader, which
    /// closes the PTY slave so the reader thread's `read()` returns EOF and the
    /// thread exits), then SIGKILL the group to sweep anything still alive, then
    /// reap. The pgid is captured while the leader is alive and reused: it stays
    /// valid until we `wait()`, even after the leader dies. All best-effort.
    fn drop(&mut self) {
        // Only group-kill the direct bash/zsh path. Under screen/tmux (`attach_hint`
        // is `Some`) the `Child` is a multiplexer *client*; the real shell runs in
        // a detached daemon in another process group, so a group-kill here would
        // miss the shell entirely and could disturb unrelated sessions of the same
        // multiplexer. Let the multiplexer own its lifecycle in that case.
        #[cfg(unix)]
        let pgid = if self.attach_hint.is_some() {
            None
        } else {
            self.child.process_id().and_then(killable_group)
        };

        #[cfg(unix)]
        if let Some(pgid) = pgid {
            signal_group(pgid, libc::SIGTERM);
        }

        // SIGKILL the leader and close the PTY slave (unblocks the reader thread).
        let _ = self.child.kill();

        #[cfg(unix)]
        if let Some(pgid) = pgid {
            signal_group(pgid, libc::SIGKILL);
        }

        let _ = self.child.wait();
    }
}

impl PtyShell {
    /// Create a new PTY shell session
    ///
    /// # Arguments
    /// * `initial_dir` - Starting directory for the shell
    /// * `restricted_mode` - Whether to use bash restricted mode (-r)
    ///
    /// # Returns
    /// A new `PtyShell` instance with an active bash session
    pub fn new(initial_dir: &Path, restricted_mode: bool) -> Result<Self> {
        info!(
            "Creating new PTY shell (restricted: {}) in {}",
            restricted_mode,
            initial_dir.display()
        );

        // Initialize the native PTY system
        let pty_system = native_pty_system();

        // Configure terminal size
        let size =
            PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 };

        // Open the PTY pair (master + slave)
        let pair = pty_system.openpty(size).context("Failed to open PTY pair")?;

        // Build the command
        let (mut cmd, attach_hint, is_zsh) = attachable_command(restricted_mode);

        // Set up environment for proper terminal behavior
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("PAGER", "cat");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("COLUMNS", DEFAULT_COLS.to_string());
        cmd.env("ROWS", DEFAULT_ROWS.to_string());
        // Per-shell random nonce embedded in the prompt so command output can't
        // forge the completion marker. 64 bits of entropy in hex.
        let nonce = format!("{:016x}", rand::random::<u64>());
        // WCGW-style prompt for command completion detection
        // Note: removed \r\e[2K which was erasing the prompt before it could be detected
        // `__winx_ec=$?` captures the user command's exit status as the very
        // first thing PROMPT_COMMAND does, then we print it after the marker.
        cmd.env(
            "PROMPT_COMMAND",
            format!(r#"__winx_ec=$?; printf "◉ %s──➤{nonce}:%s " "$PWD" "$__winx_ec""#),
        );
        cmd.cwd(initial_dir);

        // Spawn bash in the PTY slave
        let child = pair.slave.spawn_command(cmd).context("Failed to spawn bash in PTY")?;

        // Get reader and writer from master
        let mut reader = pair.master.try_clone_reader().context("Failed to clone PTY reader")?;
        let writer = pair.master.take_writer().context("Failed to take PTY writer")?;

        // Bounded channel: if the consumer stalls, the reader thread blocks on
        // send (natural backpressure via the PTY OS buffer) instead of growing
        // memory without bound on a runaway producer like `yes`.
        let (output_tx, output_rx) = mpsc::sync_channel::<String>(1024);

        // Live terminal emulator, shared with the reader thread so the screen
        // grid stays current without any consumer needing to poll.
        let live = Arc::new(StdMutex::new(LiveTerminal::new(DEFAULT_ROWS, DEFAULT_COLS)));
        let live_reader = Arc::clone(&live);

        // Spawn a background thread to read from the PTY
        // This prevents blocking the main thread
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // Bytes of an incomplete trailing UTF-8 char held back for the next
            // read. Without this, a multibyte glyph split across a 4096-byte read
            // boundary would decode to two U+FFFDs — corrupting the output buffer
            // and the prompt/CWD detection that runs over it.
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF - PTY closed. Flush any held bytes lossily.
                        if !carry.is_empty() {
                            let _ = output_tx.send(String::from_utf8_lossy(&carry).into_owned());
                        }
                        break;
                    }
                    Ok(n) => {
                        // Tap the raw bytes into the live emulator first (brief
                        // lock; feed is O(chunk len)). Feeding bytes — not the
                        // lossy String — keeps the persistent VTE parser exact
                        // across chunk boundaries. Recover from a poisoned lock
                        // instead of dropping the feed silently.
                        {
                            let mut emu = live_reader
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            emu.feed(&buf[..n]);
                        }
                        // Decode the longest valid prefix; carry an incomplete
                        // trailing char into the next read.
                        carry.extend_from_slice(&buf[..n]);
                        let (chunk, rest) = decode_keep_incomplete(&carry);
                        carry = rest;
                        if !chunk.is_empty() && output_tx.send(chunk).is_err() {
                            // Receiver dropped, exit thread
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("PTY reader thread error: {}", e);
                        break;
                    }
                }
            }
            debug!("PTY reader thread exiting");
        });

        // Create the shell instance
        let mut shell = Self {
            master: pair.master,
            child,
            writer,
            output_rx,
            size,
            last_command: String::new(),
            output_buffer: String::new(),
            command_running: false,
            last_exit_code: None,
            max_output_size: MAX_OUTPUT_SIZE,
            output_truncated: false,
            ring: LineRing::new(RING_BUFFER_LINES, MAX_PARTIAL_LINE_BYTES),
            last_returned_hash: None,
            attach_hint,
            prompt_end_marker: format!("{WCGW_PROMPT_END}{nonce}"),
            live,
            scratch_workspace_root: None,
            scratch_path: None,
            scratch_bytes: 0,
        };

        // Initialize the shell with WCGW-style prompt
        shell.initialize_prompt(is_zsh, &nonce)?;

        debug!("PTY shell created successfully");
        Ok(shell)
    }

    /// Initialize the shell prompt for WCGW compatibility
    fn initialize_prompt(&mut self, is_zsh: bool, nonce: &str) -> Result<()> {
        // Set up the dynamic prompt - matches WCGW Python PROMPT_STATEMENT.
        // zsh ignores PROMPT_COMMAND, so it gets a precmd hook with a blanked
        // default prompt instead.
        // Blank the shell's own PS1/PROMPT so the line ends exactly at our `──➤`
        // marker. Otherwise a user's ~/.bashrc/~/.zshrc PS1 (e.g. `[user@host]$`)
        // is appended after the marker, and prompt detection — which anchors on a
        // trailing `──➤` — only fires when the chunk happens to fragment right
        // before the PS1, making command-completion detection flaky.
        let prompt_statement = if is_zsh {
            // Clear precmd_functions too: frameworks (oh-my-zsh/p10k) register the
            // prompt via that array, so redefining `precmd` alone leaves their
            // prompt in place and our `──➤` marker never ends the line.
            format!(
                r#"export GIT_PAGER=cat PAGER=cat; precmd_functions=(); preexec_functions=(); PROMPT=''; RPROMPT=''; precmd() {{ local __winx_ec=$?; printf "◉ %s──➤{nonce}:%s " "$PWD" "$__winx_ec" }}"#
            )
        } else {
            format!(
                r#"export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND='__winx_ec=$?; printf "◉ %s──➤{nonce}:%s " "$PWD" "$__winx_ec"'; PS1=''"#
            )
        };

        self.write_command(&prompt_statement)?;

        // Wait for prompt to be ready
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.drain_output();

        Ok(())
    }

    /// Write a command to the PTY, submitting it with a carriage return.
    fn write_command(&mut self, command: &str) -> Result<()> {
        // Submit with `\r`, matching the foreground path (`send_special_key("Enter")`
        // also sends `\r`) and a real terminal's Enter key. The two paths used to
        // disagree — background sent `\n` here, foreground sent `\r` — which is a
        // real difference for readline-based TUIs. bash's canonical line discipline
        // maps CR to NL (ICRNL), so plain commands behave identically.
        let cmd_with_newline = format!("{command}\r");
        self.writer.write_all(cmd_with_newline.as_bytes()).context("Failed to write to PTY")?;
        self.writer.flush().context("Failed to flush PTY")?;
        Ok(())
    }

    /// Drain any pending output from the PTY channel
    fn drain_output(&mut self) -> String {
        let mut output = String::new();
        let deadline = Instant::now() + Duration::from_millis(200);

        // Drain all available output from the channel
        while Instant::now() < deadline {
            match self.output_rx.try_recv() {
                Ok(chunk) => {
                    output.push_str(&chunk);

                    // Prevent runaway reads
                    if output.len() > self.max_output_size {
                        self.output_truncated = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => {
                    // No more data, wait briefly for more
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => {
                    // Reader thread died
                    break;
                }
            }
        }

        output
    }

    /// Drain any pending output and, if a previous command still seems to be
    /// running, send a Ctrl-C to flush it. Mirrors wcgw's `clear_to_run` so a
    /// new command never inherits stale prompt fragments or a half-typed line.
    ///
    /// Returns `true` if the shell looks idle (prompt seen), `false` if it
    /// still wouldn't yield after the Ctrl-C — caller may want to reset.
    pub fn clear_to_run(&mut self, max_wait_secs: f32) -> Result<bool> {
        // Drain whatever is in the channel without blocking. Use the existing
        // read_output to also catch the prompt fingerprint.
        let (_, complete) = self.read_output(max_wait_secs.min(0.5))?;
        if complete {
            return Ok(true);
        }

        // Something is still running — interrupt it.
        debug!("clear_to_run: prompt not seen, sending Ctrl+C");
        self.send_interrupt()?;

        // Re-drain after the interrupt so the next command starts on a clean prompt.
        let (_, drained) = self.read_output(max_wait_secs)?;
        Ok(drained)
    }

    /// Send a command to the shell and start reading output
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        debug!("PTY sending command: {}", command);

        // Clear previous state
        self.output_buffer.clear();
        self.output_truncated = false;
        self.reset_scratch();
        self.last_command = command.to_string();
        self.command_running = true;
        self.last_exit_code = None;
        // A new command means the next status_check should return whatever
        // shows up — drop the dedup hash so we don't elide the first response.
        self.last_returned_hash = None;

        // Write the command
        self.write_command(command)?;

        Ok(())
    }

    /// Return up to `lines` recent lines from the scrollback ring, oldest
    /// first. When older lines were evicted or a long unterminated line was
    /// clipped, a `[winx: …]` notice is prepended so the caller knows the
    /// scrollback is incomplete rather than silently receiving less than asked.
    pub fn collect_scrollback(&self, lines: usize) -> String {
        // The ring holds raw PTY lines. Replay them through the terminal
        // emulator so cursor movements (readline echo, in-place redraws) are
        // *applied* — not merely stripped — yielding what the screen actually
        // showed. A plain strip can't undo a `\x1b[D`, so it would leave
        // `>>> p>>> pr>>> pri...` echo noise behind; the emulator collapses it.
        let rendered =
            crate::state::terminal::render_terminal_output(&self.ring.raw(lines)).join("\n");
        match self.ring.scrollback_notice(lines) {
            Some(notice) => format!("{notice}\n{rendered}"),
            None => rendered,
        }
    }

    /// Hash arbitrary rendered output into a u64 dedup key.
    pub fn fingerprint(text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Read output from the PTY with timeout
    ///
    /// Returns (output, `is_complete`) tuple where `is_complete` indicates
    /// whether the command has finished (prompt detected)
    pub fn read_output(&mut self, timeout_secs: f32) -> Result<(String, bool)> {
        let timeout = Duration::from_secs_f32(timeout_secs.clamp(0.1, 60.0));
        let start = Instant::now();
        let mut complete = false;
        let mut no_data_count = 0;
        let mut prompt_detected_at: Option<Instant> = None;

        while start.elapsed() < timeout {
            match self.output_rx.try_recv() {
                Ok(chunk) => {
                    self.output_buffer.push_str(&chunk);
                    self.ring.push_chunk(&chunk);
                    no_data_count = 0;

                    // Check for WCGW prompt indicating command completion
                    if prompt_detected_at.is_none()
                        && (Self::check_prompt_complete(&chunk, &self.prompt_end_marker)
                            || Self::check_prompt_complete(
                                &self.output_buffer,
                                &self.prompt_end_marker,
                            ))
                    {
                        prompt_detected_at = Some(Instant::now());
                        debug!("Prompt detected, draining remaining output...");
                    }

                    // Truncate if too large (offloads the dropped head to scratch).
                    self.truncate_output_buffer_with_offload();
                }
                Err(TryRecvError::Empty) => {
                    // No data available, wait briefly
                    thread::sleep(Duration::from_millis(10));
                    no_data_count += 1;

                    // If prompt was detected, check if we've drained long enough
                    if let Some(detected_time) = prompt_detected_at {
                        // Wait 100ms after prompt detection to capture any trailing output
                        if detected_time.elapsed() > Duration::from_millis(100) {
                            complete = true;
                            debug!("Command completed - prompt detected and drained");
                            break;
                        }
                    } else if no_data_count > 10
                        && Self::check_prompt_complete(&self.output_buffer, &self.prompt_end_marker)
                    {
                        // Prompt detected during empty reads
                        prompt_detected_at = Some(Instant::now());
                        debug!("Prompt detected after wait, draining...");
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    // Reader thread died - PTY closed
                    warn!("PTY reader disconnected");
                    complete = true;
                    break;
                }
            }
        }

        if complete || prompt_detected_at.is_some() {
            self.command_running = false;
            self.last_exit_code =
                Self::parse_prompt_exit_code(&self.output_buffer, &self.prompt_end_marker);
            complete = true;
        }

        Ok((self.output_buffer.clone(), complete))
    }

    /// Non-blocking drain of whatever the reader thread has already queued.
    ///
    /// Unlike [`read_output`](Self::read_output), this never sleeps: it consumes
    /// every chunk currently available, updates prompt/completion state, and
    /// returns immediately. `prune_finished_shells` uses it so the global
    /// background-shell lock is never held across a blocking 100ms `read_output`.
    /// Returns `true` if the command has completed (prompt seen or PTY closed).
    pub fn poll_output_nonblocking(&mut self) -> bool {
        let mut prompt_seen = false;
        loop {
            match self.output_rx.try_recv() {
                Ok(chunk) => {
                    let chunk_has_prompt =
                        Self::check_prompt_complete(&chunk, &self.prompt_end_marker);
                    self.output_buffer.push_str(&chunk);
                    self.ring.push_chunk(&chunk);
                    if chunk_has_prompt
                        || Self::check_prompt_complete(&self.output_buffer, &self.prompt_end_marker)
                    {
                        prompt_seen = true;
                    }
                    self.truncate_output_buffer_with_offload();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.command_running = false;
                    return true;
                }
            }
        }
        if prompt_seen {
            self.command_running = false;
            self.last_exit_code =
                Self::parse_prompt_exit_code(&self.output_buffer, &self.prompt_end_marker);
        }
        prompt_seen
    }

    /// Check if the output ends with this shell's prompt.
    ///
    /// `prompt_end` is the per-shell `──➤<nonce>` suffix. Anchoring on the LAST
    /// non-empty line (not a global `contains`) plus the random nonce means
    /// command output that happens to print the prompt glyphs mid-stream — or
    /// even a forged `◉ x──➤ ` at end of line — can't be mistaken for the prompt
    /// and truncate output or end the command early.
    fn check_prompt_complete(text: &str, prompt_end: &str) -> bool {
        Self::parse_prompt_exit_code(text, prompt_end).is_some()
    }

    /// If the last non-empty line is this shell's completion prompt
    /// (`◉ <pwd>──➤<nonce>:<code>`), return the exit code that follows the
    /// marker. Returns `None` when the prompt isn't present — which is exactly
    /// "the command is still running". The per-shell nonce plus the structured
    /// `:<digits>` suffix keep command output from forging completion.
    fn parse_prompt_exit_code(text: &str, prompt_end: &str) -> Option<i32> {
        let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
        // Strip ANSI so a trailing erase/cursor sequence after the code
        // (e.g. ":0 \x1b[K") doesn't defeat the suffix check.
        let clean = crate::state::terminal::strip_ansi_codes(last);
        let clean = clean.trim_end();
        if !clean.contains(WCGW_PROMPT_PATTERN) {
            return None;
        }
        // Everything after the last "──➤<nonce>" must be ":<digits>" and nothing
        // else — this confirms the prompt ends the line AND yields the code.
        let after = clean.rsplit_once(prompt_end)?.1;
        after.strip_prefix(':')?.trim().parse::<i32>().ok()
    }

    /// Point output-offload scratch files at `root` (the workspace). Idempotent;
    /// callers set it per command so it always tracks the current workspace.
    pub fn set_scratch_root(&mut self, root: &Path) {
        self.scratch_workspace_root = Some(root.to_path_buf());
    }

    /// The scratch file holding the current command's dropped output head, if any
    /// was offloaded this command.
    pub fn scratch_path(&self) -> Option<&Path> {
        self.scratch_path.as_deref()
    }

    /// Forget the current command's scratch file so the next command starts a
    /// fresh one. Does not delete the file (pruned later by age).
    pub fn reset_scratch(&mut self) {
        self.scratch_path = None;
        self.scratch_bytes = 0;
    }

    /// Append a just-dropped output `head` to the per-command scratch file so the
    /// agent can recover it via `ReadFiles`. Best-effort: needs a known workspace
    /// root, stops at `SCRATCH_MAX_BYTES`, and swallows IO errors.
    fn offload_dropped_head(&mut self, head: &str) {
        let Some(root) = self.scratch_workspace_root.clone() else {
            return;
        };
        // Clamp to the remaining byte budget so the file honors SCRATCH_MAX_BYTES
        // exactly (a single head can be up to keep_size, ~500 KB). Snap to a char
        // boundary so we never slice mid-UTF-8.
        let remaining = SCRATCH_MAX_BYTES.saturating_sub(self.scratch_bytes);
        if remaining == 0 {
            return;
        }
        let head = match usize::try_from(remaining) {
            Ok(rem) if rem < head.len() => &head[..crate::utils::floor_char_boundary(head, rem)],
            _ => head,
        };
        if self.scratch_path.is_none() {
            self.scratch_path = crate::utils::scratch_file::new_scratch_path(&root);
        }
        let Some(path) = self.scratch_path.clone() else {
            return;
        };
        match crate::utils::scratch_file::append_scratch(&path, head.as_bytes()) {
            Ok(()) => self.scratch_bytes = self.scratch_bytes.saturating_add(head.len() as u64),
            Err(e) => debug!("scratch: append to {} failed: {e}", path.display()),
        }
    }

    /// Once `output_buffer` exceeds the cap, offload the head about to be dropped
    /// to the scratch file, then keep only the tail with a truncation marker.
    /// Shared by the blocking and non-blocking drain paths so the offload and the
    /// char-boundary handling live in one place.
    fn truncate_output_buffer_with_offload(&mut self) {
        if self.output_buffer.len() <= self.max_output_size {
            return;
        }
        self.output_truncated = true;
        let keep_size = self.max_output_size / 2;
        // Snap to a char boundary: a raw byte offset can land mid-UTF-8
        // (CJK/emoji/box-drawing/the prompt glyphs) and slicing there would panic.
        let cut = crate::utils::floor_char_boundary(
            &self.output_buffer,
            self.output_buffer.len() - keep_size,
        );
        let head = self.output_buffer[..cut].to_owned();
        self.offload_dropped_head(&head);
        self.output_buffer = format!("\n(...output truncated...)\n{}", &self.output_buffer[cut..]);
    }

    /// Send Ctrl+C (interrupt) to the PTY
    pub fn send_interrupt(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+C");
        self.writer
            .write_all(&[0x03]) // ASCII ETX (Ctrl+C)
            .context("Failed to send Ctrl+C")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send Ctrl+D (EOF) to the PTY
    pub fn send_eof(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+D");
        self.writer
            .write_all(&[0x04]) // ASCII EOT (Ctrl+D)
            .context("Failed to send Ctrl+D")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send Ctrl+Z (suspend) to the PTY
    pub fn send_suspend(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+Z");
        self.writer
            .write_all(&[0x1A]) // ASCII SUB (Ctrl+Z)
            .context("Failed to send Ctrl+Z")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send text directly to the PTY (for interactive input)
    pub fn send_text(&mut self, text: &str) -> Result<()> {
        debug!("PTY sending text: {:?}", text);
        self.send_bytes(text.as_bytes()).context("Failed to send text")?;
        Ok(())
    }

    /// Send raw bytes directly to the PTY.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).context("Failed to send bytes")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send a special key sequence
    pub fn send_special_key(&mut self, key: &str) -> Result<()> {
        let bytes: &[u8] = match key {
            "Enter" => b"\r",
            "Tab" => b"\t",
            "Backspace" => b"\x7F",
            "Escape" => b"\x1B",
            "Up" | "KeyUp" => b"\x1B[A",
            "Down" | "KeyDown" => b"\x1B[B",
            "Right" | "KeyRight" => b"\x1B[C",
            "Left" | "KeyLeft" => b"\x1B[D",
            "Home" => b"\x1B[H",
            "End" => b"\x1B[F",
            "PageUp" => b"\x1B[5~",
            "PageDown" => b"\x1B[6~",
            "Delete" => b"\x1B[3~",
            "Insert" => b"\x1B[2~",
            "CtrlC" | "Ctrl-C" => b"\x03",
            "CtrlD" | "Ctrl-D" => b"\x04",
            "CtrlZ" | "Ctrl-Z" => b"\x1A",
            "CtrlL" | "Ctrl-L" => b"\x0C",
            _ => return Err(anyhow!("Unknown special key: {key}")),
        };

        debug!("PTY sending special key: {} ({:?})", key, bytes);
        self.send_bytes(bytes)?;
        Ok(())
    }

    /// Snapshot the live terminal screen — a stable, consolidated view of what a
    /// human would currently see, with cursor moves/redraws applied and ANSI
    /// stripped. `max_lines` of 0 returns the full screen buffer. This is the
    /// foundation for piloting interactive TUIs (the `claude` CLI, vim, ...).
    pub fn live_snapshot(&self, max_lines: usize) -> Vec<String> {
        // Recover from a poisoned lock instead of returning a blank screen
        // forever — a single panic while holding `live` would otherwise wedge
        // every future snapshot (and `wait_for_turn`/`screen` with it).
        self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner).snapshot(max_lines)
    }

    /// Whether the live terminal is currently on the alternate screen buffer
    /// (a full-screen app like vim/htop/less is running).
    pub fn live_in_alt_screen(&self) -> bool {
        self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner).in_alt_screen()
    }

    /// Cursor position `(row, col)` on the live screen (0-based), so a piloting
    /// agent knows where focus is in a menu/form. `(0, 0)` if the lock is poisoned.
    pub fn live_cursor_position(&self) -> (u16, u16) {
        self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cursor_position()
    }

    /// Diff the live screen against what the client last saw, returning only the
    /// changed lines when the change is small (huge token savings over many
    /// polls). Updates the baseline as a side effect. See [`ScreenUpdate`].
    pub fn live_snapshot_diff(&self, max_lines: usize, threshold: usize) -> ScreenUpdate {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot_diff(max_lines, threshold)
    }

    /// Resize the terminal
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        debug!("PTY resizing to {}x{}", cols, rows);

        let new_size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };

        self.master.resize(new_size).context("Failed to resize PTY")?;

        // Keep the live emulator's viewport in lockstep with the PTY size.
        self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resize(rows, cols);

        self.size = new_size;
        Ok(())
    }

    /// Get current terminal size
    pub fn get_size(&self) -> (u16, u16) {
        (self.size.cols, self.size.rows)
    }

    /// Check if the shell is still alive
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
    }
}

/// Thread-safe wrapper for `PtyShell`
pub type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

/// Create a new shared PTY shell
pub fn create_shared_pty(initial_dir: &Path, restricted_mode: bool) -> Result<SharedPtyShell> {
    let shell = PtyShell::new(initial_dir, restricted_mode)?;
    Ok(Arc::new(Mutex::new(Some(shell))))
}

/// Split a byte slice into the longest valid-UTF-8 prefix (decoded) and the
/// trailing bytes of an incomplete final char (to carry into the next read).
/// Genuinely invalid sequences become a single U+FFFD and are consumed, so the
/// carry only ever holds a recoverable incomplete tail and never grows unbounded.
///
/// Iterative on purpose: a recursive version blows the reader thread's stack on
/// a burst of invalid bytes (e.g. `cat`-ing a binary), which aborts under
/// `panic = "abort"`. This stays O(1) in stack depth regardless of input.
fn decode_keep_incomplete(bytes: &[u8]) -> (String, Vec<u8>) {
    let mut decoded = String::new();
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                decoded.push_str(text);
                return (decoded, Vec::new());
            }
            Err(error) => {
                let valid = error.valid_up_to();
                // `rest[..valid]` is valid UTF-8 by construction (no allocation
                // for the borrowed case).
                decoded.push_str(&String::from_utf8_lossy(&rest[..valid]));
                match error.error_len() {
                    // Incomplete final char: keep the tail for the next read.
                    None => return (decoded, rest[valid..].to_vec()),
                    // Genuinely invalid run: emit one replacement and skip past it.
                    Some(bad) => {
                        decoded.push('\u{FFFD}');
                        rest = &rest[valid + bad..];
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn decode_passes_clean_utf8_through() {
        let (text, carry) = decode_keep_incomplete("hello ✓ café".as_bytes());
        assert_eq!(text, "hello ✓ café");
        assert!(carry.is_empty());
    }

    #[test]
    fn decode_carries_incomplete_trailing_char_across_reads() {
        // "é" is 0xC3 0xA9; a read boundary splits it. The first half must NOT
        // turn into U+FFFD — it's held and completed on the next read.
        let (text1, carry1) = decode_keep_incomplete(&[b'a', 0xC3]);
        assert_eq!(text1, "a");
        assert_eq!(carry1, vec![0xC3]);

        let mut next = carry1;
        next.push(0xA9); // continuation byte arrives in the next read
        let (text2, carry2) = decode_keep_incomplete(&next);
        assert_eq!(text2, "é");
        assert!(carry2.is_empty());
    }

    #[test]
    fn decode_replaces_genuinely_invalid_bytes() {
        // 0xFF is never valid UTF-8 and is not an incomplete prefix.
        let (text, carry) = decode_keep_incomplete(&[b'x', 0xFF, b'y']);
        assert_eq!(text, "x\u{FFFD}y");
        assert!(carry.is_empty());
    }

    #[test]
    fn decode_handles_large_invalid_burst_without_overflow() {
        // Regression: the first (recursive) version blew the stack — one frame
        // per invalid byte — and aborted under panic="abort" on a binary dump.
        // The iterative version must handle far more than the 4KB reader buffer.
        let big = vec![0xFFu8; 200_000];
        let (text, carry) = decode_keep_incomplete(&big);
        assert!(carry.is_empty());
        assert_eq!(text.chars().filter(|&c| c == '\u{FFFD}').count(), 200_000);
    }

    #[test]
    fn prompt_detection_is_suffix_anchored() {
        // Per-shell nonce + exit code after the arrow: `──➤<nonce>:<code>`.
        let end = "──➤deadbeefcafe0001";
        // real prompt on the last line -> complete
        assert!(PtyShell::check_prompt_complete("out\nmore\n◉ /home/x──➤deadbeefcafe0001:0 ", end));
        // prompt with trailing ANSI erase -> still complete
        assert!(PtyShell::check_prompt_complete("◉ /home/x──➤deadbeefcafe0001:0 \u{1b}[K", end));
        // the bug: ◉ and ──➤ appear MID-output, last line is normal -> NOT complete
        assert!(!PtyShell::check_prompt_complete(
            "menu: ◉ start ──➤deadbeefcafe0001 stop\nstill running",
            end
        ));
        // command echoed after the prompt (not the waiting prompt) -> not complete
        assert!(!PtyShell::check_prompt_complete("◉ /home/x──➤deadbeefcafe0001:0 ls -la", end));
        // no prompt at all
        assert!(!PtyShell::check_prompt_complete("just some output\n", end));
        // the nonce's payoff: output that forges the bare glyphs but NOT the
        // nonce (or omits the exit code) must not be mistaken for the prompt.
        assert!(!PtyShell::check_prompt_complete("◉ /fake──➤ ", end));
        assert!(!PtyShell::check_prompt_complete("◉ /fake──➤wrongnonce:0 ", end));
        assert!(!PtyShell::check_prompt_complete("◉ /home/x──➤deadbeefcafe0001 ", end));
    }

    #[test]
    fn parses_exit_code_from_prompt_marker() {
        let end = "──➤cafe1234";
        assert_eq!(PtyShell::parse_prompt_exit_code("ok\n◉ /x──➤cafe1234:0 ", end), Some(0));
        assert_eq!(PtyShell::parse_prompt_exit_code("oops\n◉ /x──➤cafe1234:1 ", end), Some(1));
        assert_eq!(PtyShell::parse_prompt_exit_code("◉ /x──➤cafe1234:42 ", end), Some(42));
        // still running (no completion prompt yet) -> None
        assert_eq!(PtyShell::parse_prompt_exit_code("building...", end), None);
        // forged with the wrong nonce -> None
        assert_eq!(PtyShell::parse_prompt_exit_code("◉ /x──➤wrong:0 ", end), None);
    }

    use proptest::prelude::*;

    proptest! {
        // Exit-code parsing runs on untrusted program output — arbitrary text
        // (and a forgeable marker) must never panic, only ever return None or a
        // parsed i32.
        #[test]
        fn parse_prompt_exit_code_never_panics(
            text in prop::collection::vec(any::<char>(), 0..256),
            nonce in "[0-9a-f]{0,16}",
        ) {
            let s: String = text.into_iter().collect();
            let end = format!("{WCGW_PROMPT_END}{nonce}");
            // Must not panic; any Option<i32> is acceptable.
            let _ = PtyShell::parse_prompt_exit_code(&s, &end);
        }

        // A well-formed marker round-trips the embedded code for any i32.
        #[test]
        fn parse_prompt_exit_code_round_trips(code in any::<i32>(), nonce in "[0-9a-f]{1,16}") {
            let end = format!("{WCGW_PROMPT_END}{nonce}");
            let line = format!("output line\n{WCGW_PROMPT_PATTERN} /home/x{end}:{code} ");
            prop_assert_eq!(PtyShell::parse_prompt_exit_code(&line, &end), Some(code));
        }
    }

    #[test]
    fn test_pty_shell_creation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let result = PtyShell::new(temp_dir.path(), false);
        assert!(result.is_ok(), "Failed to create PTY shell: {:?}", result.err());
        Ok(())
    }

    #[test]
    fn test_pty_shell_echo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let mut shell = PtyShell::new(temp_dir.path(), false)?;

        shell.send_command("echo 'hello pty'")?;
        let (output, _complete) = shell.read_output(2.0)?;

        assert!(output.contains("hello pty"), "Output should contain 'hello pty': {output}");
        Ok(())
    }

    #[test]
    fn test_pty_shell_pwd() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let mut shell = PtyShell::new(temp_dir.path(), false)?;

        // Simply verify shell responds to pwd command
        // Use single quotes like echo test for consistency
        shell.send_command("pwd && echo 'pwd_done'")?;
        let (output, _complete) = shell.read_output(2.0)?;

        // Verify the echo marker appears (proves command executed)
        assert!(output.contains("pwd_done"), "Output should contain 'pwd_done': {output}");
        Ok(())
    }

    #[test]
    fn test_pty_resize() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let mut shell = PtyShell::new(temp_dir.path(), false)?;

        let result = shell.resize(120, 40);
        assert!(result.is_ok());

        let (cols, rows) = shell.get_size();
        assert_eq!(cols, 120);
        assert_eq!(rows, 40);
        Ok(())
    }

    /// `kill(pid, 0)` probes existence without sending a signal: 0 = the pid
    /// exists, -1 (ESRCH) = it's gone. A freshly-killed orphan lingers briefly
    /// as a zombie under init until reaped, so callers poll.
    #[cfg(unix)]
    fn pid_alive(pid: i32) -> bool {
        // SAFETY: kill(2) with signal 0 only checks permission/existence for an
        // integer pid; it sends nothing and touches no memory.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_shell_kills_orphaned_background_child() -> Result<()> {
        // A background job the shell starts must not outlive the shell. Spawn a
        // long `sleep` in the background, record its pid, drop the shell, and
        // confirm the sleep is gone - proving the group-kill reached past the
        // shell to a child `Child::kill` alone would have orphaned.
        let dir = TempDir::new()?;
        let pidfile = dir.path().join("child.pid");
        let mut shell = PtyShell::new(dir.path(), false)?;

        shell
            .send_command(&format!("sleep 300 & echo $! > {} ; echo spawned", pidfile.display()))?;
        let (out, _) = shell.read_output(3.0)?;
        assert!(out.contains("spawned"), "command did not run: {out}");

        let child_pid: i32 = std::fs::read_to_string(&pidfile)?
            .trim()
            .parse()
            .map_err(|e| anyhow!("bad pid file: {e}"))?;
        assert!(pid_alive(child_pid), "background child should be running before drop");

        drop(shell);

        // The group-kill plus init reaping the orphan are async w.r.t. us; poll.
        let mut gone = false;
        for _ in 0..150 {
            if !pid_alive(child_pid) {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(gone, "background child {child_pid} survived shell teardown (orphan leak)");
        Ok(())
    }
}
