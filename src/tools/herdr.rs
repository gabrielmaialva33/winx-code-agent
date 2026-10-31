//! Optional, explicitly bound access to an operator-configured Herdr server.
//!
//! No action discovers servers or implicitly selects the focused/current terminal.
//! The public server derives the trusted principal and prompt authority separately.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::Instant;

const CONFIG_LIMIT: usize = 256 * 1024;
const STDOUT_LIMIT: usize = 64 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;
const ARGV_LIMIT: usize = 32 * 1024;
const PROMPT_LIMIT: usize = 16 * 1024;
const TARGET_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HerdrAction {
    #[default]
    List,
    Get,
    Explain,
    Read,
    Prompt,
    Wait,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HerdrStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl HerdrStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HerdrRequest {
    pub action: HerdrAction,
    pub target: Option<String>,
    pub text: Option<String>,
    pub lines: Option<u32>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub until: Vec<HerdrStatus>,
}

#[derive(Debug, Clone)]
pub struct HerdrContext {
    pub principal_key: String,
    pub workspace_root: PathBuf,
    pub allow_prompt: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("Herdr is disabled; an operator must configure WINX_HERDR_CONFIG")]
    Disabled,
    #[error("invalid Herdr configuration: {0}")]
    Configuration(String),
    #[error("Herdr access denied: {0}")]
    Denied(String),
    #[error("Herdr unavailable: {0}")]
    Unavailable(String),
    #[error("invalid Herdr request: {0}")]
    InvalidRequest(String),
    #[error("Herdr command failed: {0}")]
    Failed(String),
    #[error("invalid Herdr response: {0}")]
    InvalidResponse(String),
    #[error("Herdr output limit exceeded; do not automatically retry prompts")]
    OutputLimit,
    #[error("Herdr prompt delivery is uncertain: {0}; do not automatically retry")]
    DeliveryUncertain(String),
    #[error("Herdr deadline expired; prompt delivery uncertain: {uncertain}; do not automatically retry")]
    Timeout { uncertain: bool },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HerdrConfig {
    executable: PathBuf,
    socket_path: PathBuf,
    #[serde(default = "default_deadline")]
    deadline_ms: u64,
    bindings: Vec<Binding>,
}

const fn default_deadline() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    principal_key: String,
    workspace_root: PathBuf,
    targets: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct HerdrAdapter {
    config: Option<HerdrConfig>,
}

impl HerdrAdapter {
    pub fn disabled() -> Self {
        Self { config: None }
    }

    /// A configuration hint, not a health probe or a grant of target authority.
    /// Do not open the configured path or start a process during discovery.
    pub(crate) fn configuration_hint() -> Value {
        let present = std::env::var_os("WINX_HERDR_CONFIG").is_some();
        json!({
            "configuration_present": present,
            "availability": if present { "unknown" } else { "disabled" },
            "reason": if present { "configuration_not_validated" } else { "configuration_missing" },
        })
    }

    pub fn from_env() -> Result<Self, HerdrError> {
        match std::env::var_os("WINX_HERDR_CONFIG") {
            None => Ok(Self::disabled()),
            Some(path) => Self::from_config_path(Path::new(&path)),
        }
    }

    pub fn from_config_path(path: &Path) -> Result<Self, HerdrError> {
        if !path.is_absolute() {
            return Err(configuration("configuration path must be absolute"));
        }
        let file =
            std::fs::File::open(path).map_err(|_| configuration("cannot open configuration"))?;
        let metadata =
            file.metadata().map_err(|_| configuration("cannot inspect configuration"))?;
        if !metadata.is_file() {
            return Err(configuration("configuration must be a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(configuration("configuration must be private (chmod 600)"));
            }
        }
        let mut bytes = Vec::new();
        file.take((CONFIG_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| configuration("cannot read configuration"))?;
        if bytes.len() > CONFIG_LIMIT {
            return Err(configuration("configuration exceeds 256 KiB"));
        }
        let mut config: HerdrConfig = serde_json::from_slice(&bytes)
            .map_err(|error| configuration(&format!("malformed JSON: {error}")))?;
        config.validate()?;
        Ok(Self { config: Some(config) })
    }

    pub async fn run(
        &self,
        context: &HerdrContext,
        request: HerdrRequest,
    ) -> Result<Value, HerdrError> {
        let config = self.config.as_ref().ok_or(HerdrError::Disabled)?;
        let binding = config.binding(context)?;
        validate_request(&request, context.allow_prompt)?;
        let deadline = Instant::now() + Duration::from_millis(config.deadline_ms);
        if request.action == HerdrAction::List {
            // One `agent list` call, filtered to the configured stable IDs in
            // configuration order. Unconfigured agents never leave this function.
            let live = config.list(context, deadline).await?;
            let mut agents = Vec::new();
            let mut seen = BTreeSet::new();
            let mut aggregate_bytes = 16;
            for id in binding.targets.values() {
                if !seen.insert(id) {
                    continue;
                }
                if let Some(agent) = live.iter().find(|agent| agent["terminal_id"] == **id) {
                    aggregate_bytes += serde_json::to_vec(agent)
                        .map_err(|_| invalid_response("cannot encode agent"))?
                        .len()
                        + 1;
                    if aggregate_bytes > STDOUT_LIMIT {
                        return Err(HerdrError::OutputLimit);
                    }
                    agents.push(agent.clone());
                }
            }
            let result = json!({ "agents": agents });
            if serde_json::to_vec(&result)
                .map_err(|_| invalid_response("cannot encode agents"))?
                .len()
                > STDOUT_LIMIT
            {
                return Err(HerdrError::OutputLimit);
            }
            return Ok(result);
        }
        let target = request
            .target
            .as_deref()
            .ok_or_else(|| invalid_request("an explicit configured target is required"))?;
        let id = binding
            .targets
            .get(target)
            .or_else(|| binding.targets.values().find(|id| id.as_str() == target))
            .ok_or_else(|| {
                HerdrError::Denied("target is not configured for this principal/workspace".into())
            })?;

        // Every targeted action first verifies the stable ID through `agent get`
        // on the pane currently hosting it; explain and read return no identity.
        let (agent, pane) = config.get(context, id, deadline).await?;
        if request.action == HerdrAction::Get {
            return Ok(json!({ "agent": agent }));
        }
        let action = request.action;
        let (args, expected_type) = command_args(&pane, request);
        let output =
            execute_cli(config, context, &args, deadline, action == HerdrAction::Prompt).await?;
        parse_output(action, output, id, expected_type)
    }
}

impl HerdrConfig {
    fn validate(&mut self) -> Result<(), HerdrError> {
        if !self.executable.is_absolute() || !self.socket_path.is_absolute() {
            return Err(configuration("executable and socket_path must be absolute"));
        }
        if self.socket_path.as_os_str().len() > 4096
            || self.socket_path.to_string_lossy().contains('\0')
        {
            return Err(configuration("socket_path is too long or contains NUL"));
        }
        self.executable = std::fs::canonicalize(&self.executable)
            .map_err(|_| HerdrError::Unavailable("configured executable does not exist".into()))?;
        let metadata = std::fs::metadata(&self.executable)
            .map_err(|_| HerdrError::Unavailable("cannot inspect configured executable".into()))?;
        if !metadata.is_file() {
            return Err(HerdrError::Unavailable(
                "configured executable is not a regular file".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(HerdrError::Unavailable("configured file is not executable".into()));
            }
        }
        if !(1..=30_000).contains(&self.deadline_ms) {
            return Err(configuration("deadline_ms must be between 1 and 30000"));
        }
        let mut scopes = BTreeSet::new();
        for binding in &self.bindings {
            if binding.principal_key.is_empty() || binding.principal_key.len() > 256 {
                return Err(configuration("principal_key must be nonempty and bounded"));
            }
            if !binding.workspace_root.is_absolute()
                || std::fs::canonicalize(&binding.workspace_root).ok().as_ref()
                    != Some(&binding.workspace_root)
            {
                return Err(configuration(
                    "workspace_root must be an existing canonical absolute path",
                ));
            }
            if !scopes.insert((&binding.principal_key, &binding.workspace_root)) {
                return Err(configuration("duplicate principal/workspace binding"));
            }
            if binding.targets.is_empty() || binding.targets.len() > TARGET_LIMIT {
                return Err(configuration("each binding must have 1 through 32 targets"));
            }
            for (alias, id) in &binding.targets {
                if !valid_target(alias) || !valid_target(id) {
                    return Err(configuration(
                        "targets require explicit stable IDs and safe aliases",
                    ));
                }
                if binding.targets.get(id).is_some_and(|mapped| mapped != id) {
                    return Err(configuration(
                        "an alias must not shadow another configured terminal ID",
                    ));
                }
            }
        }
        Ok(())
    }

    fn binding(&self, context: &HerdrContext) -> Result<&Binding, HerdrError> {
        if std::fs::canonicalize(&context.workspace_root).ok().as_ref()
            != Some(&context.workspace_root)
        {
            return Err(HerdrError::Denied("workspace must be canonical".into()));
        }
        self.bindings
            .iter()
            .find(|binding| {
                binding.principal_key == context.principal_key
                    && binding.workspace_root == context.workspace_root
            })
            .ok_or_else(|| HerdrError::Denied("principal/workspace binding is absent".into()))
    }

    /// Live agents as reported by `agent list`.
    async fn list(
        &self,
        context: &HerdrContext,
        deadline: Instant,
    ) -> Result<Vec<Value>, HerdrError> {
        let args = vec!["agent".into(), "list".into()];
        let output = execute_cli(self, context, &args, deadline, false).await?;
        parse_agent_list(&output)
    }

    /// Resolve and verify a configured stable terminal ID.
    ///
    /// Herdr agent commands address a pane ID or a live agent name, never a
    /// terminal ID, and pane IDs change when a pane moves. The terminal ID is the
    /// stable handle, so look up the pane hosting it now, then confirm through
    /// `agent get` on that pane that the same terminal answers. Returns the agent
    /// and the pane to address follow-up commands to.
    async fn get(
        &self,
        context: &HerdrContext,
        id: &str,
        deadline: Instant,
    ) -> Result<(Value, String), HerdrError> {
        let pane = self
            .list(context, deadline)
            .await?
            .iter()
            .find(|agent| agent["terminal_id"] == id)
            .and_then(|agent| agent["pane_id"].as_str())
            .filter(|pane| valid_target(pane))
            .map(str::to_string)
            .ok_or_else(|| {
                HerdrError::Unavailable("configured terminal hosts no live agent".into())
            })?;
        let args = vec!["agent".into(), "get".into(), pane.clone()];
        let output = execute_cli(self, context, &args, deadline, false).await?;
        Ok((parse_agent(&output, id, "agent_info")?, pane))
    }
}

async fn execute_cli(
    config: &HerdrConfig,
    context: &HerdrContext,
    args: &[String],
    deadline: Instant,
    prompt: bool,
) -> Result<Vec<u8>, HerdrError> {
    if config.executable.as_os_str().len() + 1 + args.iter().map(|arg| arg.len() + 1).sum::<usize>()
        > ARGV_LIMIT
        || args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(invalid_request("argv exceeds its byte bound or contains NUL"));
    }
    if Instant::now() >= deadline {
        return Err(HerdrError::Timeout { uncertain: false });
    }
    let mut command = Command::new(&config.executable);
    command
        .args(args)
        .current_dir(&context.workspace_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Herdr routing may otherwise inherit session/current-pane/plugin state.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("HERDR_") {
            command.env_remove(key);
        }
    }
    command.env("HERDR_SOCKET_PATH", &config.socket_path);
    let mut child = command
        .spawn()
        .map_err(|_| HerdrError::Unavailable("cannot spawn configured executable".into()))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(after_spawn_error(
            HerdrError::Failed("output pipe unavailable".into()),
            prompt,
        ));
    };
    let result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| HerdrError::Failed("cannot wait for subprocess".into()))
            },
            read_bounded(stdout, STDOUT_LIMIT),
            read_bounded(stderr, STDERR_LIMIT)
        )
    })
    .await;
    match result {
        Ok(Ok((status, stdout, _stderr))) if status.success() => Ok(stdout),
        Ok(Ok((status, _, _))) => {
            if prompt {
                Err(HerdrError::DeliveryUncertain(format!("CLI exited with {status}")))
            } else {
                Err(HerdrError::Failed(format!("CLI exited with {status}")))
            }
        }
        failed => {
            // Cancellation or a full output pipe must not leave the CLI alive.
            let _ = child.kill().await;
            let _ = child.wait().await;
            match failed {
                Ok(Err(error)) => Err(after_spawn_error(error, prompt)),
                Err(_) => Err(HerdrError::Timeout { uncertain: prompt }),
                Ok(Ok(_)) => unreachable!("successful command handled above"),
            }
        }
    }
}

