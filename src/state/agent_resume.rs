//! Durable interactive-agent session records, so a coding-agent TUI piloted
//! through winx (claude, codex, ...) is not silently lost when the process
//! that owned its PTY dies (host reboot, guardian kill, embedded-adapter
//! restart).
//!
//! Modeled on herdr's `agent_resume` (<https://github.com/herdrdev/herdr>,
//! Apache-2.0): persist *what was launched where*, and rebuild a resume
//! command after the loss. winx keeps it deliberately simpler than herdr — it
//! records the launch command it executed itself instead of inspecting
//! process trees, and resumes by conversation-continuation flags (`claude
//! --continue`, `codex resume --last`) instead of persisted session ids.
//!
//! Lifecycle: every foreground `Command` action either overwrites the record
//! (the command launches a known agent) or clears it (it doesn't) — the main
//! PTY runs one foreground program at a time, so the latest record is the
//! only one that can still matter. Records survive until superseded; a stale
//! record after a clean agent exit only makes the recovery hint say the agent
//! "was running", which continuation-style resume commands tolerate.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::persistence::get_state_dir;

/// What was running in the foreground PTY when the record was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    /// Canonical agent id — matches the turn-manifest ids (`claude`, `codex`,
    /// `gemini`, ...), so the same string works as a `wait_for_turn`
    /// recognizer hint after resuming.
    pub agent: String,
    /// The exact command line that launched it.
    pub command: String,
    /// Working directory at launch (resume must happen in the same project).
    pub cwd: String,
    pub launched_at_unix_ms: u64,
}

impl AgentSessionRecord {
    /// The command that resumes (or best-effort relaunches) this agent's
    /// conversation. Agents with a documented continuation flag get it;
    /// everything else is relaunched with its original command.
    pub fn resume_command(&self) -> String {
        match self.agent.as_str() {
            "claude" => "claude --continue".to_string(),
            "codex" => "codex resume --last".to_string(),
            _ => self.command.clone(),
        }
    }
}

/// Canonical agent id when `command` launches a known interactive coding
/// agent in the foreground, `None` otherwise.
///
/// Heuristic on the command string winx itself executes (there is no process
/// tree to inspect at recovery time): take the last `&&`/`;` segment (so
/// `cd x && claude` counts), refuse pipelines (a piped agent is not an
/// interactive TUI), skip leading `VAR=value` assignments, and match the
/// basename of the first real token against the known launcher set.
pub fn detect_agent_launch(command: &str) -> Option<&'static str> {
    let segment = command.split("&&").last()?.split(';').next_back()?.trim();
    if segment.contains('|') {
        return None;
    }
    let token = segment.split_whitespace().find(|token| !is_env_assignment(token))?;
    agent_for_basename(basename(token))
}

fn is_env_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && !name.starts_with(|ch: char| ch.is_ascii_digit())
    })
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Launcher basenames for the interactive agents winx knows how to recognize
/// (the same set the turn manifests cover), normalized to manifest ids.
fn agent_for_basename(name: &str) -> Option<&'static str> {
    match name {
        "claude" | "claude-code" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "agy" | "antigravity" => Some("agy"),
        "cursor-agent" | "cursor" => Some("cursor"),
        "opencode" | "opencode2" => Some("opencode"),
        "copilot" | "ghcs" => Some("copilot"),
        "kimi" => Some("kimi"),
        "kiro" | "kiro-cli" => Some("kiro"),
        "droid" => Some("droid"),
        "amp" => Some("amp"),
        "grok" => Some("grok"),
        "hermes" => Some("hermes"),
        "kilo" => Some("kilo"),
        "qodercli" => Some("qodercli"),
        "qwen" => Some("qwen"),
        "pi" => Some("pi"),
        "devin" | "devin-cli" => Some("devin"),
        "cline" => Some("cline"),
        "maki" => Some("maki"),
        "muse" | "muse-code" | "muse-cli" => Some("muse"),
        _ => None,
    }
}

fn record_path(thread_id: &str) -> Result<PathBuf> {
    Ok(get_state_dir()?.join(format!("{thread_id}_agent_session.json")))
}

