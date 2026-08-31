//! Privacy-preserving operational telemetry for MCP tool calls.
//!
//! This module deliberately records classifications and counters only. Raw
//! arguments, command text, paths outside the existing workspace label, and
//! file contents never become usage-log fields.

use std::fmt::Write as _;

use axum::http::request::Parts;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult},
    service::{RequestContext, RoleServer},
};
use sha2::{Digest, Sha256};

use super::principal::RequestScope;
use crate::build_info::BuildIdentity;

const USAGE_SCHEMA_VERSION: u64 = 1;

pub(super) struct UsageEvent {
    tool: String,
    action: String,
    command_kind: &'static str,
    ws: String,
    principal: String,
    thread_id: String,
    request_id: String,
    client_name: String,
    client_version: String,
    protocol: String,
    client_session: String,
    conversation_source: String,
    conversation_id: String,
    workspace_id: String,
    workspace_coherence: String,
    batch_items: usize,
    worker_limit: usize,
    build: BuildIdentity,
    started: std::time::Instant,
}

#[derive(Default)]
pub(super) struct UsageRecoveryMetadata<'a> {
    pub next_action_tool: &'a str,
    pub required_read_count: u64,
    pub retry_same_call: bool,
    pub edit_applied: bool,
    pub fresh_read_required: bool,
    pub verification_status: &'a str,
    pub mutation_transition: &'a str,
    pub mutation_receipt_state: &'static str,
    pub recovery_attempt: u64,
    pub recovery_level: &'static str,
}

#[derive(Default)]
pub(super) struct UsageResultMetadata<'a> {
    pub error_code: &'a str,
    pub recovery: UsageRecoveryMetadata<'a>,
    pub source_kind: &'a str,
    pub payload_bytes: u64,
    pub source_bytes: u64,
    pub image_transcoded: bool,
    pub image_deduplicated: bool,
    pub temporary_session_files: u64,
    pub temporary_session_bytes: u64,
    pub temporary_stale_pruned_files: u64,
    pub temporary_stale_pruned_bytes: u64,
    pub temporary_over_budget: bool,
    pub edit_file_count: u64,
}