async fn read_bounded(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, HerdrError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let length = pipe
            .read(&mut buffer)
            .await
            .map_err(|_| HerdrError::Failed("cannot read subprocess output".into()))?;
        if length == 0 {
            return Ok(bytes);
        }
        if bytes.len() + length > limit {
            return Err(HerdrError::OutputLimit);
        }
        bytes.extend_from_slice(&buffer[..length]);
    }
}

fn parse_agent_list(output: &[u8]) -> Result<Vec<Value>, HerdrError> {
    let response: Value = serde_json::from_slice(output)
        .map_err(|_| invalid_response("stdout is not a JSON success envelope"))?;
    if !response["id"].is_string()
        || response.get("error").is_some()
        || response["result"]["type"] != "agent_list"
    {
        return Err(invalid_response("unexpected response envelope/type"));
    }
    response["result"]["agents"]
        .as_array()
        .cloned()
        .ok_or_else(|| invalid_response("agent list is not an array"))
}

fn parse_agent(output: &[u8], id: &str, expected_type: &str) -> Result<Value, HerdrError> {
    let response: Value = serde_json::from_slice(output)
        .map_err(|_| invalid_response("stdout is not a JSON success envelope"))?;
    if !response["id"].is_string()
        || response.get("error").is_some()
        || response["result"]["type"] != expected_type
    {
        return Err(invalid_response("unexpected response envelope/type"));
    }
    let agent = &response["result"]["agent"];
    if agent["terminal_id"] != id {
        return Err(invalid_response(
            "returned terminal identity does not match the configured stable ID",
        ));
    }
    Ok(agent.clone())
}

