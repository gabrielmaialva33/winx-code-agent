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

pub(super) mod lifecycle;
mod workflow;

use lifecycle::{Phase, TraceSummary, UsageTrace};

use workflow::{UsageWorkflowRequestMetadata, UsageWorkflowResultMetadata};

const USAGE_SCHEMA_VERSION: u64 = 1;

pub(super) struct UsageEvent {
    tool: String,
    action: String,
    command_kind: &'static str,
    requested_wait: UsageWaitMetadata,
    workflow: Option<UsageWorkflowRequestMetadata>,
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
    trace: UsageTrace,
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
    pub execution: UsageExecutionMetadata,
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
        execution: usage_execution_metadata(domain_data),
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
        verification_status: verification_status(structured, domain_data),
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

#[derive(Default)]
pub(super) struct UsageExecutionMetadata {
    pub exit_code: Option<i64>,
    pub output_truncated: Option<bool>,
    pub execution_id: String,
    pub bg_command_id: String,
    pub verification_id: String,
    pub mutation_receipt_id: String,
    pub verification_replayed: Option<bool>,
    pub command_started: Option<bool>,
    pub shell_busy: Option<bool>,
}

#[derive(Default)]
struct UsageWaitMetadata {
    policy: Option<&'static str>,
    milliseconds: Option<u64>,
}

fn execution_data(domain: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    match domain.and_then(|data| data.get("verification")) {
        Some(verification) => verification.get("data"),
        None => domain,
    }
}

fn execution_exit_code(domain: Option<&serde_json::Value>) -> Option<i64> {
    let data = execution_data(domain);
    if optional_bool(data, "command_started") == Some(false) {
        return None;
    }
    data.and_then(|data| data.get("exit_code")).and_then(serde_json::Value::as_i64).or_else(|| {
        domain
            .and_then(|data| data.get("verification_exit_code"))
            .and_then(serde_json::Value::as_i64)
    })
}

fn optional_bool(data: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    data.and_then(|data| data.get(key)).and_then(serde_json::Value::as_bool)
}

fn optional_id<'a>(data: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    data.and_then(|data| data.get(key))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
}

fn fingerprinted_id(prefix: &str, id: Option<&str>) -> String {
    id.map_or_else(String::new, |id| short_fingerprint(prefix, id))
}

fn usage_execution_metadata(domain: Option<&serde_json::Value>) -> UsageExecutionMetadata {
    let data = execution_data(domain);
    let command_started = optional_bool(data, "command_started");
    let observed = if command_started == Some(false) { None } else { data };
    UsageExecutionMetadata {
        exit_code: execution_exit_code(domain),
        output_truncated: optional_bool(observed, "output_truncated"),
        execution_id: fingerprinted_id("e", optional_id(observed, "execution_id")),
        bg_command_id: fingerprinted_id("b", optional_id(observed, "bg_command_id")),
        verification_id: fingerprinted_id(
            "v",
            optional_id(domain, "verification_id").or_else(|| optional_id(data, "verification_id")),
        ),
        mutation_receipt_id: fingerprinted_id("m", optional_id(domain, "mutation_receipt_id")),
        verification_replayed: optional_bool(domain, "verification_replayed")
            .or_else(|| optional_bool(data, "verification_replayed")),
        command_started,
        shell_busy: optional_bool(data, "shell_busy").or_else(|| {
            optional_bool(domain.and_then(|data| data.get("session_status")), "command_running")
        }),
    }
}

fn verification_status<'a>(
    structured: Option<&'a serde_json::Value>,
    domain: Option<&'a serde_json::Value>,
) -> &'a str {
    if let Some(
        status @ ("completed" | "passed" | "failed" | "running" | "awaiting_input"
        | "awaiting_approval"),
    ) =
        domain.and_then(|data| data.get("verification_status")).and_then(serde_json::Value::as_str)
    {
        return status;
    }
    let nested = domain.and_then(|data| data.get("verification"));
    let is_verification = nested.is_some()
        || optional_id(domain, "verification_id").is_some()
        || string_field(domain, "action") == "verify"
        || string_field(structured, "tool") == "VerifyEdit";
    if !is_verification {
        return "";
    }
    let status = string_field(nested.or(structured), "status");
    // Legacy VerifyEdit deliberately reports verification_passed=false while active.
    if matches!(status, "running" | "awaiting_input" | "awaiting_approval") {
        return status;
    }
    if let Some(code) = execution_exit_code(domain) {
        return if code == 0 { "passed" } else { "failed" };
    }
    match optional_bool(domain, "verification_passed") {
        Some(true) => return "passed",
        Some(false) => return "failed",
        None => {}
    }
    if matches!(status, "failed" | "invalid_input" | "denied" | "not_found" | "recovery_exhausted")
    {
        return "failed";
    }
    ""
}

