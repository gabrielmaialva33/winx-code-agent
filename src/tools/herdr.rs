//! Optional, explicitly bound access to an operator-configured Herdr server.
//!
//! No action discovers servers or implicitly selects the focused/current terminal.
//! The public server derives the trusted principal and prompt authority separately.
//!
//! The adapter speaks Herdr's socket API: newline-delimited JSON over the Unix
//! socket named by trusted configuration. Herdr serves exactly one request per
//! connection, so each request opens its own connection, all bounded by one
//! operation deadline.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::Instant;

const CONFIG_LIMIT: usize = 256 * 1024;
const RESPONSE_LIMIT: usize = 64 * 1024;
/// A maximal prompt with every byte JSON-escaped still fits.
const REQUEST_LIMIT: usize = 128 * 1024;
const PROMPT_LIMIT: usize = 16 * 1024;
const TARGET_LIMIT: usize = 32;
/// Herdr socket protocol versions whose agent methods this adapter speaks.
const SUPPORTED_PROTOCOLS: &[u64] = &[22];
/// `agent.prompt` errors Herdr returns only before any input reaches the pane.
/// Every other prompt error, including `timeout`, leaves delivery uncertain.
const PRE_DELIVERY_PROMPT_ERRORS: &[&str] = &[
    "agent_not_found",
    "agent_target_ambiguous",
    "agent_blocked",
    "agent_not_ready",
    "empty_agent_prompt",
    "invalid_request",
    "unknown_method",
];

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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HerdrRequest {
    pub action: HerdrAction,
    pub target: Option<String>,
    pub text: Option<String>,
    pub lines: Option<u32>,
    /// Wait limit; on prompt, it also requests Herdr's atomic submit-and-wait.
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
    #[error("Herdr request failed: {0}")]
    Failed(String),
    #[error("Herdr agent is blocked and requires interactive input; no input was sent")]
    AgentBlocked,
    #[error("Herdr rejected the request ({code}); no input was sent")]
    Rejected { code: String },
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
    /// Accepted from configurations written for the former CLI transport and
    /// still validated when present; the socket transport never runs it.
    #[serde(default)]
    executable: Option<PathBuf>,
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
    /// Do not open the configured path or connect to the socket during discovery.
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
        // Resolve the alias before connecting: a denied target never reaches Herdr.
        let id = match request.action {
            HerdrAction::List => None,
            _ => Some(binding.resolve(request.target.as_deref())?),
        };
        let deadline = Instant::now() + Duration::from_millis(config.deadline_ms);
        let mut client = Client::connect(&config.socket_path, deadline).await?;
        let Some(id) = id else {
            return list_configured(binding, &client.list().await?);
        };

        // Every targeted action first verifies the stable ID through `agent.get`
        // on the pane currently hosting it; read and explain return no identity.
        let (agent, pane) = client.resolve(id).await?;
        match request.action {
            HerdrAction::Get => Ok(json!({ "agent": agent })),
            HerdrAction::Read => client.read(id, &pane, request.lines.unwrap_or(100)).await,
            HerdrAction::Explain => client.explain(id, &pane).await,
            HerdrAction::Wait => client.wait(id, &pane, &request).await,
            HerdrAction::Prompt => client.prompt(id, &pane, request).await,
            HerdrAction::List => Err(invalid_request("unexpected list action")),
        }
    }
}

/// One `agent.list` result, filtered to the configured stable IDs in
/// configuration order. Unconfigured agents never leave this function.
fn list_configured(binding: &Binding, live: &[Value]) -> Result<Value, HerdrError> {
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
            if aggregate_bytes > RESPONSE_LIMIT {
                return Err(HerdrError::OutputLimit);
            }
            agents.push(agent.clone());
        }
    }
    let result = json!({ "agents": agents });
    if serde_json::to_vec(&result).map_err(|_| invalid_response("cannot encode agents"))?.len()
        > RESPONSE_LIMIT
    {
        return Err(HerdrError::OutputLimit);
    }
    Ok(result)
}