fn parse_output(
    action: HerdrAction,
    output: Vec<u8>,
    id: &str,
    expected_type: &str,
) -> Result<Value, HerdrError> {
    match action {
        HerdrAction::Read => {
            let text = String::from_utf8(output)
                .map_err(|_| invalid_response("read output is not UTF-8"))?;
            Ok(json!({ "terminal_id": id, "source": "recent", "text": text }))
        }
        HerdrAction::Explain => {
            let explain: Value = serde_json::from_slice(&output)
                .map_err(|_| invalid_response("explain output is not JSON"))?;
            if !explain.is_object()
                || !explain["state"].is_string()
                || explain.get("error").is_some()
            {
                return Err(invalid_response("explain output is not an explanation object"));
            }
            Ok(json!({ "terminal_id": id, "explain": explain }))
        }
        HerdrAction::Prompt | HerdrAction::Wait => {
            let agent = parse_agent(&output, id, expected_type)
                .map_err(|error| after_spawn_error(error, action == HerdrAction::Prompt))?;
            Ok(json!({ "agent": agent }))
        }
        HerdrAction::List | HerdrAction::Get => Err(invalid_request("unexpected response action")),
    }
}

/// Argv for a targeted action, addressed to the verified hosting `pane`.
fn command_args(pane: &str, request: HerdrRequest) -> (Vec<String>, &'static str) {
    let mut args = vec!["agent".into()];
    let expected_type = match request.action {
        HerdrAction::Explain => {
            args.extend(["explain".into(), pane.into(), "--json".into()]);
            ""
        }
        HerdrAction::Read => {
            args.extend([
                "read".into(),
                pane.into(),
                "--source".into(),
                "recent".into(),
                "--lines".into(),
                request.lines.unwrap_or(100).to_string(),
                "--format".into(),
                "text".into(),
            ]);
            ""
        }
        HerdrAction::Prompt => {
            // Stop session routing before both positionals. A separator in TEXT's
            // original argv slot is escaped by Herdr and becomes literal text.
            let mut text = request.text.unwrap_or_default();
            // Herdr strips one leading verbatim escape even after `--`.
            if text.starts_with('\u{1}') {
                text.insert(0, '\u{1}');
            }
            args.extend(["prompt".into(), "--".into(), pane.into(), text]);
            "agent_prompted"
        }
        HerdrAction::Wait => {
            args.extend([
                "wait".into(),
                pane.into(),
                "--timeout".into(),
                request.timeout_ms.unwrap_or(1000).to_string(),
            ]);
            for status in &request.until {
                args.extend(["--until".into(), status.as_str().into()]);
            }
            "agent_info"
        }
        HerdrAction::List | HerdrAction::Get => "",
    };
    (args, expected_type)
}