fn usage_wait_metadata(
    tool: &str,
    arguments: Option<&rmcp::model::JsonObject>,
) -> UsageWaitMetadata {
    if let Some(workflow) = UsageWorkflowRequestMetadata::from_request(tool, arguments) {
        return UsageWaitMetadata { policy: None, milliseconds: workflow.requested_wait_ms };
    }
    let wait_key = match tool {
        "BashCommand" | "VerifyEdit" => "wait_for_seconds",
        "EditFiles" => "verify_wait_for_seconds",
        _ => return UsageWaitMetadata::default(),
    };
    let policy = if tool == "BashCommand" {
        match arguments
            .and_then(|arguments| arguments.get("wait_policy"))
            .and_then(serde_json::Value::as_str)
        {
            Some("adaptive") => Some("adaptive"),
            Some("until_complete") => Some("until_complete"),
            Some("return_early") => Some("return_early"),
            _ => None,
        }
    } else {
        None
    };
    let milliseconds = arguments
        .and_then(|arguments| arguments.get(wait_key))
        .and_then(serde_json::Value::as_f64)
        .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());
    UsageWaitMetadata { policy, milliseconds }
}

fn usage_action(
    tool: &str,
    arguments: Option<&rmcp::model::JsonObject>,
    workflow: Option<UsageWorkflowRequestMetadata>,
) -> (String, &'static str) {
    if let Some(workflow) = workflow {
        (workflow.action.to_string(), "")
    } else if tool == "BashCommand" {
        (bash_action(arguments), bash_command_kind(arguments))
    } else if tool == "VerifyEdit" {
        ("verification".to_string(), "")
    } else {
        (String::new(), "")
    }
}