pub fn save_agent_session(thread_id: &str, record: &AgentSessionRecord) -> Result<()> {
    save_agent_session_to_path(&record_path(thread_id)?, record)
}

pub fn save_agent_session_to_path(path: &Path, record: &AgentSessionRecord) -> Result<()> {
    let json = serde_json::to_string(record).context("failed to serialize agent session")?;
    std::fs::write(path, json)
        .with_context(|| format!("failed to write agent session to {}", path.display()))
}

pub fn load_agent_session(thread_id: &str) -> Result<Option<AgentSessionRecord>> {
    load_agent_session_from_path(&record_path(thread_id)?)
}

pub fn load_agent_session_from_path(path: &Path) -> Result<Option<AgentSessionRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read agent session from {}", path.display()))?;
    Ok(serde_json::from_str(&json).ok())
}

pub fn clear_agent_session(thread_id: &str) -> Result<()> {
    clear_agent_session_at_path(&record_path(thread_id)?)
}

pub fn clear_agent_session_at_path(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to clear agent session {}", path.display()))
        }
    }
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn detects_direct_and_wrapped_agent_launches() {
        assert_eq!(detect_agent_launch("claude"), Some("claude"));
        assert_eq!(detect_agent_launch("claude --dangerously-skip-permissions"), Some("claude"));
        assert_eq!(detect_agent_launch("cd ~/repo && claude"), Some("claude"));
        assert_eq!(detect_agent_launch("FOO=bar codex --model gpt-5"), Some("codex"));
        assert_eq!(detect_agent_launch("/usr/local/bin/gemini"), Some("gemini"));
        assert_eq!(detect_agent_launch("antigravity"), Some("agy"));
        assert_eq!(detect_agent_launch("cursor-agent"), Some("cursor"));
    }

    #[test]
    fn refuses_non_agents_pipelines_and_backgrounded_noise() {
        assert_eq!(detect_agent_launch("ls -la"), None);
        assert_eq!(detect_agent_launch("claude | tee log"), None);
        assert_eq!(detect_agent_launch("echo claude"), None);
        assert_eq!(detect_agent_launch("claudette"), None);
        assert_eq!(detect_agent_launch("FOO=claude env"), None);
        assert_eq!(detect_agent_launch(""), None);
    }

    #[test]
    fn compound_commands_use_the_last_segment() {
        // The last segment is what owns the PTY foreground.
        assert_eq!(detect_agent_launch("claude; ls"), None);
        assert_eq!(detect_agent_launch("git pull && codex"), Some("codex"));
    }

    #[test]
    fn resume_commands_prefer_continuation_flags() {
        let record = |agent: &str, command: &str| AgentSessionRecord {
            agent: agent.to_string(),
            command: command.to_string(),
            cwd: "/repo".to_string(),
            launched_at_unix_ms: 0,
        };
        assert_eq!(record("claude", "claude --verbose").resume_command(), "claude --continue");
        assert_eq!(record("codex", "codex").resume_command(), "codex resume --last");
        assert_eq!(record("grok", "grok --fast").resume_command(), "grok --fast");
    }

    #[test]
    fn record_roundtrip_and_clear() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("t1_agent_session.json");
        let record = AgentSessionRecord {
            agent: "claude".to_string(),
            command: "claude".to_string(),
            cwd: "/repo".to_string(),
            launched_at_unix_ms: 42,
        };
        save_agent_session_to_path(&path, &record).expect("save");
        assert_eq!(load_agent_session_from_path(&path).expect("load"), Some(record));
        clear_agent_session_at_path(&path).expect("clear");
        assert_eq!(load_agent_session_from_path(&path).expect("load"), None);
        // Clearing an absent record is idempotent.
        clear_agent_session_at_path(&path).expect("clear absent");
    }

    #[test]
    fn corrupt_record_reads_as_absent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("t2_agent_session.json");
        std::fs::write(&path, "not json").expect("write");
        assert_eq!(load_agent_session_from_path(&path).expect("load"), None);
    }
}