fn validate_request(request: &HerdrRequest, allow_prompt: bool) -> Result<(), HerdrError> {
    if request.text.is_some() && request.action != HerdrAction::Prompt {
        return Err(invalid_request("text is only supported for prompt"));
    }
    if request.lines.is_some() && request.action != HerdrAction::Read {
        return Err(invalid_request("lines is only supported for read"));
    }
    if (request.timeout_ms.is_some() || !request.until.is_empty())
        && request.action != HerdrAction::Wait
    {
        return Err(invalid_request(
            "timeout_ms and until are only supported for wait; prompt is immediate submission",
        ));
    }
    if request.action == HerdrAction::List && request.target.is_some() {
        return Err(invalid_request("list does not accept a target"));
    }
    if request.action == HerdrAction::Prompt {
        if !allow_prompt {
            return Err(HerdrError::Denied(
                "prompt requires wcgw mode and BashCommand authority".into(),
            ));
        }
        let text =
            request.text.as_deref().ok_or_else(|| invalid_request("prompt text is required"))?;
        if text.is_empty() || text.len() > PROMPT_LIMIT || text.contains('\0') {
            return Err(invalid_request(
                "prompt must be nonempty, at most 16 KiB and contain no NUL",
            ));
        }
    }
    if request.lines.is_some_and(|lines| !(1..=1000).contains(&lines)) {
        return Err(invalid_request("lines must be between 1 and 1000"));
    }
    if request.timeout_ms.is_some_and(|timeout| !(1..=30_000).contains(&timeout))
        || request.until.len() > 5
    {
        return Err(invalid_request(
            "timeout_ms must be between 1 and 30000 and until has at most five states",
        ));
    }
    Ok(())
}

fn valid_target(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && !matches!(value, "current" | "focused" | "focus")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}
fn configuration(message: &str) -> HerdrError {
    HerdrError::Configuration(message.into())
}
fn invalid_request(message: &str) -> HerdrError {
    HerdrError::InvalidRequest(message.into())
}
fn invalid_response(message: &str) -> HerdrError {
    HerdrError::InvalidResponse(message.into())
}
fn after_spawn_error(error: HerdrError, prompt: bool) -> HerdrError {
    if prompt {
        HerdrError::DeliveryUncertain(error.to_string())
    } else {
        error
    }
}