impl HerdrConfig {
    fn validate(&mut self) -> Result<(), HerdrError> {
        if !self.socket_path.is_absolute() {
            return Err(configuration("socket_path must be absolute"));
        }
        if self.socket_path.as_os_str().len() > 4096
            || self.socket_path.to_string_lossy().contains('\0')
        {
            return Err(configuration("socket_path is too long or contains NUL"));
        }
        if let Some(executable) = &self.executable {
            validate_executable(executable)?;
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
}

fn validate_executable(executable: &Path) -> Result<(), HerdrError> {
    if !executable.is_absolute() {
        return Err(configuration("executable must be absolute"));
    }
    let executable = std::fs::canonicalize(executable)
        .map_err(|_| HerdrError::Unavailable("configured executable does not exist".into()))?;
    let metadata = std::fs::metadata(&executable)
        .map_err(|_| HerdrError::Unavailable("cannot inspect configured executable".into()))?;
    if !metadata.is_file() {
        return Err(HerdrError::Unavailable("configured executable is not a regular file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(HerdrError::Unavailable("configured file is not executable".into()));
        }
    }
    Ok(())
}

impl Binding {
    /// The configured stable terminal ID for an alias or for the ID itself.
    fn resolve(&self, target: Option<&str>) -> Result<&str, HerdrError> {
        let target =
            target.ok_or_else(|| invalid_request("an explicit configured target is required"))?;
        self.targets
            .get(target)
            .or_else(|| self.targets.values().find(|id| id.as_str() == target))
            .map(String::as_str)
            .ok_or_else(|| {
                HerdrError::Denied("target is not configured for this principal/workspace".into())
            })
    }
}

/// Sequential requests against one Herdr server under one operation deadline.
struct Client<'a> {
    socket_path: &'a Path,
    deadline: Instant,
    nonce: u64,
    sequence: u64,
}

impl<'a> Client<'a> {
    /// Verify the server speaks a supported protocol before any agent request.
    async fn connect(socket_path: &'a Path, deadline: Instant) -> Result<Self, HerdrError> {
        let mut client = Self { socket_path, deadline, nonce: rand::random(), sequence: 0 };
        let pong = client.call("ping", json!({}), false).await?;
        expect_type(&pong, "pong", false)?;
        let protocol = pong["protocol"].as_u64();
        if !protocol.is_some_and(|protocol| SUPPORTED_PROTOCOLS.contains(&protocol)) {
            let version = pong["version"].as_str().map_or_else(|| "unknown".into(), printable);
            let protocol = protocol.map_or_else(|| "unknown".into(), |p| p.to_string());
            return Err(HerdrError::Unavailable(format!(
                "Herdr {version} speaks socket protocol {protocol}; this adapter supports \
                 protocol {SUPPORTED_PROTOCOLS:?}"
            )));
        }
        Ok(client)
    }

    /// Live agents as reported by `agent.list`.
    async fn list(&mut self) -> Result<Vec<Value>, HerdrError> {
        let mut result = self.call("agent.list", json!({}), false).await?;
        expect_type(&result, "agent_list", false)?;
        match result["agents"].take() {
            Value::Array(agents) => Ok(agents),
            _ => Err(invalid_response("agent list is not an array")),
        }
    }

    /// Resolve and verify a configured stable terminal ID.
    ///
    /// Herdr agent methods address a pane ID or a live agent name, never a
    /// terminal ID, and pane IDs change when a pane moves. The terminal ID is the
    /// stable handle, so look up the pane hosting it now, then confirm through
    /// `agent.get` on that pane that the same terminal answers. Returns the agent
    /// and the pane to address follow-up requests to.
    async fn resolve(&mut self, id: &str) -> Result<(Value, String), HerdrError> {
        let pane = self
            .list()
            .await?
            .iter()
            .find(|agent| agent["terminal_id"] == id)
            .and_then(|agent| agent["pane_id"].as_str())
            .filter(|pane| valid_target(pane))
            .map(str::to_string)
            .ok_or_else(|| {
                HerdrError::Unavailable("configured terminal hosts no live agent".into())
            })?;
        let result = self.call("agent.get", json!({ "target": pane }), false).await?;
        Ok((verified_agent(result, id, "agent_info", false)?, pane))
    }

    async fn read(&mut self, id: &str, pane: &str, lines: u32) -> Result<Value, HerdrError> {
        let params =
            json!({ "target": pane, "source": "recent", "lines": lines, "format": "text" });
        let result = self.call("agent.read", params, false).await?;
        expect_type(&result, "pane_read", false)?;
        let read = &result["read"];
        if read["pane_id"] != pane {
            return Err(invalid_response("read answered for a different pane"));
        }
        let text = read["text"].as_str().ok_or_else(|| invalid_response("read text missing"))?;
        let truncated = read["truncated"].as_bool().unwrap_or(false);
        Ok(json!({ "terminal_id": id, "source": "recent", "text": text, "truncated": truncated }))
    }

    async fn explain(&mut self, id: &str, pane: &str) -> Result<Value, HerdrError> {
        let mut result = self.call("agent.explain", json!({ "target": pane }), false).await?;
        expect_type(&result, "agent_explain", false)?;
        let explain = result["explain"].take();
        if !explain.is_object() || !explain["state"].is_string() {
            return Err(invalid_response("explain result is not an explanation object"));
        }
        Ok(json!({ "terminal_id": id, "explain": explain }))
    }

    async fn wait(
        &mut self,
        id: &str,
        pane: &str,
        request: &HerdrRequest,
    ) -> Result<Value, HerdrError> {
        let params = json!({
            "target": pane,
            "until": request.until,
            "timeout_ms": self.wait_budget(request.timeout_ms.unwrap_or(1000)),
        });
        let result = self.call("agent.wait", params, false).await?;
        Ok(json!({ "agent": verified_agent(result, id, "agent_info", false)? }))
    }

    /// Submit verbatim text; with `until`/`timeout_ms`, Herdr also waits for the
    /// lifecycle in the same request.
    async fn prompt(
        &mut self,
        id: &str,
        pane: &str,
        request: HerdrRequest,
    ) -> Result<Value, HerdrError> {
        let mut params = json!({ "target": pane, "text": request.text.unwrap_or_default() });
        if request.timeout_ms.is_some() || !request.until.is_empty() {
            let budget = self.deadline.saturating_duration_since(Instant::now()).as_millis();
            let requested = request.timeout_ms.unwrap_or(u64::try_from(budget).unwrap_or(0));
            params["wait"] =
                json!({ "until": request.until, "timeout_ms": self.wait_budget(requested) });
        }
        let result = self.call("agent.prompt", params, true).await?;
        Ok(json!({ "agent": verified_agent(result, id, "agent_prompted", true)? }))
    }

    /// Cap a server-side wait so Herdr answers before the operation deadline.
    fn wait_budget(&self, requested_ms: u64) -> u64 {
        let remaining = self.deadline.saturating_duration_since(Instant::now()).as_millis();
        let remaining = u64::try_from(remaining).unwrap_or(u64::MAX);
        let margin = (remaining / 10).clamp(20, 500);
        requested_ms.min(remaining.saturating_sub(margin)).max(1)
    }

    /// Send one request and return its `result` object.
    ///
    /// `delivers_input` marks a prompt: once its request line is written, any
    /// failure other than a known pre-delivery rejection is uncertain delivery.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        delivers_input: bool,
    ) -> Result<Value, HerdrError> {
        self.sequence += 1;
        let id = format!("winx:{:016x}:{}", self.nonce, self.sequence);
        let mut line = serde_json::to_vec(&json!({ "id": id, "method": method, "params": params }))
            .map_err(|_| invalid_request("cannot encode request"))?;
        line.push(b'\n');
        if line.len() > REQUEST_LIMIT {
            return Err(invalid_request("request exceeds its byte bound"));
        }
        let response =
            transport::exchange(self.socket_path, &line, self.deadline, delivers_input).await?;
        decode_response(&response, &id, delivers_input)
    }
}

fn decode_response(bytes: &[u8], id: &str, delivers_input: bool) -> Result<Value, HerdrError> {
    let uncertain = |error| after_send_error(error, delivers_input);
    let mut response: Value = serde_json::from_slice(bytes)
        .map_err(|_| uncertain(invalid_response("response is not JSON")))?;
    if response["id"].as_str() != Some(id) {
        return Err(uncertain(invalid_response("response id does not match the request")));
    }
    if let Some(error) = response.get("error") {
        let code = error["code"]
            .as_str()
            .ok_or_else(|| uncertain(invalid_response("error response has no code")))?;
        return Err(remote_error(code, delivers_input));
    }
    match response["result"].take() {
        result @ Value::Object(_) => Ok(result),
        _ => Err(uncertain(invalid_response("response has no result object"))),
    }
}

/// Map a Herdr error code. Herdr messages are not forwarded: they can describe
/// unconfigured agents (for example ambiguous-target candidates).
fn remote_error(code: &str, delivers_input: bool) -> HerdrError {
    let code = if !code.is_empty()
        && code.len() <= 64
        && code.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        code
    } else {
        "unrecognized"
    };
    if delivers_input && !PRE_DELIVERY_PROMPT_ERRORS.contains(&code) {
        return HerdrError::DeliveryUncertain(format!("Herdr reported {code}"));
    }
    if code == "agent_blocked" {
        HerdrError::AgentBlocked
    } else {
        HerdrError::Rejected { code: code.into() }
    }
}