pub(super) fn usage_result_metadata(result: Option<&CallToolResult>) -> UsageResultMetadata<'_> {
    let structured = result.and_then(|result| result.structured_content.as_ref());
    let data = structured.and_then(|structured| structured.get("data"));
    let domain_data = data.and_then(|data| data.get("result")).or(data).or(structured);
    let temporary = data.and_then(|data| data.get("temporary_artifact_budget")).or_else(|| {
        domain_data.filter(|data| {
            data.get("temporary_artifact_cleanup_required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
    });
    UsageResultMetadata {
        error_code: structured
            .and_then(|structured| structured.get("errorCode"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        recovery: usage_recovery_metadata(structured, domain_data),
        source_kind: structured
            .and_then(|structured| structured.get("sourceKind"))
            .or_else(|| domain_data.and_then(|data| data.get("source_kind")))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        payload_bytes: structured
            .and_then(|structured| structured.get("payloadBytes"))
            .or_else(|| domain_data.and_then(|data| data.get("payload_bytes")))
            .or_else(|| domain_data.and_then(|data| data.get("delivered_bytes")))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        source_bytes: domain_data
            .and_then(|data| data.get("source_bytes"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        image_transcoded: domain_data
            .and_then(|data| data.get("transcoded"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        image_deduplicated: domain_data
            .and_then(|data| data.get("deduplicated"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        temporary_session_files: temporary
            .and_then(|temporary| temporary.get("session_files"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        temporary_session_bytes: temporary
            .and_then(|temporary| temporary.get("session_bytes"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        temporary_stale_pruned_files: temporary
            .and_then(|temporary| temporary.get("stale_pruned_files"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        temporary_stale_pruned_bytes: temporary
            .and_then(|temporary| temporary.get("stale_pruned_bytes"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        temporary_over_budget: temporary
            .and_then(|temporary| temporary.get("over_budget"))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| {
                temporary
                    .and_then(|temporary| temporary.get("temporary_artifact_cleanup_required"))
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(false),
        edit_file_count: domain_data
            .and_then(|data| data.get("file_count"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    }
}

fn usage_recovery_metadata<'a>(
    structured: Option<&'a serde_json::Value>,
    domain_data: Option<&'a serde_json::Value>,
) -> UsageRecoveryMetadata<'a> {
    let receipt_persisted = domain_data
        .and_then(|data| data.get("mutation_receipt_persisted"))
        .and_then(serde_json::Value::as_bool);
    let recovery_escalated = domain_data
        .and_then(|data| data.get("recovery_escalated"))
        .and_then(serde_json::Value::as_bool);
    UsageRecoveryMetadata {
        next_action_tool: structured
            .and_then(|structured| structured.get("nextAction"))
            .and_then(|action| action.get("tool"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        required_read_count: structured
            .and_then(|structured| structured.get("requiredReads"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, |reads| u64::try_from(reads.len()).unwrap_or(u64::MAX)),
        retry_same_call: structured
            .and_then(|structured| structured.get("retrySameCall"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        edit_applied: domain_data
            .and_then(|data| data.get("edit_applied"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        fresh_read_required: domain_data
            .and_then(|data| data.get("fresh_read_required"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        verification_status: domain_data
            .and_then(|data| data.get("verification_status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        mutation_transition: domain_data
            .and_then(|data| data.get("mutation_transition"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        mutation_receipt_state: match receipt_persisted {
            Some(true) => "persisted",
            Some(false) => "volatile",
            None => "",
        },
        recovery_attempt: domain_data
            .and_then(|data| data.get("recovery_attempt"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        recovery_level: match recovery_escalated {
            Some(true) => "escalated",
            Some(false) => "normal",
            None => "",
        },
    }
}

impl UsageEvent {
    pub fn start(
        request: &CallToolRequestParams,
        scope: &RequestScope,
        context: &RequestContext<RoleServer>,
    ) -> Self {
        let arguments = request.arguments.as_ref();
        let thread_id = arguments
            .and_then(|arguments| arguments.get("thread_id"))
            .and_then(serde_json::Value::as_str)
            .map(|thread_id| scope.unscope_text(thread_id))
            .unwrap_or_default();
        let (action, command_kind) = if request.name == "BashCommand" {
            (bash_action(arguments), bash_command_kind(arguments))
        } else if request.name == "VerifyEdit" {
            ("verification".to_string(), "")
        } else {
            (String::new(), "")
        };
        let ws = if request.name == "Initialize" {
            arguments
                .and_then(|arguments| arguments.get("any_workspace_path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        };
        let workspace = if request.name == "Initialize" {
            arguments
                .and_then(|arguments| arguments.get("any_workspace_path"))
                .and_then(serde_json::Value::as_str)
        } else {
            arguments
                .and_then(|arguments| arguments.get("workspace_root"))
                .and_then(serde_json::Value::as_str)
        }
        .unwrap_or_default();
        let workspace_id =
            if workspace.is_empty() { String::new() } else { short_fingerprint("w", workspace) };
        let client = context.client_info();
        let client_name =
            client.as_ref().map_or("unknown", |client| client.name.as_str()).to_string();
        let client_version =
            client.as_ref().map_or("unknown", |client| client.version.as_str()).to_string();
        let protocol = context
            .protocol_version()
            .map_or_else(|| "unknown".to_string(), |version| version.to_string());
        let request_id = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<crate::http_server::RequestCorrelation>())
            .map_or_else(
                || {
                    serde_json::to_string(&context.id)
                        .map_or_else(|_| "unknown".to_string(), |id| short_fingerprint("r", &id))
                },
                |correlation| correlation.as_str().to_string(),
            );
        let client_session = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .and_then(|value| value.to_str().ok())
            .map_or_else(|| "stateless".to_string(), |id| short_fingerprint("s", id));
        let (conversation_source, conversation_id) = conversation_telemetry(context);
        let batch_items = match request.name.as_ref() {
            "ReadFiles" => arguments
                .and_then(|arguments| arguments.get("file_paths"))
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            _ => 0,
        };
        let worker_limit = if request.name == "ReadFiles" {
            crate::tools::read_files::configured_parallelism()
        } else {
            0
        };
        Self {
            tool: request.name.to_string(),
            action,
            command_kind,
            ws,
            principal: scope
                .principal()
                .map_or_else(|| "local".to_string(), |principal| principal.name().to_string()),
            thread_id,
            request_id,
            client_name,
            client_version,
            protocol,
            client_session,
            conversation_source,
            conversation_id,
            workspace_id,
            workspace_coherence: "unchecked".to_string(),
            batch_items,
            worker_limit,
            build: BuildIdentity::current(),
            started: std::time::Instant::now(),
        }
    }

    pub fn set_workspace_coherence(&mut self, value: &str) {
        self.workspace_coherence = value.to_string();
    }

    pub fn emit(
        &self,
        outcome: &str,
        result_status: &str,
        response_bytes: usize,
        result: Option<&CallToolResult>,
    ) {
        if self.tool == "Initialize" {
            self.emit_initialize(outcome, result_status, response_bytes, result);
        } else {
            self.emit_non_initialize(outcome, result_status, response_bytes, result);
        }
    }

    fn emit_initialize(
        &self,
        outcome: &str,
        result_status: &str,
        response_bytes: usize,
        result: Option<&CallToolResult>,
    ) {
        let data = result
            .and_then(|result| result.structured_content.as_ref())
            .and_then(|structured| structured.get("data"));
        let initialize_transition = string_field(data, "initialize_transition");
        let initialize_reused = bool_field(data, "initialize_reused");
        let initialize_recovered_missing_session =
            bool_field(data, "initialize_recovered_missing_session");
        let initialize_response_mode = string_field(data, "initialize_response_mode");
        let context_bytes = u64_field(data, "context_bytes");
        let guidelines_bytes = u64_field(data, "guidelines_bytes");
        let initial_files_count = u64_field(data, "initial_files_count");
        let code_writer_policy_strength = string_field(data, "code_writer_policy_strength");
        let shell_spawners_present = bool_field(data, "shell_spawners_present");
        let shell_reset_performed = bool_field(data, "shell_reset_performed");
        let shell_reset_retry_after_seconds = u64_field(data, "shell_reset_retry_after_seconds");
        let temporary_stale_pruned_files = u64_field(data, "temporary_artifact_stale_pruned_files");
        let temporary_stale_pruned_bytes = u64_field(data, "temporary_artifact_stale_pruned_bytes");
        let error_code = usage_result_metadata(result).error_code;
        tracing::info!(
            target: "winx::usage",
            event = "tool_call",
            usage_schema = USAGE_SCHEMA_VERSION,
            tool = %self.tool,
            action = %self.action,
            command_kind = self.command_kind,
            ws = %self.ws,
            principal = %self.principal,
            thread_id = %self.thread_id,
            request_id = %self.request_id,
            client_name = %self.client_name,
            client_version = %self.client_version,
            protocol = %self.protocol,
            client_session = %self.client_session,
            conversation_source = %self.conversation_source,
            conversation_id = %self.conversation_id,
            workspace_id = %self.workspace_id,
            workspace_coherence = %self.workspace_coherence,
            build = %self.build.display,
            build_version = %self.build.package_version,
            build_revision = %self.build.revision,
            build_dirty = self.build.dirty,
            batch_items = self.batch_items,
            worker_limit = self.worker_limit,
            initialize_transition,
            initialize_reused,
            initialize_recovered_missing_session,
            initialize_response_mode,
            context_bytes,
            guidelines_bytes,
            initial_files_count,
            code_writer_policy_strength,
            shell_spawners_present,
            shell_reset_performed,
            shell_reset_retry_after_seconds,
            temporary_stale_pruned_files,
            temporary_stale_pruned_bytes,
            error_code,
            result_status,
            response_bytes,
            duration_ms = elapsed_ms(self.started),
            outcome,
            "tool call"
        );
    }

    fn emit_non_initialize(
        &self,
        outcome: &str,
        result_status: &str,
        response_bytes: usize,
        result: Option<&CallToolResult>,
    ) {
        let metadata = usage_result_metadata(result);
        let batch_items = usize::try_from(metadata.edit_file_count)
            .ok()
            .filter(|count| *count > 0)
            .unwrap_or(self.batch_items);
        tracing::info!(
            target: "winx::usage",
            event = "tool_call",
            usage_schema = USAGE_SCHEMA_VERSION,
            tool = %self.tool,
            action = %self.action,
            command_kind = self.command_kind,
            ws = %self.ws,
            principal = %self.principal,
            thread_id = %self.thread_id,
            request_id = %self.request_id,
            client_name = %self.client_name,
            client_version = %self.client_version,
            protocol = %self.protocol,
            client_session = %self.client_session,
            conversation_source = %self.conversation_source,
            conversation_id = %self.conversation_id,
            workspace_id = %self.workspace_id,
            workspace_coherence = %self.workspace_coherence,
            build = %self.build.display,
            build_version = %self.build.package_version,
            build_revision = %self.build.revision,
            build_dirty = self.build.dirty,
            batch_items,
            worker_limit = self.worker_limit,
            error_code = metadata.error_code,
            next_action_tool = metadata.recovery.next_action_tool,
            required_read_count = metadata.recovery.required_read_count,
            retry_same_call = metadata.recovery.retry_same_call,
            edit_applied = metadata.recovery.edit_applied,
            fresh_read_required = metadata.recovery.fresh_read_required,
            verification_status = metadata.recovery.verification_status,
            mutation_transition = metadata.recovery.mutation_transition,
            mutation_receipt_state = metadata.recovery.mutation_receipt_state,
            recovery_attempt = metadata.recovery.recovery_attempt,
            recovery_level = metadata.recovery.recovery_level,
            source_kind = metadata.source_kind,
            payload_bytes = metadata.payload_bytes,
            source_bytes = metadata.source_bytes,
            image_transcoded = metadata.image_transcoded,
            image_deduplicated = metadata.image_deduplicated,
            temporary_session_files = metadata.temporary_session_files,
            temporary_session_bytes = metadata.temporary_session_bytes,
            temporary_stale_pruned_files = metadata.temporary_stale_pruned_files,
            temporary_stale_pruned_bytes = metadata.temporary_stale_pruned_bytes,
            temporary_over_budget = metadata.temporary_over_budget,
            result_status,
            response_bytes,
            duration_ms = elapsed_ms(self.started),
            outcome,
            "tool call"
        );
    }
}

fn string_field<'a>(value: Option<&'a serde_json::Value>, key: &str) -> &'a str {
    value.and_then(|value| value.get(key)).and_then(serde_json::Value::as_str).unwrap_or_default()
}

fn bool_field(value: Option<&serde_json::Value>, key: &str) -> bool {
    value.and_then(|value| value.get(key)).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

fn u64_field(value: Option<&serde_json::Value>, key: &str) -> u64 {
    value.and_then(|value| value.get(key)).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn short_fingerprint(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = format!("{prefix}_");
    for byte in &digest[..8] {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn conversation_telemetry(context: &RequestContext<RoleServer>) -> (String, String) {
    let Some(parts) = context.extensions.get::<Parts>() else {
        return ("none".to_string(), String::new());
    };
    for (header, source) in
        [("mcp-session-id", "mcp_session"), ("x-winx-conversation-id", "gateway_header")]
    {
        if let Some(value) = parts
            .headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return (source.to_string(), short_fingerprint("c", value));
        }
    }
    ("none".to_string(), String::new())
}

/// Classify a `BashCommand` call by action kind without retaining command text.
fn bash_action(arguments: Option<&rmcp::model::JsonObject>) -> String {
    use serde_json::Value;
    const KINDS: [&str; 7] = [
        "command",
        "status_check",
        "send_text",
        "send_specials",
        "send_ascii",
        "screen",
        "wait_for_turn",
    ];
    let Some(arguments) = arguments else {
        return String::new();
    };
    let parsed;
    let action = match arguments.get("action_json") {
        Some(Value::Object(object)) => object,
        Some(Value::String(text)) => {
            match serde_json::from_str::<Value>(&text.replace('\n', " ")) {
                Ok(Value::Object(object)) => {
                    parsed = object;
                    &parsed
                }
                _ => return "command".to_string(),
            }
        }
        _ => arguments,
    };
    if let Some(kind) = action.get("type").and_then(Value::as_str) {
        return kind.to_string();
    }
    KINDS.iter().find(|kind| action.contains_key(**kind)).map_or("?", |kind| kind).to_string()
}

/// Return a fixed, privacy-safe command category. Exact executables, arguments,
/// URLs, filenames, environment values, and shell text are never returned.
fn bash_command_kind(arguments: Option<&rmcp::model::JsonObject>) -> &'static str {
    let Some(arguments) = arguments else {
        return "";
    };
    let command = match arguments.get("action_json") {
        Some(serde_json::Value::Object(action)) => {
            action.get("command").and_then(serde_json::Value::as_str).map(ToString::to_string)
        }
        Some(serde_json::Value::String(text)) => {
            serde_json::from_str::<serde_json::Value>(&text.replace('\n', " "))
                .ok()
                .and_then(|value| {
                    value
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
                .or_else(|| Some(text.clone()))
        }
        _ => arguments.get("command").and_then(serde_json::Value::as_str).map(ToString::to_string),
    };
    command.as_deref().map_or("", classify_command)
}

fn classify_command(command: &str) -> &'static str {
    let mut executable = "";
    for word in command.split_whitespace() {
        let word = word.trim_matches(|character: char| {
            matches!(character, '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']')
        });
        if word.is_empty()
            || word == "env"
            || word.starts_with('-')
            || (word.contains('=') && !word.starts_with(['/', '.']))
            || matches!(word, "command" | "exec" | "nohup" | "nice" | "sudo" | "time")
        {
            continue;
        }
        executable = word.rsplit('/').next().unwrap_or(word);
        break;
    }
    match executable {
        "cargo" | "rustc" | "rustfmt" | "rustup" => "rust_toolchain",
        "node" | "npm" | "npx" | "pnpm" | "yarn" | "bun" | "deno" => "javascript_toolchain",
        "python" | "python3" | "pip" | "pip3" | "uv" | "ruff" | "pytest" | "mypy" | "pyright" => {
            "python_toolchain"
        }
        "go" => "go_toolchain",
        "make" | "cmake" | "ninja" | "meson" | "just" | "gradle" | "mvn" => "build_tool",
        "git" | "hg" | "svn" => "vcs",
        "rg" | "grep" | "fd" | "find" | "locate" => "search",
        "cat" | "head" | "tail" | "sed" | "awk" | "ls" | "tree" | "stat" | "file" | "wc"
        | "pwd" | "realpath" | "readlink" => "filesystem_read",
        "cp" | "mv" | "rm" | "mkdir" | "rmdir" | "touch" | "install" | "ln" | "chmod" | "chown" => {
            "filesystem_write"
        }
        "curl" | "wget" | "ssh" | "scp" | "sftp" | "rsync" | "nc" => "network",
        "ps" | "pgrep" | "pkill" | "kill" | "killall" | "top" | "htop" | "systemctl"
        | "journalctl" => "process_control",
        "docker" | "podman" | "kubectl" | "helm" => "container",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "" => "",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_classification_is_fixed_and_does_not_echo_sensitive_text() {
        let sensitive = "TOKEN=super-secret curl https://private.example/token";
        let kind = classify_command(sensitive);
        assert_eq!(kind, "network");
        assert!(!sensitive.contains(kind));
        assert!(!kind.contains("secret"));
        assert_eq!(classify_command("cargo test --all-features"), "rust_toolchain");
        assert_eq!(classify_command("/usr/bin/rg password ."), "search");
    }
}
