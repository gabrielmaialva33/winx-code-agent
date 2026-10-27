//! Durable launch evidence for an interactive agent whose PTY was lost.
//!
//! Explicit selectors identify the target requested at launch, not a backend-confirmed
//! conversation or the conversation currently selected after an in-TUI switch.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::persistence::get_state_dir;

const RECORD_BYTES_CAP: usize = 16 * 1024;
const COMMAND_BYTES_CAP: usize = 8 * 1024;
const PATH_BYTES_CAP: usize = 4096;

/// What Winx requested in its last accepted foreground agent launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub agent: String,
    pub command: String,
    pub cwd: String,
    pub launched_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_token: Option<crate::runtime::ShellExecutionToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_selector: Option<ConversationSelector>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSelector {
    pub id: String,
    pub kind: SelectorKind,
    pub source: SelectorSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorKind {
    NewSession,
    ResumeTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorSource {
    LaunchArgument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryClassification {
    ExplicitTarget,
    BestEffort,
    Unavailable,
}

impl RecoveryClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitTarget => "explicit_target",
            Self::BestEffort => "best_effort",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryAdvice {
    pub classification: RecoveryClassification,
    pub source: Option<SelectorSource>,
    pub current_conversation_verified: bool,
    pub resume_argv: Option<Vec<String>>,
}

impl AgentSessionRecord {
    pub fn from_launch(
        agent: &str,
        command: &str,
        cwd: &Path,
        workspace_root: &Path,
        scoped_thread_id: &str,
        execution_token: Option<crate::runtime::ShellExecutionToken>,
    ) -> Self {
        Self {
            agent: agent.to_string(),
            command: command.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
            launched_at_unix_ms: now_unix_ms(),
            workspace_root: Some(workspace_root.to_string_lossy().into_owned()),
            scoped_thread_id: Some(scoped_thread_id.to_string()),
            execution_token,
            conversation_selector: selector_from_command(command)
                .filter(|(backend, _)| *backend == agent)
                .map(|(_, selector)| selector),
        }
    }

    /// A literal recovery target only; an empty string means no executable advice.
    pub fn resume_command(&self) -> String {
        self.recovery_advice().resume_argv.map_or_else(String::new, |argv| argv.join(" "))
    }

    pub fn recovery_advice(&self) -> RecoveryAdvice {
        let unavailable = || RecoveryAdvice {
            classification: RecoveryClassification::Unavailable,
            source: None,
            current_conversation_verified: false,
            resume_argv: None,
        };
        if validate_record(self).is_err() {
            return unavailable();
        }
        let Some(selector) = self.conversation_selector.as_ref() else {
            return RecoveryAdvice {
                classification: if matches!(self.agent.as_str(), "claude" | "codex")
                    && simple_launch_text(&self.command)
                    && self.command.trim() == self.agent
                {
                    RecoveryClassification::BestEffort
                } else {
                    RecoveryClassification::Unavailable
                },
                ..unavailable()
            };
        };
        if self.workspace_root.as_deref().is_none_or(str::is_empty)
            || self.scoped_thread_id.as_deref().is_none_or(str::is_empty)
            || selector_from_command(&self.command)
                .is_none_or(|(agent, parsed)| agent != self.agent || &parsed != selector)
        {
            return unavailable();
        }
        let resume_argv = match self.agent.as_str() {
            "claude" => vec!["claude".into(), "--resume".into(), selector.id.clone()],
            "codex" => vec!["codex".into(), "resume".into(), selector.id.clone()],
            _ => return unavailable(),
        };
        RecoveryAdvice {
            classification: RecoveryClassification::ExplicitTarget,
            source: Some(SelectorSource::LaunchArgument),
            current_conversation_verified: false,
            resume_argv: Some(resume_argv),
        }
    }

    pub fn recovery_advice_for(&self, thread_id: &str, workspace_root: &Path) -> RecoveryAdvice {
        let mut advice = self.recovery_advice();
        if self.scoped_thread_id.as_deref().is_some_and(|recorded| recorded != thread_id)
            || self
                .workspace_root
                .as_deref()
                .is_some_and(|recorded| Path::new(recorded) != workspace_root)
        {
            advice.classification = RecoveryClassification::Unavailable;
            advice.source = None;
            advice.resume_argv = None;
        }
        advice
    }
}

fn simple_launch_text(command: &str) -> bool {
    !command.is_empty()
        && command.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b' ' | b'_' | b'-' | b'/' | b'.' | b'=' | b':')
        })
}

