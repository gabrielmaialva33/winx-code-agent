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
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::state::terminal::TerminalEmulator;

/// Default terminal dimensions (columns x rows)
pub const DEFAULT_COLS: u16 = 200;
pub const DEFAULT_ROWS: u16 = 50;

/// Maximum output buffer size to prevent memory issues
const MAX_OUTPUT_SIZE: usize = 1_000_000;

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
    /// Maximum output size before truncation
    max_output_size: usize,
    /// Flag for output truncation
    pub output_truncated: bool,
    /// Rolling buffer of fully-emitted lines for opt-in scrollback. The newest
    /// line is at the back; capped at `RING_BUFFER_LINES`.
    pub line_ring: VecDeque<String>,
    /// Carries the unterminated tail across reads so partial lines aren't
    /// double-counted when more bytes arrive.
    line_ring_partial: String,
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
    /// Live terminal emulator fed continuously by the reader thread. Unlike the
    /// scrollback ring (raw lines), this keeps a consolidated screen grid —
    /// cursor moves, redraws, alternate-screen and synchronized-output applied —
    /// so a TUI (the `claude` CLI, vim, htop) yields a stable, non-stacked
    /// snapshot. See `live_snapshot`.
    live: Arc<StdMutex<TerminalEmulator>>,
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

impl Drop for PtyShell {
    /// Kill and reap the shell child so it doesn't leak.
    ///
    /// `std::process::Child::drop` neither kills nor waits, so without this every
    /// dropped shell (`reset_shell`, background-shell prune/remove) would leak a
    /// live bash process — soon a zombie — plus the reader thread blocked in
    /// `read()`. Killing the child closes the PTY slave, which makes the reader's
    /// `read()` return EOF so the thread terminates on its own. Best-effort.
    fn drop(&mut self) {
        let _ = self.child.kill();
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
        cmd.env("PROMPT_COMMAND", format!(r#"printf "◉ %s──➤{nonce} " "$PWD""#));
        cmd.cwd(initial_dir);

        // Spawn bash in the PTY slave
        let child = pair.slave.spawn_command(cmd).context("Failed to spawn bash in PTY")?;

        // Get reader and writer from master
        let mut reader = pair.master.try_clone_reader().context("Failed to clone PTY reader")?;
        let writer = pair.master.take_writer().context("Failed to take PTY writer")?;

        // Create channel for output from reader thread
        let (output_tx, output_rx) = mpsc::channel::<String>();

        // Live terminal emulator, shared with the reader thread so the screen
        // grid stays current without any consumer needing to poll.
        let live = Arc::new(StdMutex::new(TerminalEmulator::new(DEFAULT_COLS as usize)));
        let live_reader = Arc::clone(&live);

        // Spawn a background thread to read from the PTY
        // This prevents blocking the main thread
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF - PTY closed
                        break;
                    }
                    Ok(n) => {
                        // Tap the raw bytes into the live emulator first (brief
                        // lock; feed is O(chunk len)). Feeding bytes — not the
                        // lossy String — keeps the persistent VTE parser exact
                        // across chunk boundaries.
                        if let Ok(mut emu) = live_reader.lock() {
                            emu.feed(&buf[..n]);
                        }
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if output_tx.send(chunk).is_err() {
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
            max_output_size: MAX_OUTPUT_SIZE,
            output_truncated: false,
            line_ring: VecDeque::with_capacity(RING_BUFFER_LINES),
            line_ring_partial: String::new(),
            last_returned_hash: None,
            attach_hint,
            prompt_end_marker: format!("{WCGW_PROMPT_END}{nonce}"),
            live,
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
                r#"export GIT_PAGER=cat PAGER=cat; precmd_functions=(); preexec_functions=(); PROMPT=''; RPROMPT=''; precmd() {{ printf "◉ %s──➤{nonce} " "$PWD" }}"#
            )
        } else {
            format!(
                r#"export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND='printf "◉ %s──➤{nonce} " "$PWD"'; PS1=''"#
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
        self.last_command = command.to_string();
        self.command_running = true;
        // A new command means the next status_check should return whatever
        // shows up — drop the dedup hash so we don't elide the first response.
        self.last_returned_hash = None;

        // Write the command
        self.write_command(command)?;

        Ok(())
    }

    /// Push freshly-arrived bytes through the line-oriented ringbuffer so
    /// callers can request bounded scrollback later.
    fn ingest_into_ring(&mut self, chunk: &str) {
        let combined = if self.line_ring_partial.is_empty() {
            chunk.to_string()
        } else {
            let mut s = std::mem::take(&mut self.line_ring_partial);
            s.push_str(chunk);
            s
        };

        let mut last_nl_end: Option<usize> = None;
        for (idx, ch) in combined.char_indices() {
            if ch == '\n' {
                let end = idx + ch.len_utf8();
                let start = last_nl_end.unwrap_or(0);
                // Keep the raw line (CR/cursor moves intact); the emulator in
                // collect_scrollback replays them. Only drop a trailing CR (CRLF).
                let line = combined[start..idx].trim_end_matches('\r').to_string();
                if self.line_ring.len() == RING_BUFFER_LINES {
                    self.line_ring.pop_front();
                }
                self.line_ring.push_back(line);
                last_nl_end = Some(end);
            }
        }

        if let Some(end) = last_nl_end {
            self.line_ring_partial = combined[end..].to_string();
        } else {
            self.line_ring_partial = combined;
        }

        // A stream that never emits a newline (a `\r`-only progress bar, a binary
        // blob, `yes | tr -d '\n'`) would grow the partial without bound. Keep
        // only the tail; older bytes of an unterminated line aren't useful as
        // scrollback. Snap to a char boundary so we never split a code point.
        if self.line_ring_partial.len() > MAX_PARTIAL_LINE_BYTES {
            let cut = self.line_ring_partial.len() - MAX_PARTIAL_LINE_BYTES;
            let cut = crate::utils::floor_char_boundary(&self.line_ring_partial, cut);
            self.line_ring_partial.drain(..cut);
        }
    }

    /// Return up to `lines` recent lines from the ringbuffer, oldest first.
    /// Includes any in-flight partial line.
    pub fn collect_scrollback(&self, lines: usize) -> String {
        if lines == 0 {
            return String::new();
        }
        let start = self.line_ring.len().saturating_sub(lines);
        let mut out = String::new();
        for line in self.line_ring.iter().skip(start) {
            out.push_str(line);
            out.push('\n');
        }
        if !self.line_ring_partial.is_empty() {
            out.push_str(&self.line_ring_partial);
        }
        // The ring holds raw PTY lines. Replay them through the terminal emulator
        // so cursor movements (readline echo, in-place redraws) are *applied* —
        // not merely stripped — yielding what the screen actually showed. A plain
        // strip can't undo a `\x1b[D`, so it would leave `>>> p>>> pr>>> pri...`
        // echo noise behind; the emulator collapses it to the final line.
        crate::state::terminal::render_terminal_output(&out).join("\n")
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
                    self.ingest_into_ring(&chunk);
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

                    // Truncate if too large
                    if self.output_buffer.len() > self.max_output_size {
                        self.output_truncated = true;
                        let truncate_msg = "\n(...output truncated...)\n";
                        let keep_size = self.max_output_size / 2;
                        // Snap the cut up to a char boundary: a raw byte offset can
                        // land mid-UTF-8 (CJK/emoji/box-drawing/the prompt glyphs)
                        // and slicing there would panic on this hot read path.
                        let mut cut = self.output_buffer.len() - keep_size;
                        while cut < self.output_buffer.len()
                            && !self.output_buffer.is_char_boundary(cut)
                        {
                            cut += 1;
                        }
                        self.output_buffer =
                            format!("{truncate_msg}{}", &self.output_buffer[cut..]);
                    }
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
            complete = true;
        }

        Ok((self.output_buffer.clone(), complete))
    }

    /// Check if the output ends with this shell's prompt.
    ///
    /// `prompt_end` is the per-shell `──➤<nonce>` suffix. Anchoring on the LAST
    /// non-empty line (not a global `contains`) plus the random nonce means
    /// command output that happens to print the prompt glyphs mid-stream — or
    /// even a forged `◉ x──➤ ` at end of line — can't be mistaken for the prompt
    /// and truncate output or end the command early.
    fn check_prompt_complete(text: &str, prompt_end: &str) -> bool {
        text.lines().rev().find(|line| !line.trim().is_empty()).is_some_and(|last| {
            // Strip ANSI so a trailing erase/cursor sequence after the arrow
            // (e.g. "──➤ \x1b[K") doesn't defeat the suffix check.
            let clean = crate::state::terminal::strip_ansi_codes(last);
            let clean = clean.trim_end();
            clean.contains(WCGW_PROMPT_PATTERN) && clean.ends_with(prompt_end)
        })
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
        match self.live.lock() {
            Ok(emu) => emu.snapshot(max_lines),
            Err(_) => Vec::new(),
        }
    }

    /// Whether the live terminal is currently on the alternate screen buffer
    /// (a full-screen app like vim/htop/less is running).
    pub fn live_in_alt_screen(&self) -> bool {
        self.live.lock().is_ok_and(|emu| emu.in_alt_screen())
    }

    /// Resize the terminal
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        debug!("PTY resizing to {}x{}", cols, rows);

        let new_size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };

        self.master.resize(new_size).context("Failed to resize PTY")?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prompt_detection_is_suffix_anchored() {
        // Per-shell nonce appended after the arrow.
        let end = "──➤deadbeefcafe0001";
        // real prompt on the last line -> complete
        assert!(PtyShell::check_prompt_complete("out\nmore\n◉ /home/x──➤deadbeefcafe0001 ", end));
        // prompt with trailing ANSI erase -> still complete
        assert!(PtyShell::check_prompt_complete("◉ /home/x──➤deadbeefcafe0001 \u{1b}[K", end));
        // the bug: ◉ and ──➤ appear MID-output, last line is normal -> NOT complete
        assert!(!PtyShell::check_prompt_complete(
            "menu: ◉ start ──➤deadbeefcafe0001 stop\nstill running",
            end
        ));
        // command echoed after the arrow (not the waiting prompt) -> not complete
        assert!(!PtyShell::check_prompt_complete("◉ /home/x──➤deadbeefcafe0001 ls -la", end));
        // no prompt at all
        assert!(!PtyShell::check_prompt_complete("just some output\n", end));
        // the nonce's payoff: output that forges the bare glyphs but NOT the
        // nonce must not be mistaken for the prompt.
        assert!(!PtyShell::check_prompt_complete("◉ /fake──➤ ", end));
        assert!(!PtyShell::check_prompt_complete("◉ /fake──➤wrongnonce ", end));
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
}