fn expect_type(result: &Value, expected: &str, delivers_input: bool) -> Result<(), HerdrError> {
    if result["type"] == expected {
        Ok(())
    } else {
        Err(after_send_error(invalid_response("unexpected result type"), delivers_input))
    }
}

fn verified_agent(
    mut result: Value,
    id: &str,
    expected_type: &str,
    delivers_input: bool,
) -> Result<Value, HerdrError> {
    expect_type(&result, expected_type, delivers_input)?;
    let agent = result["agent"].take();
    if agent["terminal_id"] != id {
        return Err(after_send_error(
            invalid_response("returned terminal identity does not match the configured stable ID"),
            delivers_input,
        ));
    }
    Ok(agent)
}

#[cfg(unix)]
mod transport {
    use super::{after_send_error, HerdrError, RESPONSE_LIMIT};
    use std::path::Path;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::time::{timeout_at, Instant};

    /// One request line in, one bounded response line out, on a fresh connection.
    pub(super) async fn exchange(
        socket_path: &Path,
        line: &[u8],
        deadline: Instant,
        delivers_input: bool,
    ) -> Result<Vec<u8>, HerdrError> {
        if Instant::now() >= deadline {
            return Err(HerdrError::Timeout { uncertain: false });
        }
        check_socket(socket_path)?;
        let mut stream = match timeout_at(deadline, UnixStream::connect(socket_path)).await {
            Err(_) => return Err(HerdrError::Timeout { uncertain: false }),
            Ok(Err(_)) => return Err(unavailable("cannot connect to the configured socket")),
            Ok(Ok(stream)) => stream,
        };
        // The path check above can race a replacement; the peer credential cannot.
        let peer = stream.peer_cred().map_err(|_| unavailable("cannot verify the socket peer"))?;
        if peer.uid() != crate::os::unix::effective_uid() {
            return Err(unavailable("the socket peer is not the current user"));
        }
        // Herdr acts only on a complete line, and the newline is the final byte,
        // so an unfinished write cannot have delivered the request.
        match timeout_at(deadline, stream.write_all(line)).await {
            Err(_) => return Err(HerdrError::Timeout { uncertain: false }),
            Ok(Err(_)) => return Err(HerdrError::Failed("cannot write the request".into())),
            Ok(Ok(())) => {}
        }
        // Keep the write half open: Herdr treats a closed client as cancelling a wait.
        match timeout_at(deadline, read_line(&mut stream)).await {
            Err(_) => Err(HerdrError::Timeout { uncertain: delivers_input }),
            Ok(result) => result.map_err(|error| after_send_error(error, delivers_input)),
        }
    }