fn selector_from_command(command: &str) -> Option<(&'static str, ConversationSelector)> {
    if command.len() > COMMAND_BYTES_CAP || !simple_launch_text(command) {
        return None;
    }
    let mut tokens = command.split_ascii_whitespace();
    let (agent, option, id) = (tokens.next()?, tokens.next()?, tokens.next()?);
    if tokens.next().is_some() || !valid_uuid(id) {
        return None;
    }
    let (agent, kind) = match (agent, option) {
        ("claude", "--session-id") => ("claude", SelectorKind::NewSession),
        ("claude", "--resume" | "-r") => ("claude", SelectorKind::ResumeTarget),
        ("codex", "resume") => ("codex", SelectorKind::ResumeTarget),
        _ => return None,
    };
    Some((
        agent,
        ConversationSelector { id: id.to_string(), kind, source: SelectorSource::LaunchArgument },
    ))
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn validate_record(record: &AgentSessionRecord) -> Result<()> {
    if record.agent.is_empty()
        || record.agent.len() > 64
        || !record
            .agent
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || record.command.len() > COMMAND_BYTES_CAP
        || record.cwd.is_empty()
        || record.cwd.len() > PATH_BYTES_CAP
        || record.cwd.chars().any(char::is_control)
        || record.workspace_root.as_ref().is_some_and(|root| {
            root.is_empty() || root.len() > PATH_BYTES_CAP || root.chars().any(char::is_control)
        })
        || record.scoped_thread_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > PATH_BYTES_CAP
                || crate::types::normalize_thread_id(id) != *id
        })
        || record.conversation_selector.as_ref().is_some_and(|selector| !valid_uuid(&selector.id))
        || record.execution_token.as_ref().is_some_and(|token| {
            token.guardian_epoch.len() > 256 || token.session_epoch.len() > 256
        })
    {
        bail!("invalid or oversized agent-session record");
    }
    Ok(())
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
    if thread_id.is_empty()
        || thread_id.len() > PATH_BYTES_CAP
        || crate::types::normalize_thread_id(thread_id) != thread_id
    {
        bail!("invalid scoped agent-session thread");
    }
    Ok(get_state_dir()?.join(format!("{thread_id}_agent_session.json")))
}

pub fn save_agent_session(thread_id: &str, record: &AgentSessionRecord) -> Result<()> {
    save_agent_session_to_path(&record_path(thread_id)?, record)
}