struct Emission<'a> {
    metadata: UsageResultMetadata<'a>,
    workflow: UsageWorkflowResultMetadata,
    phases: TraceSummary,
    batch_items: usize,
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
        let workflow = UsageWorkflowRequestMetadata::from_request(request.name.as_ref(), arguments);
        let (action, command_kind) = usage_action(request.name.as_ref(), arguments, workflow);
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
        let mut usage = Self {
            tool: request.name.to_string(),
            action,
            command_kind,
            requested_wait: usage_wait_metadata(request.name.as_ref(), arguments),
            workflow,
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
            trace: UsageTrace::default(),
        };
        usage.trace = UsageTrace::start(&usage);
        usage
    }

    pub(super) fn trace(&self) -> UsageTrace {
        self.trace.clone()
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
        let metadata = usage_result_metadata(result);
        let phases = self.trace.summary();
        let error_code = metadata.error_code;
        tracing::info!(
            target: "winx::usage",
            event = "tool_call",
            usage_schema = USAGE_SCHEMA_VERSION,
            lifecycle_schema = self.trace.schema(),
            usage_call_id = self.trace.call_id(),
            phase_workspace_check_ms = phases.milliseconds(Phase::WorkspaceCheck),
            phase_workspace_check_calls = phases.calls(Phase::WorkspaceCheck),
            phase_session_admission_ms = phases.milliseconds(Phase::SessionAdmission),
            phase_session_admission_calls = phases.calls(Phase::SessionAdmission),
            phase_catalog_ms = phases.milliseconds(Phase::Catalog),
            phase_catalog_calls = phases.calls(Phase::Catalog),
            phase_runtime_inspect_ms = phases.milliseconds(Phase::RuntimeInspect),
            phase_runtime_inspect_calls = phases.calls(Phase::RuntimeInspect),
            phase_runtime_capture_ms = phases.milliseconds(Phase::RuntimeCapture),
            phase_runtime_capture_calls = phases.calls(Phase::RuntimeCapture),
            phase_prompt_rpc_ms = phases.milliseconds(Phase::PromptRpc),
            phase_prompt_rpc_calls = phases.calls(Phase::PromptRpc),
            phase_herdr_adapter_ms = phases.milliseconds(Phase::HerdrAdapter),
            phase_herdr_adapter_calls = phases.calls(Phase::HerdrAdapter),
            phase_wait_idle_ms = phases.milliseconds(Phase::WaitIdle),
            phase_wait_idle_calls = phases.calls(Phase::WaitIdle),
            phase_temp_audit_ms = phases.milliseconds(Phase::TempAudit),
            phase_temp_audit_calls = phases.calls(Phase::TempAudit),
            phase_response_finalize_ms = phases.milliseconds(Phase::ResponseFinalize),
            phase_response_finalize_calls = phases.calls(Phase::ResponseFinalize),
            phase_abandoned_count = self.trace.schema().map(|_| phases.abandoned_count),
            phase_trace_suppressed_count = self.trace.schema().map(|_| phases.suppressed_count),

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
            shell_busy = metadata.execution.shell_busy,
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

    fn emission<'a>(&self, result: Option<&'a CallToolResult>) -> Emission<'a> {
        let metadata = usage_result_metadata(result);
        let phases = self.trace.summary();
        let workflow =
            self.workflow.map(|workflow| workflow.result(&self.tool, result)).unwrap_or_default();
        let batch_items = usize::try_from(metadata.edit_file_count)
            .ok()
            .filter(|count| *count > 0)
            .unwrap_or(self.batch_items);
        Emission { metadata, workflow, phases, batch_items }
    }

    fn emit_non_initialize(
        &self,
        outcome: &str,
        result_status: &str,
        response_bytes: usize,
        result: Option<&CallToolResult>,
    ) {
        let Emission { metadata, workflow, phases, batch_items } = self.emission(result);
        tracing::info!(
            target: "winx::usage",
            event = "tool_call",
            usage_schema = USAGE_SCHEMA_VERSION,
            lifecycle_schema = self.trace.schema(),
            usage_call_id = self.trace.call_id(),
            phase_workspace_check_ms = phases.milliseconds(Phase::WorkspaceCheck),
            phase_workspace_check_calls = phases.calls(Phase::WorkspaceCheck),
            phase_session_admission_ms = phases.milliseconds(Phase::SessionAdmission),
            phase_session_admission_calls = phases.calls(Phase::SessionAdmission),
            phase_catalog_ms = phases.milliseconds(Phase::Catalog),
            phase_catalog_calls = phases.calls(Phase::Catalog),
            phase_runtime_inspect_ms = phases.milliseconds(Phase::RuntimeInspect),
            phase_runtime_inspect_calls = phases.calls(Phase::RuntimeInspect),
            phase_runtime_capture_ms = phases.milliseconds(Phase::RuntimeCapture),
            phase_runtime_capture_calls = phases.calls(Phase::RuntimeCapture),
            phase_prompt_rpc_ms = phases.milliseconds(Phase::PromptRpc),
            phase_prompt_rpc_calls = phases.calls(Phase::PromptRpc),
            phase_herdr_adapter_ms = phases.milliseconds(Phase::HerdrAdapter),
            phase_herdr_adapter_calls = phases.calls(Phase::HerdrAdapter),
            phase_wait_idle_ms = phases.milliseconds(Phase::WaitIdle),
            phase_wait_idle_calls = phases.calls(Phase::WaitIdle),
            phase_temp_audit_ms = phases.milliseconds(Phase::TempAudit),
            phase_temp_audit_calls = phases.calls(Phase::TempAudit),
            phase_response_finalize_ms = phases.milliseconds(Phase::ResponseFinalize),
            phase_response_finalize_calls = phases.calls(Phase::ResponseFinalize),
            phase_abandoned_count = self.trace.schema().map(|_| phases.abandoned_count),
            phase_trace_suppressed_count = self.trace.schema().map(|_| phases.suppressed_count),

            tool = %self.tool,
            action = %self.action,
            workflow_schema = self.workflow.map(|_| 1_u64),
            workflow_backend = self.workflow.map(|workflow| workflow.backend),
            workflow_wait_kind = self.workflow.and_then(|workflow| workflow.wait_kind),
            workflow_wait_outcome = workflow.wait_outcome,
            prompt_receipt_present = workflow.receipt_present,
            prompt_delivery = workflow.delivery,
            prompt_activity_observed = workflow.activity_observed,
            prompt_outcome = workflow.prompt_outcome,
            agent_submission_status = workflow.submission_status,
            agent_input_sent = workflow.input_sent,
            agent_input_may_have_been_sent = workflow.input_may_have_been_sent,
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
            exit_code = metadata.execution.exit_code,
            output_truncated = metadata.execution.output_truncated,
            execution_id = %metadata.execution.execution_id,
            bg_command_id = %metadata.execution.bg_command_id,
            verification_id = %metadata.execution.verification_id,
            mutation_receipt_id = %metadata.execution.mutation_receipt_id,
            verification_replayed = metadata.execution.verification_replayed,
            command_started = metadata.execution.command_started,
            shell_busy = metadata.execution.shell_busy,
            requested_wait_policy = self.requested_wait.policy,
            requested_wait_ms = self.requested_wait.milliseconds,
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

    #[derive(Clone)]
    struct UsageCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for UsageCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn captured_usage(
        tool: &str,
        action: &str,
        envelope: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let mut arguments = serde_json::json!({"action":action});
        if tool == "Execution" {
            arguments["action"] = serde_json::json!({"type":action});
        }
        if let Some(backend) = envelope.get("data").and_then(|data| data.get("backend")) {
            arguments["backend"] = backend.clone();
        }
        captured_usage_request(tool, action, &arguments, Some(envelope))
    }

    fn captured_usage_request(
        tool: &str,
        action: &str,
        arguments: &serde_json::Value,
        envelope: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_bytes = bytes.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || UsageCapture(writer_bytes.clone()))
            .finish();
        let usage = fixture_usage(tool, action, arguments);
        let status = envelope
            .as_ref()
            .and_then(|envelope| envelope["status"].as_str())
            .unwrap_or("completed")
            .to_string();
        let result = envelope.map(|envelope| {
            let mut result = CallToolResult::success(Vec::new());
            result.structured_content = Some(envelope);
            result
        });
        tracing::subscriber::with_default(subscriber, || {
            usage.emit("ok", &status, 0, result.as_ref());
        });
        let captured = bytes.lock().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let event: serde_json::Value = serde_json::from_slice(&captured)?;
        Ok(event["fields"].clone())
    }

    fn fixture_usage(tool: &str, action: &str, arguments: &serde_json::Value) -> UsageEvent {
        UsageEvent {
            tool: tool.to_string(),
            action: UsageWorkflowRequestMetadata::from_request(tool, arguments.as_object())
                .map_or_else(|| action.to_string(), |workflow| workflow.action.to_string()),
            command_kind: "other",
            requested_wait: usage_wait_metadata(tool, arguments.as_object()),
            workflow: UsageWorkflowRequestMetadata::from_request(tool, arguments.as_object()),
            ws: String::new(),
            principal: "test-principal".to_string(),
            thread_id: "test-thread".to_string(),
            request_id: "request-fixture".to_string(),
            client_name: "test-client".to_string(),
            client_version: "1".to_string(),
            protocol: "test-protocol".to_string(),
            client_session: "stateless".to_string(),
            conversation_source: "none".to_string(),
            conversation_id: String::new(),
            workspace_id: "workspace-fixture".to_string(),
            workspace_coherence: "validated".to_string(),
            batch_items: 0,
            worker_limit: 0,
            build: BuildIdentity::current(),
            started: std::time::Instant::now(),
            trace: UsageTrace::default(),
        }
    }

    #[test]
    fn usage_json_preserves_zero_false_and_opaque_execution_identity() -> anyhow::Result<()> {
        let fields = captured_usage(
            "BashCommand",
            "command",
            serde_json::json!({
                "status":"completed", "tool":"BashCommand",
                "data": {
                    "action":"command", "exit_code":0, "output_truncated":false,
                    "execution_id":"execution-example", "bg_command_id":"background-example"
                }
            }),
        )?;
        assert_eq!(fields["exit_code"], 0);
        assert_eq!(fields["output_truncated"], false);
        assert_eq!(fields["execution_id"], "e_ce249da9eeb4e561");
        assert_eq!(fields["bg_command_id"], "b_51d1cad915c8c310");
        assert!(!fields.to_string().contains("execution-example"));
        assert!(!fields.to_string().contains("background-example"));
        Ok(())
    }

    #[test]
    fn usage_json_uses_nested_verification_execution_and_keeps_outer_receipt() -> anyhow::Result<()>
    {
        let fields = captured_usage(
            "EditFiles",
            "",
            serde_json::json!({
                "status":"completed_with_issues", "tool":"EditFiles", "errorCode":"verification_failed",
                "data": {
                    "edit_applied":true, "output_truncated":false,
                    "mutation_receipt_id":"receipt-example", "verification_id":"verification-example",
                    "verification_status":"failed", "verification_passed":false,
                    "verification_exit_code":7,
                    "verification": {
                        "status":"completed", "tool":"BashCommand",
                        "data":{"action":"verify", "exit_code":7, "output_truncated":true,
                                "execution_id":"execution-example"}
                    }
                }
            }),
        )?;
        assert_eq!(fields["exit_code"], 7);
        assert_eq!(fields["output_truncated"], true);
        assert_eq!(fields["verification_status"], "failed");
        assert_eq!(fields["execution_id"], "e_ce249da9eeb4e561");
        assert_eq!(fields["verification_id"], "v_7cd3a6e41039e056");
        assert_eq!(fields["mutation_receipt_id"], "m_922cfc84fd64e8f5");
        Ok(())
    }

    #[test]
    fn usage_json_active_legacy_verification_is_not_a_failed_check() -> anyhow::Result<()> {
        let fields = captured_usage(
            "VerifyEdit",
            "verification",
            serde_json::json!({
                "status":"running", "tool":"VerifyEdit",
                "data": {
                    "verification_id":"verification-example", "verification_passed":false,
                    "verification": {
                        "status":"running", "tool":"BashCommand",
                        "data":{"action":"verify", "output_truncated":false,
                                "execution_id":"execution-example"}
                    }
                }
            }),
        )?;
        assert_eq!(fields["verification_status"], "running");
        assert_eq!(fields["verification_id"], "v_7cd3a6e41039e056");
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        assert_eq!(fields["output_truncated"], false);
        Ok(())
    }

    #[test]
    fn usage_json_approved_replay_does_not_invent_a_new_execution() -> anyhow::Result<()> {
        let fields = captured_usage(
            "BashCommand",
            "verify",
            serde_json::json!({
                "status":"completed", "tool":"BashCommand",
                "data": {
                    "action":"verify", "verification_id":"verification-example",
                    "verification_passed":true, "verification_replayed":true
                }
            }),
        )?;
        assert_eq!(fields["verification_status"], "passed");
        assert_eq!(fields["verification_replayed"], true);
        assert_eq!(fields["verification_id"], "v_7cd3a6e41039e056");
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        assert!(fields.get("output_truncated").is_none_or(serde_json::Value::is_null));
        assert_eq!(fields["execution_id"], "");
        Ok(())
    }

    #[test]
    fn usage_json_keeps_unknown_execution_fields_and_sensitive_values_private() -> anyhow::Result<()>
    {
        let fields = captured_usage(
            "BashCommand",
            "command",
            serde_json::json!({
                "status":"completed", "tool":"BashCommand", "output":"SECRET-OUTPUT",
                "data": {
                    "action":"command", "exit_code":"0", "output_truncated":"false",
                    "execution_id":"SECRET-EXECUTION-ID", "command":"TOKEN=SECRET-COMMAND",
                    "url":"https://private.example/?token=SECRET-URL"
                }
            }),
        )?;
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        assert!(fields.get("output_truncated").is_none_or(serde_json::Value::is_null));
        assert_eq!(fields["execution_id"], "e_b87bc247c7fa135c");
        let json = fields.to_string();
        for sentinel in ["SECRET-OUTPUT", "SECRET-COMMAND", "SECRET-URL", "SECRET-EXECUTION-ID"] {
            assert!(!json.contains(sentinel), "sensitive value was recorded: {sentinel}");
        }
        Ok(())
    }
    #[test]
    fn usage_json_initialize_records_busy_state_without_recording_the_command() -> anyhow::Result<()>
    {
        let fields = captured_usage(
            "Initialize",
            "",
            serde_json::json!({
                "status":"completed", "tool":"Initialize",
                "data": {
                    "initialize_transition":"attached_existing", "initialize_reused":true,
                    "session_status":{"command_running":true, "running_command_id":"SECRET-ACTIVE-COMMAND"}
                }
            }),
        )?;
        assert_eq!(fields["shell_busy"], true);
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        assert!(!fields.to_string().contains("SECRET-ACTIVE-COMMAND"));
        Ok(())
    }

    #[test]
    fn usage_json_blocked_command_does_not_claim_the_blocking_execution() -> anyhow::Result<()> {
        let fields = captured_usage(
            "BashCommand",
            "command",
            serde_json::json!({
                "status":"running", "tool":"BashCommand", "errorCode":"command_already_running",
                "data": {
                    "command_started":false, "shell_busy":true,
                    "running_command":"SECRET-ACTIVE-COMMAND"
                }
            }),
        )?;
        assert_eq!(fields["command_started"], false);
        assert_eq!(fields["shell_busy"], true);
        assert_eq!(fields["execution_id"], "");
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        assert!(!fields.to_string().contains("SECRET-ACTIVE-COMMAND"));
        Ok(())
    }

    #[test]
    fn usage_json_completed_legacy_verification_preserves_explicit_failure() -> anyhow::Result<()> {
        let fields = captured_usage(
            "VerifyEdit",
            "verification",
            serde_json::json!({
                "status":"completed", "tool":"VerifyEdit",
                "data": {
                    "verification_id":"verification-example", "verification_passed":false,
                    "verification":{"status":"completed", "tool":"BashCommand"}
                }
            }),
        )?;
        assert_eq!(fields["verification_status"], "failed");
        assert!(fields.get("exit_code").is_none_or(serde_json::Value::is_null));
        Ok(())
    }

    #[test]
    fn usage_json_missing_nested_execution_does_not_reuse_the_edit_truncation_flag(
    ) -> anyhow::Result<()> {
        let fields = captured_usage(
            "EditFiles",
            "",
            serde_json::json!({
                "status":"completed_with_issues", "tool":"EditFiles",
                "data": {
                    "output_truncated":false, "verification_id":"verification-example",
                    "verification_exit_code":-1,
                    "verification":{"status":"failed", "tool":"BashCommand"}
                }
            }),
        )?;
        assert_eq!(fields["verification_id"], "v_7cd3a6e41039e056");
        assert_eq!(fields["exit_code"], -1);
        assert!(fields.get("output_truncated").is_none_or(serde_json::Value::is_null));
        Ok(())
    }

    #[test]
    fn usage_wait_metadata_preserves_explicit_zero_and_fractional_seconds() {
        let request = serde_json::json!({"wait_policy":"return_early", "wait_for_seconds":0});
        let wait = usage_wait_metadata("BashCommand", request.as_object());
        assert_eq!(wait.policy, Some("return_early"));
        assert_eq!(wait.milliseconds, Some(0));
        let request = serde_json::json!({"verify_wait_for_seconds":0.125});
        let wait = usage_wait_metadata("EditFiles", request.as_object());
        assert_eq!(wait.policy, None);
        assert_eq!(wait.milliseconds, Some(125));
    }

    #[test]
    fn usage_wait_metadata_keeps_absent_and_invalid_values_unknown() {
        for request in [
            serde_json::json!({}),
            serde_json::json!({"wait_policy":"SECRET-POLICY", "wait_for_seconds":-1}),
            serde_json::json!({"wait_policy":false, "wait_for_seconds":"5"}),
            serde_json::json!({"wait_policy":null, "wait_for_seconds":null}),
        ] {
            let wait = usage_wait_metadata("BashCommand", request.as_object());
            assert_eq!(wait.policy, None);
            assert_eq!(wait.milliseconds, None);
        }
    }

    #[test]
    fn workflow_json_repeated_receipt_observations_never_claim_new_delivery() -> anyhow::Result<()>
    {
        let receipt = serde_json::json!({
            "prompt_id":"SECRET-PROMPT-ID", "delivery":"delivered", "activity_observed":false,
            "outcome":"timed_out", "generation":3, "baseline_activity_seq":7
        });
        for _ in 0..2 {
            let fields = captured_usage(
                "Agent",
                "prompt",
                serde_json::json!({
                    "tool":"Agent", "status":"completed_with_issues",
                    "data":{"backend":"native", "prompt_receipt":receipt,
                        "input_sent":false, "output":"SECRET-OUTPUT", "screen":["SECRET-SCREEN"]}
                }),
            )?;
            assert_eq!(fields["workflow_schema"], 1);
            assert_eq!(fields["prompt_receipt_present"], true);
            assert_eq!(fields["prompt_delivery"], "delivered");
            assert_eq!(fields["prompt_activity_observed"], false);
            assert_eq!(fields["prompt_outcome"], "timed_out");
            assert_eq!(fields["agent_input_sent"], false);
            for invented in
                ["receipt_replayed", "wait_satisfied", "prompts_delivered", "tasks_completed"]
            {
                assert!(fields.get(invented).is_none(), "unobserved claim: {invented}");
            }
            for secret in ["SECRET-PROMPT-ID", "SECRET-OUTPUT", "SECRET-SCREEN"] {
                assert!(!fields.to_string().contains(secret), "leaked {secret}");
            }
        }
        Ok(())
    }

    #[test]
    fn workflow_json_keeps_outer_herdr_submission_and_explicit_uncertainty() -> anyhow::Result<()> {
        let fields = captured_usage(
            "Agent",
            "prompt",
            serde_json::json!({
                "tool":"Agent", "status":"completed", "data":{
                    "backend":"herdr", "submission_status":"accepted", "input_sent":true,
                    "result":{"input_sent":false, "submission_status":"SECRET-SUBMISSION",
                        "output":"SECRET-OUTPUT", "target":"SECRET-TARGET"}
                }
            }),
        )?;
        assert_eq!(fields["agent_submission_status"], "accepted");
        assert_eq!(fields["agent_input_sent"], true);
        assert!(fields.get("prompt_receipt_present").is_none_or(serde_json::Value::is_null));
        assert!(fields.get("prompt_outcome").is_none_or(serde_json::Value::is_null));
        assert!(!fields.to_string().contains("SECRET-"));
        let uncertain = captured_usage(
            "Agent",
            "prompt",
            serde_json::json!({
                "tool":"Agent", "status":"completed_with_issues", "errorCode":"herdr_delivery_uncertain",
                "message":"SECRET-ERROR", "data":{"backend":"herdr", "input_may_have_been_sent":true}
            }),
        )?;
        assert_eq!(uncertain["agent_input_may_have_been_sent"], true);
        assert!(uncertain.get("agent_input_sent").is_none_or(serde_json::Value::is_null));
        assert!(uncertain.get("agent_submission_status").is_none_or(serde_json::Value::is_null));
        assert!(!uncertain.to_string().contains("SECRET-ERROR"));
        Ok(())
    }

    #[test]
    fn workflow_json_catalog_outcomes_require_observed_evidence() -> anyhow::Result<()> {
        for (data, expected) in [
            (
                serde_json::json!({"timed_out":false,"changes":{"resync_required":true,"executions":[]}}),
                Some("resync_required"),
            ),
            (
                serde_json::json!({"timed_out":true,"changes":{"resync_required":false,"executions":[]}}),
                Some("timed_out"),
            ),
            (
                serde_json::json!({"timed_out":false,"changes":{"resync_required":false,"executions":[{"execution_id":"SECRET-ID"}]}}),
                Some("changed"),
            ),
            (serde_json::json!({"changes":{"executions":[{"execution_id":"SECRET-ID"}]}}), None),
            (
                serde_json::json!({"timed_out":false,"changes":{"resync_required":false,"executions":[]}}),
                None,
            ),
        ] {
            let fields = captured_usage(
                "Execution",
                "wait",
                serde_json::json!({
                    "tool":"Execution", "status":"completed", "data":data
                }),
            )?;
            assert_eq!(fields["workflow_wait_outcome"].as_str(), expected);
            assert!(!fields.to_string().contains("SECRET-ID"));
        }
        Ok(())
    }

    #[test]
    fn workflow_json_unknown_receipt_fields_stay_unknown_and_private() -> anyhow::Result<()> {
        let fields = captured_usage(
            "Agent",
            "prompt",
            serde_json::json!({
                "tool":"Agent", "status":"completed", "data":{
                    "backend":"native", "prompt_receipt":{
                        "prompt_id":"SECRET-PROMPT-ID", "delivery":"SECRET-DELIVERY",
                        "activity_observed":"false", "outcome":"SECRET-OUTCOME"
                    }, "input_sent":"false", "input_may_have_been_sent":"false",
                    "text":"SECRET-TEXT", "command":"SECRET-COMMAND", "cursor":"SECRET-CURSOR",
                    "target":"SECRET-TARGET", "recognizer":"SECRET-RECOGNIZER", "screen":["SECRET-SCREEN"]
                }
            }),
        )?;
        assert_eq!(fields["workflow_schema"], 1);
        for key in [
            "prompt_delivery",
            "prompt_activity_observed",
            "prompt_outcome",
            "agent_input_sent",
            "agent_input_may_have_been_sent",
        ] {
            assert!(fields.get(key).is_none_or(serde_json::Value::is_null), "invented {key}");
        }
        assert!(!fields.to_string().contains("SECRET-"));
        let absent = captured_usage(
            "Agent",
            "prompt",
            serde_json::json!({
                "tool":"Agent", "status":"failed", "data":{"backend":"native"}
            }),
        )?;
        assert_eq!(absent["prompt_receipt_present"], false);
        assert!(absent.get("prompt_activity_observed").is_none_or(serde_json::Value::is_null));
        Ok(())
    }

    #[test]
    fn workflow_requested_timeouts_preserve_typed_zero_fraction_and_finite_bounds() {
        for (tool, request, expected) in [
            (
                "Execution",
                serde_json::json!({"action":{"type":"wait","timeout_seconds":0}}),
                Some(0),
            ),
            (
                "Execution",
                serde_json::json!({"action":{"type":"wait","timeout_seconds":0.125}}),
                Some(125),
            ),
            ("Agent", serde_json::json!({"action":"prompt","timeout_seconds":0}), Some(0)),
            ("Agent", serde_json::json!({"action":"wait","timeout_seconds":30}), Some(30_000)),
            ("Agent", serde_json::json!({"action":"wait","timeout_seconds":30.001}), None),
            ("Agent", serde_json::json!({"action":"wait","timeout_seconds":-1}), None),
            ("Agent", serde_json::json!({"action":"wait","timeout_seconds":"5"}), None),
            ("Agent", serde_json::json!({"action":"wait"}), None),
            ("Agent", serde_json::json!({"action":"read","timeout_seconds":5}), None),
            (
                "Agent",
                serde_json::json!({"backend":"SECRET-BACKEND","action":"wait","timeout_seconds":5}),
                None,
            ),
        ] {
            let wait = usage_wait_metadata(tool, request.as_object());
            assert_eq!(wait.milliseconds, expected, "{tool} {request}");
        }
    }
    #[test]
    fn workflow_json_classifies_actual_request_shapes_with_finite_allowlists() -> anyhow::Result<()>
    {
        for (tool, arguments, action, backend, kind, wait) in [
            (
                "Execution",
                serde_json::json!({"action":{"type":"wait","timeout_seconds":0}}),
                "wait",
                "native",
                Some("catalog"),
                Some(0),
            ),
            (
                "Agent",
                serde_json::json!({"action":"wait","timeout_seconds":0.125}),
                "wait",
                "native",
                Some("catalog"),
                Some(125),
            ),
            (
                "Agent",
                serde_json::json!({"backend":"native","action":"wait","prompt_id":"SECRET-PROMPT"}),
                "wait",
                "native",
                Some("receipt"),
                None,
            ),
            (
                "Agent",
                serde_json::json!({"backend":"herdr","action":"wait","timeout_seconds":0.25}),
                "wait",
                "herdr",
                Some("herdr"),
                Some(250),
            ),
            (
                "Agent",
                serde_json::json!({"backend":"herdr","action":"prompt","timeout_seconds":5}),
                "prompt",
                "herdr",
                None,
                None,
            ),
            (
                "Agent",
                serde_json::json!({"backend":"SECRET-BACKEND","action":"SECRET-ACTION","timeout_seconds":5}),
                "unknown",
                "unknown",
                None,
                None,
            ),
            (
                "Agent",
                serde_json::json!({"backend":null,"action":"wait","timeout_seconds":5}),
                "wait",
                "unknown",
                None,
                None,
            ),
            (
                "Execution",
                serde_json::json!({"action":{"type":"SECRET-ACTION","timeout_seconds":5}}),
                "unknown",
                "native",
                None,
                None,
            ),
        ] {
            let fields = captured_usage_request(
                tool,
                "",
                &arguments,
                Some(serde_json::json!({
                    "tool":tool,"status":"completed","data":{}
                })),
            )?;
            assert_eq!(fields["workflow_schema"], 1);
            assert_eq!(fields["action"], action);
            assert_eq!(fields["workflow_backend"], backend);
            assert_eq!(fields["workflow_wait_kind"].as_str(), kind);
            assert_eq!(fields["requested_wait_ms"].as_u64(), wait);
            assert!(fields.get("workflow_wait_outcome").is_none_or(serde_json::Value::is_null));
            assert!(!fields.to_string().contains("SECRET-"));
        }
        Ok(())
    }

    #[test]
    fn workflow_json_protocol_error_never_invents_a_missing_receipt_response() -> anyhow::Result<()>
    {
        let fields = captured_usage_request(
            "Agent",
            "",
            &serde_json::json!({
                "action":"wait","prompt_id":"SECRET-PROMPT","timeout_seconds":0
            }),
            None,
        )?;
        assert_eq!(fields["workflow_wait_kind"], "receipt");
        assert_eq!(fields["requested_wait_ms"], 0);
        for key in [
            "prompt_receipt_present",
            "prompt_delivery",
            "prompt_activity_observed",
            "prompt_outcome",
            "agent_input_sent",
            "agent_input_may_have_been_sent",
        ] {
            assert!(fields.get(key).is_none_or(serde_json::Value::is_null), "invented {key}");
        }
        Ok(())
    }

    #[test]
    fn workflow_json_observes_typed_receipts_in_real_workflow_envelopes() -> anyhow::Result<()> {
        use crate::runtime::workflow::{PromptDelivery, PromptOutcome, PromptReceipt};
        use crate::server::{executions::workflow_envelope, outcomes::ToolResultStatus};
        for (delivery, outcome, expected) in [
            (PromptDelivery::Delivered, PromptOutcome::Waiting, "waiting"),
            (PromptDelivery::Delivered, PromptOutcome::AwaitingInput, "awaiting_input"),
            (PromptDelivery::Delivered, PromptOutcome::AwaitingApproval, "awaiting_approval"),
            (PromptDelivery::Delivered, PromptOutcome::Exited, "exited"),
            (PromptDelivery::Uncertain, PromptOutcome::TimedOut, "timed_out"),
            (PromptDelivery::Uncertain, PromptOutcome::GenerationChanged, "generation_changed"),
        ] {
            let receipt = PromptReceipt {
                prompt_id: "SECRET-PROMPT-ID".into(),
                delivery,
                activity_observed: false,
                outcome,
                generation: 4,
                baseline_activity_seq: 9,
            };
            let response = workflow_envelope(
                "Agent",
                ToolResultStatus::CompletedWithIssues,
                "SECRET-MESSAGE",
                None,
                Some("SECRET-OUTPUT".into()),
                serde_json::json!({
                    "prompt_receipt":receipt,"input_sent":false
                }),
            )?;
            let fields = captured_usage_request(
                "Agent",
                "",
                &serde_json::json!({
                    "action":"wait","prompt_id":"SECRET-PROMPT-ID"
                }),
                response.structured_content,
            )?;
            assert_eq!(fields["prompt_receipt_present"], true);
            assert_eq!(fields["prompt_outcome"], expected);
            assert_eq!(fields["prompt_activity_observed"], false);
            assert_eq!(fields["agent_input_sent"], false);
            assert_eq!(
                fields["prompt_delivery"],
                if delivery == PromptDelivery::Delivered { "delivered" } else { "uncertain" }
            );
            assert!(fields.get("workflow_wait_outcome").is_none_or(serde_json::Value::is_null));
            assert!(!fields.to_string().contains("SECRET-"));
        }
        Ok(())
    }

    #[test]
    fn workflow_json_does_not_mine_incidental_receipts_from_get_or_list() -> anyhow::Result<()> {
        for (tool, arguments) in [
            ("Execution", serde_json::json!({"action":{"type":"get"}})),
            ("Execution", serde_json::json!({"action":{"type":"list"}})),
            ("Agent", serde_json::json!({"action":"get"})),
            ("Agent", serde_json::json!({"action":"read"})),
        ] {
            let fields = captured_usage_request(
                tool,
                "",
                &arguments,
                Some(serde_json::json!({
                    "tool":tool,"status":"completed","data":{"prompt_receipt":{
                        "prompt_id":"SECRET-PROMPT-ID","delivery":"delivered","activity_observed":true,
                        "outcome":"exited","generation":1,"baseline_activity_seq":0
                    }}
                })),
            )?;
            for key in [
                "prompt_receipt_present",
                "prompt_delivery",
                "prompt_activity_observed",
                "prompt_outcome",
            ] {
                assert!(fields.get(key).is_none_or(serde_json::Value::is_null), "incidental {key}");
            }
            assert!(!fields.to_string().contains("SECRET-PROMPT-ID"));
        }
        Ok(())
    }

    #[test]
    fn workflow_requested_timeout_uses_f32_precision_and_rejects_overflow() {
        for (seconds, expected) in [
            (serde_json::json!(1.999_999_99), Some(2000)),
            (serde_json::json!(1e100), None),
            (serde_json::json!(30.1), None),
        ] {
            let request = serde_json::json!({"action":"wait","timeout_seconds":seconds});
            assert_eq!(usage_wait_metadata("Agent", request.as_object()).milliseconds, expected);
        }
    }
    #[test]
    fn lifecycle_terminal_keeps_one_call_and_all_new_events_exclude_request_contents(
    ) -> anyhow::Result<()> {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = bytes.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || UsageCapture(writer.clone()))
            .finish();
        let arguments = serde_json::json!({"action":"prompt","text":"SECRET-TEXT",
            "command":"SECRET-COMMAND","output":"SECRET-OUTPUT","screen":["SECRET-SCREEN"],
            "cursor":"SECRET-CURSOR","target":"SECRET-TARGET","recognizer":"SECRET-RECOGNIZER"});
        let mut usage = fixture_usage("Agent", "", &arguments);
        tracing::subscriber::with_default(subscriber, || {
            usage.trace = UsageTrace::start(&usage);
            usage.trace.phase(Phase::Catalog).finish(true);
            usage.emit("protocol_error", "failed", 0, None);
        });
        let captured = bytes.lock().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let events = serde_json::Deserializer::from_slice(&captured)
            .into_iter::<serde_json::Value>()
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            events.iter().filter(|event| event["fields"]["event"] == "tool_call").count(),
            1
        );
        assert_eq!(
            events.iter().filter(|event| event["fields"]["event"] == "tool_started").count(),
            1
        );
        let terminal = &events.last().ok_or_else(|| anyhow::anyhow!("terminal"))?["fields"];
        assert_eq!(terminal["phase_catalog_calls"], 1);
        assert!(terminal["phase_catalog_ms"].as_u64().is_some());
        assert!(terminal.get("phase_wait_idle_ms").is_none_or(serde_json::Value::is_null));
        assert_eq!(terminal["phase_abandoned_count"], 0);
        assert_eq!(terminal["phase_trace_suppressed_count"], 0);
        for event in &events {
            assert_eq!(event["fields"]["usage_call_id"], terminal["usage_call_id"]);
            assert_eq!(event["fields"]["lifecycle_schema"], 1);
            assert!(!event.to_string().contains("SECRET-"));
        }
        Ok(())
    }
}