    /// The configured path must be a private socket owned by the current user.
    fn check_socket(path: &Path) -> Result<(), HerdrError> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let metadata = std::fs::metadata(path)
            .map_err(|_| unavailable("the configured socket does not exist"))?;
        if !metadata.file_type().is_socket() {
            return Err(unavailable("the configured socket_path is not a socket"));
        }
        if metadata.uid() != crate::os::unix::effective_uid() {
            return Err(unavailable("the configured socket is not owned by the current user"));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(unavailable("the configured socket must be private (mode 0600)"));
        }
        Ok(())
    }

    async fn read_line(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>, HerdrError> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let length = stream
                .read(&mut buffer)
                .await
                .map_err(|_| HerdrError::Failed("cannot read the response".into()))?;
            if length == 0 {
                return Err(HerdrError::Failed(
                    "connection closed before a complete response".into(),
                ));
            }
            let chunk = &buffer[..length];
            let end = chunk.iter().position(|byte| *byte == b'\n');
            if bytes.len() + end.unwrap_or(length) > RESPONSE_LIMIT {
                return Err(HerdrError::OutputLimit);
            }
            bytes.extend_from_slice(&chunk[..end.unwrap_or(length)]);
            if end.is_some() {
                return Ok(bytes);
            }
        }
    }

    fn unavailable(message: &str) -> HerdrError {
        HerdrError::Unavailable(message.into())
    }
}

#[cfg(not(unix))]
mod transport {
    use super::HerdrError;
    use std::path::Path;
    use tokio::time::Instant;

    // Keeps the Unix transport's async signature for the shared caller.
    #[allow(clippy::unused_async)]
    pub(super) async fn exchange(
        _socket_path: &Path,
        _line: &[u8],
        _deadline: Instant,
        _delivers_input: bool,
    ) -> Result<Vec<u8>, HerdrError> {
        Err(HerdrError::Unavailable("the Herdr socket transport requires a Unix socket".into()))
    }
}

fn validate_request(request: &HerdrRequest, allow_prompt: bool) -> Result<(), HerdrError> {
    if request.text.is_some() && request.action != HerdrAction::Prompt {
        return Err(invalid_request("text is only supported for prompt"));
    }
    if request.lines.is_some() && request.action != HerdrAction::Read {
        return Err(invalid_request("lines is only supported for read"));
    }
    if (request.timeout_ms.is_some() || !request.until.is_empty())
        && !matches!(request.action, HerdrAction::Wait | HerdrAction::Prompt)
    {
        return Err(invalid_request("timeout_ms and until are only supported for wait and prompt"));
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

/// A short, printable rendering of server-reported text for diagnostics.
fn printable(value: &str) -> String {
    value.chars().filter(char::is_ascii_graphic).take(64).collect()
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
fn after_send_error(error: HerdrError, delivers_input: bool) -> HerdrError {
    if delivers_input {
        HerdrError::DeliveryUncertain(error.to_string())
    } else {
        error
    }
}