pub fn save_agent_session_to_path(path: &Path, record: &AgentSessionRecord) -> Result<()> {
    validate_record(record)?;
    let json = serde_json::to_vec(record).context("failed to serialize agent session")?;
    if json.len() > RECORD_BYTES_CAP {
        bail!("agent-session record exceeds its byte limit");
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!("agent-session path must be a regular file");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".winx-agent-")
        .tempfile_in(parent)
        .context("failed to create private agent-session file")?;
    temporary.write_all(&json)?;
    temporary.flush()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn load_agent_session(thread_id: &str) -> Result<Option<AgentSessionRecord>> {
    load_agent_session_from_path(&record_path(thread_id)?)
}

pub fn load_agent_session_from_path(path: &Path) -> Result<Option<AgentSessionRecord>> {
    let file = match crate::os::open_read_nofollow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to open agent-session record"),
    };
    if file.metadata()?.len() > RECORD_BYTES_CAP as u64 {
        bail!("agent-session record exceeds its byte limit");
    }
    let mut bytes = Vec::new();
    file.take(RECORD_BYTES_CAP as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > RECORD_BYTES_CAP {
        bail!("agent-session record exceeds its byte limit");
    }
    let record: Option<AgentSessionRecord> = serde_json::from_slice(&bytes).ok();
    Ok(record.filter(|record| validate_record(record).is_ok()))
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
    fn unbound_records_have_no_executable_recovery_advice() {
        let record = |agent: &str, command: &str| AgentSessionRecord {
            agent: agent.to_string(),
            command: command.to_string(),
            cwd: "/repo".to_string(),
            launched_at_unix_ms: 0,
            workspace_root: None,
            scoped_thread_id: None,
            execution_token: None,
            conversation_selector: None,
        };
        assert_eq!(record("claude", "claude --verbose").resume_command(), "");
        assert_eq!(record("codex", "codex").resume_command(), "");
        assert_eq!(record("grok", "grok --fast").resume_command(), "");
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
            workspace_root: None,
            scoped_thread_id: None,
            execution_token: None,
            conversation_selector: None,
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

    fn selector_record(agent: &str, command: &str, id: &str, kind: &str) -> AgentSessionRecord {
        serde_json::from_value(serde_json::json!({
            "agent": agent, "command": command, "cwd": "/repo", "launched_at_unix_ms": 42,
            "workspace_root": "/repo", "scoped_thread_id": "selector_scope",
            "conversation_selector": {"id": id, "kind": kind, "source": "launch_argument"}
        }))
        .expect("selector record")
    }

    #[test]
    fn selector_recovery_targets_the_recorded_uuid_and_never_latest() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        for (agent, command, kind, expected) in [
            (
                "claude",
                format!("claude --session-id {id}"),
                "new_session",
                format!("claude --resume {id}"),
            ),
            (
                "claude",
                format!("claude --resume {id}"),
                "resume_target",
                format!("claude --resume {id}"),
            ),
            ("claude", format!("claude -r {id}"), "resume_target", format!("claude --resume {id}")),
            ("codex", format!("codex resume {id}"), "resume_target", format!("codex resume {id}")),
        ] {
            let record = selector_record(agent, &command, id, kind);
            assert_eq!(record.resume_command(), expected, "{command}");
            let encoded = serde_json::to_value(&record).expect("serialize record");
            assert_eq!(encoded["conversation_selector"]["source"], "launch_argument");
            assert_eq!(encoded["conversation_selector"]["id"], id);
        }
    }

    #[test]
    fn selector_recovery_legacy_and_unsupported_records_never_replay_commands() {
        for (agent, command) in [
            ("claude", "claude"),
            ("codex", "codex"),
            ("grok", "grok 'repeat the expensive initial task'"),
        ] {
            let record: AgentSessionRecord = serde_json::from_value(serde_json::json!({
                "agent":agent,"command":command,"cwd":"/repo","launched_at_unix_ms":1
            }))
            .expect("legacy record");
            assert!(
                record.resume_command().is_empty(),
                "unsafe recovery: {}",
                record.resume_command()
            );
        }
    }

    #[test]
    fn selector_recovery_ambiguous_or_tampered_records_have_no_executable_advice() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let other = "550e8400-e29b-41d4-a716-446655440001";
        for command in [
            format!("claude --session-id {id}; echo unrelated"),
            format!("claude --session-id {id} && echo unrelated"),
            format!("claude --session-id {id} | cat"),
            format!("claude --session-id {id} > output"),
            format!("claude --session-id {id} &"),
            format!("claude --resume {id} --fork-session"),
            format!("claude --session-id {id} --resume {id}"),
            format!("claude --session-id {id} --session-id {other}"),
            format!("HOME=/other claude --session-id {id}"),
            format!("cd /other && claude --session-id {id}"),
            format!("echo $(claude --session-id {id})"),
            format!("claude --session-id {other}"),
            "claude --continue".to_string(),
        ] {
            let record = selector_record("claude", &command, id, "new_session");
            assert!(record.resume_command().is_empty(), "ambiguous command: {command}");
        }
        for invalid_id in
            ["--last", "name", "/tmp/session", "550e8400-e29b-41d4-a716-44665544000z", ""]
        {
            let record = selector_record(
                "claude",
                &format!("claude --session-id {invalid_id}"),
                invalid_id,
                "new_session",
            );
            assert!(record.resume_command().is_empty(), "invalid selector: {invalid_id}");
        }
    }

    #[test]
    fn selector_recovery_survives_restart_without_choosing_a_newer_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = "550e8400-e29b-41d4-a716-446655440000";
        let newer = "550e8400-e29b-41d4-a716-446655440001";
        for (name, id) in [("a", first), ("b", newer)] {
            save_agent_session_to_path(
                &dir.path().join(name),
                &selector_record("claude", &format!("claude --session-id {id}"), id, "new_session"),
            )
            .expect("persist selector");
        }
        let record =
            load_agent_session_from_path(&dir.path().join("a")).expect("reload").expect("record");
        assert_eq!(record.resume_command(), format!("claude --resume {first}"));
    }

    #[test]
    fn selector_recovery_oversized_state_does_not_replace_a_valid_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("record");
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let valid =
            selector_record("claude", &format!("claude --session-id {id}"), id, "new_session");
        save_agent_session_to_path(&path, &valid).expect("initial save");
        let prior = std::fs::read(&path).expect("prior bytes");
        let mut oversized = valid;
        oversized.command = "x".repeat(17 * 1024);
        assert!(
            save_agent_session_to_path(&path, &oversized).is_err(),
            "oversized record accepted"
        );
        assert_eq!(std::fs::read(&path).expect("retained bytes"), prior);
        std::fs::write(&path, vec![b' '; 17 * 1024]).expect("oversized input");
        assert!(load_agent_session_from_path(&path).ok().flatten().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn selector_recovery_symlink_state_is_never_followed_for_read_or_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target");
        let link = dir.path().join("record");
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let record =
            selector_record("claude", &format!("claude --session-id {id}"), id, "new_session");
        save_agent_session_to_path(&target, &record).expect("target");
        let prior = std::fs::read(&target).expect("prior target");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(load_agent_session_from_path(&link).ok().flatten().is_none(), "followed symlink");
        assert!(save_agent_session_to_path(&link, &record).is_err(), "wrote through symlink");
        assert_eq!(std::fs::read(&target).expect("target unchanged"), prior);
    }
    #[test]
    fn selector_recovery_factory_marks_unsupported_and_ambiguous_launches_unavailable() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        for (agent, command) in [
            ("claude", format!("claude --session-id {id} --resume {id}")),
            ("claude", format!("claude --resume {id} --fork-session")),
            ("claude", format!("claude --session-id {id} --verbose")),
            ("claude", "claude --session-id not-a-uuid".to_string()),
            ("claude", "claude --resume".to_string()),
            ("claude", "claude --continue".to_string()),
            ("claude", "claude --model sonnet".to_string()),
            ("claude", format!("HOME=/other claude --session-id {id}")),
            ("claude", format!("cd /other && claude --session-id {id}")),
            ("claude", format!("claude --session-id {id} | cat")),
            ("codex", "codex resume --last".to_string()),
            ("codex", format!("codex resume {id} --all")),
        ] {
            let record = AgentSessionRecord::from_launch(
                agent,
                &command,
                Path::new("/repo"),
                Path::new("/repo"),
                "selector_scope",
                None,
            );
            let advice = record.recovery_advice();
            assert_eq!(advice.classification, RecoveryClassification::Unavailable, "{command}");
            assert!(advice.resume_argv.is_none(), "{command}");
            assert!(!advice.current_conversation_verified);
        }
        for agent in ["claude", "codex"] {
            let record = AgentSessionRecord::from_launch(
                agent,
                agent,
                Path::new("/repo"),
                Path::new("/repo"),
                "selector_scope",
                None,
            );
            assert_eq!(record.recovery_advice().classification, RecoveryClassification::BestEffort);
        }
    }
}
