use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::mutations::{MutationStart, VerificationOwner, VerificationStart};
use super::sessions::SessionGuard;
use super::{outcomes, SharedBashState, WinxService};
use crate::errors::WinxError;
use crate::runtime::{ShellActionOptions, ShellExecutionToken};
use crate::state::bash_state::generate_thread_id;
use crate::tool_policy::EditPermissionSet;
use crate::tool_registry::ToolKind;
use crate::tools::edit_files::{EditSurface, EditVerification, PreparedEditContext};
use crate::types::{
    normalize_thread_id, BashCommand, CodeMap, ContextSave, Initialize, ReadFiles, ReadImage,
    VerifyEdit,
};

const MAX_VERIFY_WAIT_SECONDS: f32 = 60.0;
const READ_EDIT_GUIDANCE: &str =
    "Edit guidance: for an existing file above, prefer EditFiles mode=line_patch with its \
     structuredContent.data.files[].revision and visibleRanges. Use BashCommand for commands, not \
     ordinary source mutation.";

pub(super) struct ToolCallExecution {
    pub result: CallToolResult,
    pub command_generation: Option<u64>,
    pub execution_token: Option<ShellExecutionToken>,
    pub generation_bound_actions: bool,
}

impl ToolCallExecution {
    fn legacy(result: CallToolResult) -> Self {
        Self {
            result,
            command_generation: None,
            execution_token: None,
            generation_bound_actions: false,
        }
    }
}

fn is_expected_recovery_status(status: &str) -> bool {
    matches!(
        status,
        "needs_read"
            | "needs_initialize"
            | "not_found"
            | "invalid_input"
            | "conflict"
            | "recovery_exhausted"
    )
}

fn log_tool_result(
    tool: &str,
    elapsed_ms: u128,
    summary: &str,
    result: &Result<CallToolResult, McpError>,
) {
    match result {
        Ok(call)
            if call.is_error == Some(true)
                && is_expected_recovery_status(&outcomes::result_status(call)) =>
        {
            info!(
                tool = %tool,
                ms = elapsed_ms,
                status = outcomes::result_status(call),
                response_bytes = outcomes::result_size_bytes(call),
                "tool call needs recovery — {summary}"
            );
        }
        Ok(call) if call.is_error == Some(true) => warn!(
            tool = %tool,
            ms = elapsed_ms,
            status = outcomes::result_status(call),
            response_bytes = outcomes::result_size_bytes(call),
            "tool call failed — {summary}"
        ),
        Ok(call) if outcomes::result_status(call) == "completed_with_issues" => info!(
            tool = %tool,
            ms = elapsed_ms,
            status = outcomes::result_status(call),
            response_bytes = outcomes::result_size_bytes(call),
            "tool call completed with follow-up — {summary}"
        ),
        Ok(call) => info!(
            tool = %tool,
            ms = elapsed_ms,
            status = outcomes::result_status(call),
            response_bytes = outcomes::result_size_bytes(call),
            "tool call ok — {summary}"
        ),
        Err(error) => warn!(
            tool = %tool,
            ms = elapsed_ms,
            "tool call protocol error — {summary}: {}",
            error.message
        ),
    }
}

pub(super) struct BashCallExecution {
    result: CallToolResult,
    command_generation: Option<u64>,
    execution_token: Option<ShellExecutionToken>,
    generation_bound_actions: bool,
}

impl BashCallExecution {
    fn legacy(result: CallToolResult) -> Self {
        Self {
            result,
            command_generation: None,
            execution_token: None,
            generation_bound_actions: false,
        }
    }
}

type BashRuntimeMetadata = (Option<u64>, Option<ShellExecutionToken>, bool);
struct PreparedEditCall {
    context: PreparedEditContext,
    slot: SharedBashState,
    /// Kept for the complete mutation lifecycle. In particular, the session
    /// cannot be evicted between preflight and commit/receipt/verification.
    _session_guard: SessionGuard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedTool {
    Public(ToolKind),
}

impl RequestedTool {
    fn parse(name: &str) -> Option<Self> {
        ToolKind::parse(name).map(Self::Public)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Public(kind) => kind.as_str(),
        }
    }

    const fn edit_surface(self) -> Option<EditSurface> {
        match self {
            Self::Public(kind) => EditSurface::from_public_tool(kind),
        }
    }
}

type EditPreparation = std::result::Result<Option<PreparedEditCall>, CallToolResult>;
enum ReceiptBoundPreparation {
    Ordinary,
    Execute(VerificationOwner),
    Replay(BashCallExecution),
}

type ReceiptBoundPreparationResult = std::result::Result<ReceiptBoundPreparation, CallToolResult>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBoundVerifyAction {
    #[serde(rename = "type")]
    action_type: String,
    verification_id: String,
    command: String,
}

fn normalize_receipt_bound_verify_action(
    arguments: &Value,
) -> crate::errors::Result<Option<ReceiptBoundVerifyAction>> {
    let Some(action) = arguments.get("action_json") else { return Ok(None) };
    if action.get("type").and_then(Value::as_str) != Some("verify") {
        return Ok(None);
    }
    let action: ReceiptBoundVerifyAction =
        serde_json::from_value(action.clone()).map_err(|error| {
            WinxError::InvalidInput(format!("Invalid receipt-bound verification action: {error}"))
        })?;
    if action.action_type != "verify" {
        return Err(WinxError::InvalidInput(
            "receipt-bound verification action type must be verify".to_string(),
        ));
    }
    if action.verification_id.trim().is_empty() {
        return Err(WinxError::InvalidInput("verification_id must not be empty".to_string()));
    }
    if action.command.trim().is_empty() {
        return Err(WinxError::InvalidInput("verification command must not be empty".to_string()));
    }
    Ok(Some(action))
}

impl WinxService {
    async fn prepare_edit_context(
        &self,
        requested_tool: RequestedTool,
        args_value: Option<&Value>,
        edit_permissions: EditPermissionSet,
    ) -> Result<EditPreparation, McpError> {
        let Some(surface) = requested_tool.edit_surface() else {
            return Ok(Ok(None));
        };
        let arguments = args_value
            .cloned()
            .ok_or_else(|| McpError::invalid_request("Missing edit arguments", None))?;
        let normalized = match crate::tools::edit_files::normalize_edit_call(surface, arguments) {
            Ok(normalized) => normalized,
            Err(error) => {
                return Ok(Err(outcomes::tool_failure(requested_tool.name(), &error, args_value)?));
            }
        };
        let thread_id = normalize_thread_id(&normalized.thread_id);
        let (slot, session_guard) = self.tool_session_for(&thread_id).await;
        if let Some(verification) = normalized.verification.as_ref() {
            if let Err(error) = Self::validate_edit_verification(&slot, verification).await {
                return Ok(Err(outcomes::tool_failure(requested_tool.name(), &error, args_value)?));
            }
        }
        let state = slot.lock().await;
        let Some(state) = state.as_ref() else {
            return Ok(Err(outcomes::tool_failure(
                requested_tool.name(),
                &WinxError::BashStateNotInitialized,
                args_value,
            )?));
        };
        Ok(match PreparedEditContext::prepare(normalized, state, edit_permissions) {
            Ok(context) => Ok(Some(PreparedEditCall {
                context,
                slot: slot.clone(),
                _session_guard: session_guard,
            })),
            Err(error) => Err(outcomes::tool_failure(requested_tool.name(), &error, args_value)?),
        })
    }

    pub(super) async fn execute_tool_call(
        &self,
        param: CallToolRequestParams,
        bash_options: ShellActionOptions,
        edit_permissions: EditPermissionSet,
    ) -> Result<ToolCallExecution, McpError> {
        let requested_tool = param.name.to_string();
        let requested_tool = RequestedTool::parse(&requested_tool).ok_or_else(|| {
            McpError::invalid_request(format!("Unknown tool: {requested_tool}"), None)
        })?;
        let tool = requested_tool.name();
        let args_value = param.arguments.map(Value::Object);
        let orchestration_args = args_value.clone();
        let prepared_edit = match self
            .prepare_edit_context(requested_tool, args_value.as_ref(), edit_permissions)
            .await?
        {
            Ok(prepared) => prepared,
            Err(result) => return Ok(ToolCallExecution::legacy(result)),
        };
        let raw_summary = prepared_edit.as_ref().map_or_else(
            || audit_summary(tool, args_value.as_ref()),
            |prepared| prepared.context.audit_summary(),
        );
        let summary = crate::utils::redact::redact(&raw_summary).into_owned();
        let started = std::time::Instant::now();

        let mutation_start = self
            .begin_edit_mutation(prepared_edit.as_ref().map(|prepared| &prepared.context))
            .await?;
        let mut mutation_owner = match mutation_start {
            MutationStart::Bypass => None,
            MutationStart::Owner(owner) => Some(owner),
            MutationStart::Replay(mut result) => {
                redact_result(&mut result);
                info!(
                    tool = %tool,
                    ms = started.elapsed().as_millis(),
                    status = outcomes::result_status(&result),
                    response_bytes = outcomes::result_size_bytes(&result),
                    "tool call replayed from mutation receipt — {summary}"
                );
                return Ok(ToolCallExecution::legacy(result));
            }
        };

        let (result, bash_runtime) = self
            .dispatch_tool(requested_tool, args_value, prepared_edit.as_ref(), bash_options)
            .await;

        let result = match result {
            Ok(mut call) => {
                outcomes::decorate_success(tool, orchestration_args.as_ref(), &mut call);
                if let Some(prepared) = prepared_edit.as_ref() {
                    outcomes::attach_prepared_edit_context(&mut call, &prepared.context);
                }
                self.apply_edit_recovery_budget(
                    prepared_edit.as_ref().map(|prepared| &prepared.context),
                    &mut call,
                );
                if let Some(owner) = mutation_owner.take() {
                    self.finish_edit_mutation(owner, &mut call).await;
                }
                redact_result(&mut call);
                Ok(call)
            }
            Err(mut error) => {
                error.message = crate::utils::redact::redact(&error.message).into_owned().into();
                Err(error)
            }
        };

        log_tool_result(tool, started.elapsed().as_millis(), &summary, &result);
        result.map(|result| {
            let Some((command_generation, execution_token, generation_bound_actions)) =
                bash_runtime
            else {
                return ToolCallExecution::legacy(result);
            };
            ToolCallExecution {
                result,
                command_generation,
                execution_token,
                generation_bound_actions,
            }
        })
    }

    async fn dispatch_tool(
        &self,
        tool: RequestedTool,
        args: Option<Value>,
        prepared_edit: Option<&PreparedEditCall>,
        bash_options: ShellActionOptions,
    ) -> (Result<CallToolResult, McpError>, Option<BashRuntimeMetadata>) {
        match tool {
            RequestedTool::Public(ToolKind::Initialize) => {
                (self.handle_initialize(args).await, None)
            }
            RequestedTool::Public(ToolKind::BashCommand) => {
                match self.handle_bash_command_with_output(args, bash_options.clone()).await {
                    Ok(execution) => (
                        Ok(execution.result),
                        Some((
                            execution.command_generation,
                            execution.execution_token,
                            execution.generation_bound_actions,
                        )),
                    ),
                    Err(error) => (Err(error), None),
                }
            }
            RequestedTool::Public(
                ToolKind::FileWriteOrEdit
                | ToolKind::MultiFileEdit
                | ToolKind::UndoEdit
                | ToolKind::ApplyPatch
                | ToolKind::EditFiles,
            ) => (self.handle_prepared_edit(prepared_edit, bash_options).await, None),
            RequestedTool::Public(ToolKind::ReadFiles) => {
                (self.handle_read_files(args).await, None)
            }
            RequestedTool::Public(ToolKind::VerifyEdit) => {
                (self.handle_verify_edit(args, bash_options).await, None)
            }
            RequestedTool::Public(ToolKind::ContextSave) => {
                (self.handle_context_save(args).await, None)
            }
            RequestedTool::Public(ToolKind::ReadImage) => {
                (self.handle_read_image(args).await, None)
            }
            RequestedTool::Public(ToolKind::CodeMap) => (self.handle_code_map(args).await, None),
        }
    }

    pub(super) async fn knowledge_transfer_prompt_text(
        &self,
        session_prefix: Option<&str>,
    ) -> String {
        let mut text = String::from(
            "Prepare a concise handoff for another agent. Include active objective, current state, important files, changed files, blockers, validation already run, and exact next commands.\n",
        );

        let state_snapshot = if let Some(slot) = self.active_slot(session_prefix).await {
            let guard = slot.lock().await;
            guard.as_ref().map(|state| {
                let whitelist = state
                    .whitelist_for_overwrite
                    .iter()
                    .take(12)
                    .map(|(path, data)| {
                        format!(
                            "- {} ({:.1}% read, {} lines)",
                            path,
                            data.get_percentage_read(),
                            data.total_lines
                        )
                    })
                    .collect::<Vec<_>>();
                (
                    state.current_thread_id.clone(),
                    state.workspace_root.clone(),
                    state.cwd.clone(),
                    state.mode.to_string(),
                    whitelist,
                    state.whitelist_for_overwrite.len(),
                )
            })
        } else {
            None
        };

        let Some((thread_id, workspace_root, cwd, mode, whitelist, whitelist_count)) =
            state_snapshot
        else {
            text.push_str("\n# Current Winx state\nWinx is not initialized.\n");
            return text;
        };

        let display_thread_id =
            session_prefix.and_then(|prefix| thread_id.strip_prefix(prefix)).unwrap_or(&thread_id);
        let _ = writeln!(
            text,
            "\n# Current Winx state\nThread: {display_thread_id}\nWorkspace: {}\nCwd: {}\nMode: {mode}\nWhitelisted files: {whitelist_count}",
            workspace_root.display(),
            cwd.display()
        );

        if !whitelist.is_empty() {
            text.push_str("\n# Recently readable files\n");
            text.push_str(&whitelist.join("\n"));
            text.push('\n');
        }

        let active_files = crate::utils::workspace_stats::active_files(&workspace_root);
        if !active_files.is_empty() {
            text.push_str("\n# Active files by Winx usage\n");
            for file in active_files.iter().take(12) {
                let _ = writeln!(text, "- {file}");
            }
        }

        if let Ok((repo_context, _)) = crate::utils::repo::get_repo_context(&workspace_root) {
            let repo_excerpt = repo_context.lines().take(80).collect::<Vec<_>>().join("\n");
            let _ = writeln!(text, "\n# Workspace context\n{repo_excerpt}");
        }

        append_command_section(&mut text, "Git status", &workspace_root, ["status", "--short"]);
        append_command_section(
            &mut text,
            "Git diff stat",
            &workspace_root,
            ["diff", "--stat", "HEAD"],
        );

        let sections = if mode == "architect" {
            "\n# Sections for the ContextSave description (architect mode)\n\
             - `# Objective` — project and task objective.\n\
             - `# All user instructions` — everything the user asked, verbatim.\n\
             - `# Designed plan` — the plan you designed, in detail.\n\
             - Provide all relevant file paths so the next agent can resume; err toward more.\n"
        } else {
            "\n# Sections for the ContextSave description\n\
             - `# Objective` — project and task objective.\n\
             - `# All user instructions` — everything the user asked, verbatim.\n\
             - `# Current status` — what's already done (not what's left).\n\
             - `# Pending issues with snippets` — verbatim errors/tracebacks/commands; be verbose.\n\
             - `# Build and development instructions` — how to build/run/test; leave empty if unknown.\n\
             - Provide all relevant file paths so the next agent can resume; err toward more.\n"
        };
        text.push_str(sections);
        text.push_str(
            "\n# Handoff checklist\n- State what changed and why.\n- Include files touched and any user-owned dirty work to preserve.\n- Include validation commands already run and their result.\n- Include the next safest command to continue.\n",
        );
        text
    }

    async fn persist_state(&self, slot: &SharedBashState) {
        let guard = slot.lock().await;
        if let Some(state) = guard.as_ref() {
            if let Err(error) = state.save_state_to_disk() {
                warn!("Failed to persist bash state: {}", error);
            }
        }
    }

    /// Deserialize `args` into `T`, retrying once after JSON-decoding any string
    /// field that is itself an encoded object/array.
    fn lenient_from_value<T: serde::de::DeserializeOwned>(
        args: Value,
    ) -> Result<T, serde_json::Error> {
        match serde_json::from_value::<T>(args.clone()) {
            Ok(value) => Ok(value),
            Err(first_error) => {
                let Value::Object(mut map) = args else {
                    return Err(first_error);
                };
                let mut changed = false;
                for value in map.values_mut() {
                    if let Value::String(text) = value {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            if parsed.is_object() || parsed.is_array() {
                                *value = parsed;
                                changed = true;
                            }
                        }
                    }
                }
                if changed {
                    serde_json::from_value::<T>(Value::Object(map))
                } else {
                    Err(first_error)
                }
            }
        }
    }

    pub(super) async fn handle_initialize(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let mut initialize: Initialize = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid Initialize parameters: {error}"), None)
        })?;

        let mut thread_id = normalize_thread_id(&initialize.thread_id);
        if thread_id.is_empty() {
            thread_id = generate_thread_id();
            initialize.thread_id.clone_from(&thread_id);
        }
        let (slot, _session_guard) = self.session_for(&thread_id).await;

        match crate::tools::initialize::handle_tool_call_with_runtime_detailed(
            self.shell_runtime.as_ref(),
            &slot,
            initialize,
        )
        .await
        {
            Ok(outcome) => {
                self.persist_state(&slot).await;
                let workspace_root = slot
                    .lock()
                    .await
                    .as_ref()
                    .map(|state| state.workspace_root.to_string_lossy().into_owned())
                    .ok_or_else(|| {
                        McpError::internal_error(
                            "Initialize completed without a bound workspace",
                            None,
                        )
                    })?;
                let mut result = CallToolResult::success(vec![ContentBlock::text(outcome.text)]);
                result.structured_content = Some(json!({
                    "workspace_root": workspace_root,
                    "initialize_transition": outcome.transition.as_str(),
                    "initialize_reused": outcome.transition.reused(),
                    "initialize_recovered_missing_session": outcome.recovered_missing_session,
                    "initialize_response_mode": if outcome.compact_response {
                        "compact"
                    } else {
                        "full"
                    },
                    "context_bytes": outcome.context_bytes,
                    "guidelines_bytes": outcome.guidelines_bytes,
                    "initial_files_count": outcome.initial_files_count,
                    "instructions_unchanged": outcome.compact_response,
                    "code_writer_policy_strength": outcome.code_writer_policy_strength,
                    "shell_spawners_present": outcome.shell_spawners_present,
                    "shell_reset_performed": outcome.transition
                        == crate::tools::initialize::InitializeTransition::ShellReset,
                    "shell_reset_retry_after_seconds": outcome.shell_reset_retry_after_seconds,
                    "temporary_artifact_dir": outcome.temporary_artifact_dir,
                    "temporary_artifact_env": "WINX_TEMP_DIR",
                    "temporary_artifact_ttl_seconds": outcome.temporary_artifact_ttl_seconds,
                    "temporary_artifact_max_bytes": outcome.temporary_artifact_max_bytes,
                    "temporary_artifact_max_session_bytes": outcome.temporary_artifact_max_session_bytes,
                    "temporary_artifact_max_file_bytes": outcome.temporary_artifact_max_file_bytes,
                    "temporary_artifact_max_files": outcome.temporary_artifact_max_files,
                    "temporary_artifact_stale_pruned_files": outcome.temporary_artifact_stale_pruned_files,
                    "temporary_artifact_stale_pruned_bytes": outcome.temporary_artifact_stale_pruned_bytes,
                    "temporary_code_map_max_calls": outcome.temporary_code_map_max_calls,
                    "temporary_code_map_max_unique_files": outcome.temporary_code_map_max_unique_files,
                }));
                Ok(result)
            }
            Err(error) => outcomes::tool_failure("Initialize", &error, Some(&recovery_args)),
        }
    }

    #[cfg(test)]
    pub(super) async fn handle_bash_command(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        Ok(self.handle_bash_command_with_output(args, ShellActionOptions::default()).await?.result)
    }

    async fn prepare_receipt_bound_bash_action(
        &self,
        bash_command: &mut BashCommand,
        verify_action: Option<ReceiptBoundVerifyAction>,
        recovery_args: &Value,
        options: &mut ShellActionOptions,
    ) -> Result<ReceiptBoundPreparationResult, McpError> {
        let Some(verify_action) = verify_action else {
            return Ok(Ok(ReceiptBoundPreparation::Ordinary));
        };
        let verification_id = verify_action.verification_id;
        let command = verify_action.command.trim().to_string();
        let thread_id = normalize_thread_id(&bash_command.thread_id);
        let (slot, _guard) = self.tool_session_for(&thread_id).await;
        let verification = EditVerification {
            command: command.clone(),
            wait_for_seconds: bash_command.wait_for_seconds,
        };
        if let Err(error) = Self::validate_edit_verification(&slot, &verification).await {
            return Ok(Err(outcomes::tool_failure("BashCommand", &error, Some(recovery_args))?));
        }
        let start = match self
            .begin_receipt_bound_verification(
                &thread_id,
                &verification_id,
                &command,
                bash_command.wait_for_seconds,
            )
            .await
        {
            Ok(start) => start,
            Err(error) => {
                return Ok(Err(outcomes::tool_failure(
                    "BashCommand",
                    &error,
                    Some(recovery_args),
                )?));
            }
        };
        match start {
            VerificationStart::Execute(owner) => {
                bash_command.action_json = crate::types::BashCommandAction::Command {
                    command,
                    is_background: false,
                    allow_multi: false,
                };
                Ok(Ok(ReceiptBoundPreparation::Execute(owner)))
            }
            VerificationStart::Poll(owner, binding) => {
                bash_command.action_json = crate::types::BashCommandAction::StatusCheck {
                    status_check: true,
                    bg_command_id: None,
                    scrollback_lines: None,
                    verbose: false,
                };
                if let Some(binding) = binding {
                    options.expected_generation = Some(binding.generation);
                    if !binding.guardian_epoch.is_empty() && !binding.session_epoch.is_empty() {
                        options.expected_execution = Some(crate::runtime::ShellExecutionToken {
                            guardian_epoch: binding.guardian_epoch,
                            session_epoch: binding.session_epoch,
                            generation: binding.generation,
                        });
                    }
                }
                Ok(Ok(ReceiptBoundPreparation::Execute(owner)))
            }
            VerificationStart::Replay(result) => {
                Ok(Ok(ReceiptBoundPreparation::Replay(BashCallExecution::legacy(result))))
            }
        }
    }

    #[allow(clippy::too_many_lines)] // validation, receipt binding, execution, and audit are one lifecycle
    pub(super) async fn handle_bash_command_with_output(
        &self,
        args: Option<Value>,
        mut options: ShellActionOptions,
    ) -> Result<BashCallExecution, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let verify_action = match normalize_receipt_bound_verify_action(&args) {
            Ok(action) => action,
            Err(error) => {
                return Ok(BashCallExecution::legacy(outcomes::tool_failure(
                    "BashCommand",
                    &error,
                    Some(&recovery_args),
                )?));
            }
        };
        let mut runtime_args = args;
        if let Some(action) = verify_action.as_ref() {
            runtime_args["action_json"] = json!({
                "type": "command",
                "command": action.command,
                "is_background": false,
                "allow_multi": false
            });
        }
        let mut bash_command: BashCommand = serde_json::from_value(runtime_args).map_err(|error| {
            McpError::invalid_request(
                format!(
                    "Invalid BashCommand parameters: {error}. Accepted forms include {{\"action_json\": {{\"command\": \"pwd\"}}}}, {{\"command\": \"pwd\"}}, or {{\"action_json\": {{\"type\": \"status_check\", \"status_check\": true}}}}."
                ),
                None,
            )
        })?;

        let receipt_bound_verification = match self
            .prepare_receipt_bound_bash_action(
                &mut bash_command,
                verify_action,
                &recovery_args,
                &mut options,
            )
            .await?
        {
            Ok(ReceiptBoundPreparation::Ordinary) => None,
            Ok(ReceiptBoundPreparation::Execute(owner)) => Some(owner),
            Ok(ReceiptBoundPreparation::Replay(execution)) => return Ok(execution),
            Err(result) => return Ok(BashCallExecution::legacy(result)),
        };

        let requested_thread_id = normalize_thread_id(&bash_command.thread_id);
        let (slot, _session_guard) = self.tool_session_for(&requested_thread_id).await;
        if requested_thread_id.is_empty() {
            if let Some(thread_id) =
                slot.lock().await.as_ref().map(|state| state.current_thread_id.clone())
            {
                bash_command.thread_id = thread_id;
            }
        }
        let mut execution = match self
            .shell_runtime
            .run_action_detailed(&slot, bash_command, options.clone())
            .await
        {
            Ok(outcome) => {
                let audit_target =
                    slot.lock().await.as_ref().map(|state| {
                        (state.workspace_root.clone(), state.current_thread_id.clone())
                    });
                let temporary_artifact_usage =
                    if let Some((workspace_root, thread_id)) = audit_target {
                        match tokio::task::spawn_blocking(move || {
                            crate::utils::agent_temp::maintain_and_audit_session(
                                &workspace_root,
                                &thread_id,
                            )
                        })
                        .await
                        {
                            Ok(Ok(usage)) => Some(usage),
                            Ok(Err(error)) => {
                                warn!(%error, "post-Bash temporary-artifact audit failed");
                                None
                            }
                            Err(error) => {
                                warn!(%error, "post-Bash temporary-artifact audit worker failed");
                                None
                            }
                        }
                    } else {
                        None
                    };
                self.persist_state(&slot).await;
                let command_generation = outcome.command_generation;
                let execution_token = outcome.execution_token.clone();
                let generation_bound_actions = outcome.generation_bound_actions;
                let mut result = outcomes::bash_success_result(
                    Some(&recovery_args),
                    outcome,
                    options.compact_output,
                )?;
                // Show-once recovery note from a rehydrated session (restart
                // that lost the live shell, agent resume hint). Lead with it so
                // the model sees the recovery before the command output.
                let recovery_note =
                    slot.lock().await.as_mut().and_then(|state| state.recovery_note.take());
                if let Some(note) = recovery_note {
                    result
                        .content
                        .insert(0, ContentBlock::text(format!("[session recovery] {note}")));
                }
                if let Some(usage) = temporary_artifact_usage.as_ref() {
                    outcomes::attach_temporary_artifact_usage(
                        &mut result,
                        Some(&recovery_args),
                        usage,
                    );
                }
                BashCallExecution {
                    result,
                    command_generation,
                    execution_token,
                    generation_bound_actions,
                }
            }
            Err(error) => {
                if error.is_shell_runtime_failure() {
                    if let Some(state) = slot.lock().await.as_mut() {
                        state.record_shell_runtime_failure();
                    }
                }
                BashCallExecution::legacy(outcomes::tool_failure(
                    "BashCommand",
                    &error,
                    Some(&recovery_args),
                )?)
            }
        };
        if let Some(owner) = receipt_bound_verification {
            bind_receipt_verification_follow_up(&mut execution.result, &recovery_args);
            self.finish_receipt_bound_verification(
                owner,
                &execution.result,
                execution.command_generation,
                execution.execution_token.as_ref(),
            )
            .await;
        }
        Ok(execution)
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let read_files: ReadFiles = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.tool_session_for(&normalize_thread_id(&read_files.thread_id)).await;
        match crate::tools::read_files::handle_tool_call_detailed(&slot, read_files).await {
            Ok(mut outcome) => {
                if outcome.successful_files > 0 {
                    let _ = write!(outcome.text, "\n\n{READ_EDIT_GUIDANCE}");
                }
                self.persist_state(&slot).await;
                if let Some(error) = outcome.errors.first() {
                    let mut result =
                        outcomes::tool_failure("ReadFiles", error, Some(&recovery_args))?;
                    result.content = vec![ContentBlock::text(outcome.text)];
                    if let Some(Value::Object(structured)) = result.structured_content.as_mut() {
                        let data = structured
                            .entry("data")
                            .or_insert_with(|| Value::Object(serde_json::Map::new()));
                        if let Value::Object(data) = data {
                            data.insert(
                                "successful_files".to_string(),
                                json!(outcome.successful_files),
                            );
                            data.insert("failed_files".to_string(), json!(outcome.errors.len()));
                            data.insert("files".to_string(), json!(outcome.files));
                        }
                    }
                    Ok(result)
                } else {
                    let mut result =
                        CallToolResult::success(vec![ContentBlock::text(outcome.text)]);
                    result.structured_content = Some(json!({"files": outcome.files}));
                    Ok(result)
                }
            }
            Err(error) => outcomes::tool_failure("ReadFiles", &error, Some(&recovery_args)),
        }
    }

    async fn validate_edit_verification(
        slot: &SharedBashState,
        verification: &EditVerification,
    ) -> crate::errors::Result<()> {
        let state = slot.lock().await;
        let state = state.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        if !state.is_command_allowed(&verification.command) {
            return Err(WinxError::CommandNotAllowed(
                "verify_command is not allowed in the current mode".to_string(),
            ));
        }
        let allow_shell_probe = matches!(state.mode, crate::types::Modes::Wcgw);
        crate::utils::bash_parser::assert_single_statement(&verification.command, allow_shell_probe)
    }

    async fn finish_edit_verification(
        &self,
        prepared: &PreparedEditContext,
        edit_result: String,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let Some(verification) = prepared.verification.clone() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(edit_result)]));
        };
        let tool = prepared.surface.tool_name();
        let recovery_args = &prepared.original_arguments;
        let canonical_verification_id =
            super::mutations::verification_receipt_id_for_prepared(prepared).ok_or_else(|| {
                McpError::internal_error("canonical verification identity missing", None)
            })?;
        let delivery_verification_id =
            super::mutations::verification_delivery_id_for_prepared(prepared).ok_or_else(|| {
                McpError::internal_error("verification delivery identity missing", None)
            })?;
        let mut retry_arguments = if prepared.surface == EditSurface::EditFiles {
            json!({
                "action_json": {
                    "type": "verify",
                    "verification_id": canonical_verification_id,
                    "command": verification.command
                },
                "thread_id": prepared.thread_id
            })
        } else {
            json!({
                "verification_id": delivery_verification_id,
                "command": verification.command,
                "thread_id": prepared.thread_id
            })
        };
        if let Some(workspace_root) = prepared.workspace_root.as_ref() {
            retry_arguments["workspace_root"] = Value::String(workspace_root.clone());
        }
        if prepared.surface == EditSurface::EditFiles {
            retry_arguments["wait_for_seconds"] =
                json!(verification.wait_for_seconds.unwrap_or(15.0));
        } else if let Some(wait) = verification.wait_for_seconds {
            retry_arguments["wait_for_seconds"] = json!(wait);
        }
        let mut arguments = json!({
            "action_json": {
                "type": "verify",
                "verification_id": canonical_verification_id,
                "command": verification.command,
            },
            "thread_id": prepared.thread_id
        });
        if let Some(workspace_root) = prepared.workspace_root.as_ref() {
            arguments["workspace_root"] = Value::String(workspace_root.clone());
        }
        arguments["wait_for_seconds"] = json!(verification.wait_for_seconds.unwrap_or(15.0));
        let execution = self.handle_bash_command_with_output(Some(arguments), bash_options).await?;
        Ok(outcomes::edit_verification_result(
            tool,
            Some(recovery_args),
            &edit_result,
            execution.result,
            &outcomes::VerificationRecovery {
                id: delivery_verification_id,
                tool: if prepared.surface == EditSurface::EditFiles {
                    "BashCommand"
                } else {
                    "VerifyEdit"
                },
                arguments: retry_arguments,
            },
        ))
    }

    async fn handle_prepared_edit(
        &self,
        prepared_call: Option<&PreparedEditCall>,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let prepared_call = prepared_call.ok_or_else(|| {
            McpError::internal_error("normalized edit context missing after preflight", None)
        })?;
        let prepared = &prepared_call.context;
        let recovery_args = &prepared.original_arguments;
        let tool = prepared.surface.tool_name();
        let slot = &prepared_call.slot;
        match crate::tools::edit_files::handle_prepared(slot, prepared.clone()).await {
            Ok(outcome) => {
                self.checkpoint_committed_edit(
                    prepared,
                    &outcome.postconditions,
                    &outcome.uncommitted_paths,
                    &outcome.undo_ids,
                    outcome.next_undo_id.clone(),
                )
                .await;
                let mut result = if let Some(partial) = outcome.partial_failure.as_ref() {
                    partial_edit_result(prepared, &outcome, partial)
                } else {
                    self.finish_edit_verification(prepared, outcome.text.clone(), bash_options)
                        .await?
                };
                if prepared.surface == EditSurface::ApplyPatch {
                    if let Some(revision) = outcome.revisions.first() {
                        attach_result_data(
                            &mut result,
                            "new_revision",
                            Value::String(revision.clone()),
                        );
                    }
                }
                if !outcome.undo_ids.is_empty() {
                    attach_result_data(&mut result, "undo_ids", json!(outcome.undo_ids));
                }
                if let Some(next_undo_id) = outcome.next_undo_id {
                    attach_result_data(&mut result, "next_undo_id", Value::String(next_undo_id));
                }
                Ok(result)
            }
            Err(error) => {
                if error.is_search_match_conflict() {
                    self.persist_state(slot).await;
                }
                outcomes::tool_failure(tool, &error, Some(recovery_args))
            }
        }
    }

    async fn handle_verify_edit(
        &self,
        args: Option<Value>,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let verify: VerifyEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid VerifyEdit parameters: {error}"), None)
        })?;
        let command = verify.command.trim().to_string();
        if command.is_empty() {
            return outcomes::tool_failure(
                "VerifyEdit",
                &WinxError::InvalidInput("command must not be empty".to_string()),
                Some(&recovery_args),
            );
        }
        if verify.wait_for_seconds.is_some_and(|wait| {
            !wait.is_finite() || !(0.0..=MAX_VERIFY_WAIT_SECONDS).contains(&wait)
        }) {
            return outcomes::tool_failure(
                "VerifyEdit",
                &WinxError::InvalidInput(format!(
                    "wait_for_seconds must be between 0 and {MAX_VERIFY_WAIT_SECONDS}"
                )),
                Some(&recovery_args),
            );
        }
        let thread_id = normalize_thread_id(&verify.thread_id);
        let (slot, _session_guard) = self.session_for(&thread_id).await;
        let verification = EditVerification {
            command: command.clone(),
            wait_for_seconds: verify.wait_for_seconds,
        };
        if let Err(error) = Self::validate_edit_verification(&slot, &verification).await {
            return outcomes::tool_failure("VerifyEdit", &error, Some(&recovery_args));
        }
        let canonical_verification_id =
            match self.resolve_legacy_verification_id(&thread_id, &verify.verification_id).await {
                Ok(id) => id,
                Err(error) => {
                    return outcomes::tool_failure("VerifyEdit", &error, Some(&recovery_args));
                }
            };
        let mut arguments = json!({
            "action_json": {
                "type": "verify",
                "verification_id": canonical_verification_id,
                "command": command,
            },
            "thread_id": thread_id
        });
        if let Some(workspace_root) = recovery_args.get("workspace_root").and_then(Value::as_str) {
            arguments["workspace_root"] = Value::String(workspace_root.to_string());
        }
        arguments["wait_for_seconds"] = json!(verify.wait_for_seconds.unwrap_or(15.0));
        let execution = self.handle_bash_command_with_output(Some(arguments), bash_options).await?;
        Ok(outcomes::verify_edit_result(
            Some(&recovery_args),
            &verify.verification_id,
            execution.result,
        ))
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let context_save: ContextSave = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ContextSave parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&context_save.thread_id)).await;
        match crate::tools::context_save::handle_tool_call(&slot, context_save).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => outcomes::tool_failure("ContextSave", &error, Some(&recovery_args)),
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let read_image: ReadImage = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ReadImage parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_image.thread_id)).await;
        match crate::tools::read_image::handle_tool_call_detailed(&slot, read_image).await {
            Ok(crate::tools::read_image::ReadImageDelivery::Image {
                mime_type,
                base64_data,
                metadata,
            }) => {
                self.persist_state(&slot).await;
                let mut result =
                    CallToolResult::success(vec![ContentBlock::image(base64_data, mime_type)]);
                result.structured_content = serde_json::to_value(metadata).ok();
                Ok(result)
            }
            Ok(crate::tools::read_image::ReadImageDelivery::AlreadyDelivered { metadata }) => {
                self.persist_state(&slot).await;
                let mut result = CallToolResult::success(vec![ContentBlock::text(format!(
                    "ReadImage compacted an unchanged repeat (fingerprint {}). The same image was \
                     already delivered in this live session; use that prior image. Set force=true \
                     only when the image must be sent again.",
                    metadata.content_fingerprint
                ))]);
                result.structured_content = serde_json::to_value(metadata).ok();
                Ok(result)
            }
            Err(error) => outcomes::tool_failure("ReadImage", &error, Some(&recovery_args)),
        }
    }

    async fn handle_code_map(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let code_map: CodeMap = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid CodeMap parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&code_map.thread_id)).await;
        match crate::tools::code_map::handle_tool_call(&slot, code_map).await {
            Ok((text, structured)) => {
                let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
                result.structured_content = Some(structured);
                Ok(result)
            }
            Err(error) => outcomes::tool_failure("CodeMap", &error, Some(&recovery_args)),
        }
    }
}

/// Build a short, non-sensitive audit summary of a tool call's arguments.
pub(super) fn audit_summary(tool: &str, args: Option<&Value>) -> String {
    let Some(args) = args else {
        return "(no args)".to_string();
    };
    let string = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    match tool {
        "BashCommand" | "VerifyEdit" => {
            let action = args.get("action_json");
            let command = action
                .and_then(|action| action.get("command"))
                .and_then(Value::as_str)
                .or_else(|| args.get("command").and_then(Value::as_str));
            if let Some(command) = command {
                format!("command bytes={}", command.len())
            } else {
                let kind = action
                    .and_then(|action| action.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                format!("action={kind}")
            }
        }
        // Mutation calls are summarized from `PreparedEditContext` before
        // reaching this fallback. Never re-introduce legacy wire-shape/path
        // introspection here: it would make audit behavior diverge by alias.
        "FileWriteOrEdit" | "MultiFileEdit" | "ApplyPatch" | "UndoEdit" | "EditFiles" => {
            "edit=normalized-context-required".to_string()
        }
        "ReadImage" => format!("path={}", string("file_path")),
        "ReadFiles" => format!(
            "files={}",
            args.get("file_paths").and_then(Value::as_array).map_or(0, Vec::len)
        ),
        "Initialize" => {
            format!("ws={} mode={}", string("any_workspace_path"), string("mode_name"))
        }
        "ContextSave" => format!("id={}", string("id")),
        "CodeMap" => {
            format!("op={} path={} name={}", string("operation"), string("path"), string("name"))
        }
        _ => String::new(),
    }
}

fn attach_result_data(result: &mut CallToolResult, key: &str, value: Value) {
    let Some(Value::Object(structured)) = result.structured_content.as_mut() else {
        result.structured_content = Some(json!({key: value}));
        return;
    };
    if structured.get("status").is_some() {
        let data =
            structured.entry("data").or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(data) = data {
            data.insert(key.to_string(), value);
        }
    } else {
        structured.insert(key.to_string(), value);
    }
}

fn partial_edit_result(
    prepared: &PreparedEditContext,
    outcome: &crate::tools::edit_files::EditExecution,
    failure: &crate::tools::edit_files::PartialEditFailure,
) -> CallToolResult {
    let mut read_arguments = json!({
        "file_paths": outcome.uncommitted_paths,
        "thread_id": prepared.thread_id,
    });
    if let Some(workspace_root) = prepared.workspace_root.as_ref() {
        read_arguments["workspace_root"] = Value::String(workspace_root.clone());
    }
    let required_reads = outcome
        .uncommitted_paths
        .iter()
        .map(|path| json!({"path": path, "ranges": []}))
        .collect::<Vec<_>>();
    let mut result = CallToolResult::success(vec![ContentBlock::text(outcome.text.clone())]);
    result.structured_content = Some(json!({
        "status": "completed_with_issues",
        "tool": prepared.surface.tool_name(),
        "message": failure.message,
        "errorCode": "partial_commit",
        "retryable": false,
        "retrySameCall": false,
        "requiredReads": required_reads,
        "nextAction": {
            "tool": "ReadFiles",
            "instruction": "Read only the uncommitted suffix, then issue a new edit containing only those files. Never repeat the original batch.",
            "arguments": read_arguments
        },
        "data": {
            "thread_id": prepared.thread_id,
            "workspace_root": prepared.workspace_root,
            "edit_applied": true,
            "verification_skipped": prepared.verification.is_some(),
            "failed_index": failure.failed_index,
            "failed_path": failure.failed_path,
            "committed_paths": outcome.committed_paths,
            "uncommitted_paths": outcome.uncommitted_paths
        }
    }));
    result
}

fn bind_receipt_verification_follow_up(result: &mut CallToolResult, arguments: &Value) {
    let Some(Value::Object(envelope)) = result.structured_content.as_mut() else { return };
    let verification_id = arguments
        .get("action_json")
        .and_then(|action| action.get("verification_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data = envelope.entry("data").or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(data) = data {
        data.insert("action".to_string(), Value::String("verify".to_string()));
        data.insert("verification_id".to_string(), Value::String(verification_id.to_string()));
    }
    let status = envelope.get("status").and_then(Value::as_str).unwrap_or("failed");
    let active = matches!(status, "running" | "awaiting_input" | "awaiting_approval");
    let failed = result.is_error == Some(true)
        || envelope
            .get("data")
            .and_then(|data| data.get("exit_code"))
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0);
    if active || failed {
        envelope.insert(
            "nextAction".to_string(),
            json!({
                "tool": "BashCommand",
                "instruction": if active {
                    "Poll this exact receipt-bound verification action; never submit a generic status_check or rerun the edit."
                } else {
                    "Correct the diagnosed code or configuration first, then rerun this exact receipt-bound verification action; never repeat the edit."
                },
                "arguments": arguments
            }),
        );
    } else {
        envelope.remove("nextAction");
    }
}

fn redact_result(result: &mut CallToolResult) {
    for content in &mut result.content {
        if let ContentBlock::Text(text) = content {
            if let std::borrow::Cow::Owned(scrubbed) = crate::utils::redact::redact(&text.text) {
                text.text = scrubbed;
            }
        }
    }
    if let Some(structured) = result.structured_content.as_mut() {
        crate::utils::redact::redact_json(structured);
    }
}

fn append_command_section<const N: usize>(
    output: &mut String,
    title: &str,
    cwd: &Path,
    args: [&str; N],
) {
    let Ok(command_output) = Command::new("git").args(["-C"]).arg(cwd).args(args).output() else {
        return;
    };
    if !command_output.status.success() {
        return;
    }

    let content = String::from_utf8_lossy(&command_output.stdout);
    if content.trim().is_empty() {
        return;
    }
    let _ = writeln!(output, "\n# {title}\n{}", content.trim_end());
}

#[cfg(test)]
mod verification_tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;

    use super::{
        audit_summary, is_expected_recovery_status, partial_edit_result, RequestedTool,
        READ_EDIT_GUIDANCE,
    };
    use crate::runtime::ShellActionOptions;
    use crate::server::WinxService;
    use crate::state::BashState;
    use serde_json::json;

    #[test]
    fn read_guidance_leads_existing_file_edits_to_revision_bound_line_patch() {
        assert!(READ_EDIT_GUIDANCE.contains("mode=line_patch"));
        assert!(READ_EDIT_GUIDANCE.contains("data.files[].revision"));
        assert!(READ_EDIT_GUIDANCE.contains("Use BashCommand for commands"));
    }

    #[test]
    fn edit_verification_fields_are_removed_before_stable_struct_deserialization() {
        let arguments = json!({
            "file_path": "/workspace/lib.rs",
            "percentage_to_change": 100,
            "text_or_search_replace_blocks": "fn main() {}",
            "thread_id": "thread",
            "verify_command": " cargo check ",
            "verify_wait_for_seconds": 12.5
        });
        let normalized = crate::tools::edit_files::normalize_edit_call(
            crate::tools::edit_files::EditSurface::FileWriteOrEdit,
            arguments,
        )
        .expect("valid edit");
        let verification = normalized.verification.expect("present");
        assert_eq!(verification.command, "cargo check");
        assert_eq!(verification.wait_for_seconds, Some(12.5));
    }

    #[test]
    fn malformed_verification_is_rejected_before_an_edit_can_run() {
        let base = json!({
            "file_path": "/workspace/lib.rs",
            "percentage_to_change": 100,
            "text_or_search_replace_blocks": "content",
            "thread_id": "thread"
        });
        for invalid in [
            json!({"verify_command":""}),
            json!({"verify_wait_for_seconds":1}),
            json!({"verify_command":"true","verify_wait_for_seconds":61}),
        ] {
            let mut request = base.clone();
            request
                .as_object_mut()
                .expect("object")
                .extend(invalid.as_object().expect("object").clone());
            assert!(crate::tools::edit_files::normalize_edit_call(
                crate::tools::edit_files::EditSurface::FileWriteOrEdit,
                request,
            )
            .is_err());
        }
    }

    #[test]
    fn raw_edit_audit_never_introspects_legacy_wire_content() {
        let arguments = json!({
            "file_path": "/workspace/lib.rs",
            "verify_command": "secret command content"
        });
        let summary = audit_summary("FileWriteOrEdit", Some(&arguments));
        assert_eq!(summary, "edit=normalized-context-required");
        assert!(!summary.contains("/workspace/lib.rs"));
        assert!(!summary.contains("secret command content"));
    }

    #[test]
    fn expected_agent_recovery_states_do_not_raise_operational_warnings() {
        for status in ["needs_read", "needs_initialize", "not_found", "invalid_input", "conflict"] {
            assert!(is_expected_recovery_status(status), "{status}");
        }
        for status in ["denied", "failed", "verification_failed"] {
            assert!(!is_expected_recovery_status(status), "{status}");
        }
    }

    #[test]
    fn partial_commit_is_a_successful_typed_outcome_for_only_the_uncommitted_suffix() {
        let prepared = crate::tools::edit_files::PreparedEditContext {
            surface: crate::tools::edit_files::EditSurface::EditFiles,
            command: crate::tools::edit_files::EditCommand::Apply {
                changes: vec![
                    crate::tools::edit_files::EditChange::Replace {
                        file_path: "/workspace/first.rs".to_string(),
                        content: "first".to_string(),
                    },
                    crate::tools::edit_files::EditChange::Replace {
                        file_path: "/workspace/second.rs".to_string(),
                        content: "second".to_string(),
                    },
                ],
            },
            verification: None,
            thread_id: "thread".to_string(),
            workspace_root: Some("/workspace".to_string()),
            canonical_workspace_root: "/workspace".to_string(),
            targets: ["/workspace/first.rs", "/workspace/second.rs"]
                .into_iter()
                .map(|path| {
                    crate::tools::edit_files::CanonicalEditTarget::from_preflight(path.into())
                        .expect("canonical test target")
                })
                .collect(),
            original_arguments: json!({}),
            effective_permissions: crate::tool_policy::ToolPolicy::default().edit_permissions(),
        };
        let failure = crate::tools::edit_files::PartialEditFailure {
            failed_index: 2,
            failed_path: "/workspace/second.rs".to_string(),
            message: "second write failed after first committed".to_string(),
        };
        let outcome = crate::tools::edit_files::EditExecution {
            text: failure.message.clone(),
            revisions: vec!["rev-first".to_string()],
            undo_ids: vec![Some("undo-first".to_string())],
            next_undo_id: None,
            postconditions: vec![crate::state::bash_state::EditMutationPostcondition {
                path: "/workspace/first.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            committed_paths: vec!["/workspace/first.rs".to_string()],
            uncommitted_paths: vec!["/workspace/second.rs".to_string()],
            partial_failure: Some(failure.clone()),
        };
        let result = partial_edit_result(&prepared, &outcome, &failure);
        assert_ne!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured partial result");
        assert_eq!(structured["status"], "completed_with_issues");
        assert_eq!(structured["errorCode"], "partial_commit");
        assert_eq!(structured["data"]["edit_applied"], true);
        assert_eq!(
            structured["nextAction"]["arguments"]["file_paths"],
            json!(["/workspace/second.rs"])
        );
        assert!(!structured["nextAction"].to_string().contains("first.rs"));
    }

    #[tokio::test]
    async fn prepared_edit_guard_survives_session_churn_through_commit_and_checkpoint() {
        let workspace = tempfile::tempdir().expect("workspace");
        let target = workspace.path().join("created.txt");
        let service = WinxService::new();
        let (slot, setup_guard) = service.session_for("guarded-edit").await;
        let mut state = BashState::new();
        state.initialized = true;
        state.current_thread_id = crate::types::normalize_thread_id("guarded-edit");
        state.workspace_root = workspace.path().to_path_buf();
        state.cwd = workspace.path().to_path_buf();
        *slot.lock().await = Some(state);
        drop(setup_guard);

        let arguments = json!({
            "operation": "apply",
            "files": [{
                "file_path": target,
                "mode": "replace",
                "content": "committed\n"
            }],
            "thread_id": "guarded-edit",
            "workspace_root": workspace.path()
        });
        let prepared = service
            .prepare_edit_context(
                RequestedTool::Public(crate::tool_registry::ToolKind::EditFiles),
                Some(&arguments),
                crate::tool_policy::ToolPolicy::default().edit_permissions(),
            )
            .await
            .expect("prepare protocol")
            .expect("prepare tool result")
            .expect("prepared edit");

        for index in 0..(crate::server::sessions::MAX_SESSIONS + 12) {
            let (_, guard) = service.session_for(&format!("churn-before-{index}")).await;
            drop(guard);
        }
        assert!(Arc::ptr_eq(&slot, &prepared.slot));

        let result = service
            .handle_prepared_edit(Some(&prepared), ShellActionOptions::default())
            .await
            .expect("execute prepared edit");
        assert_ne!(result.is_error, Some(true));
        assert_eq!(std::fs::read_to_string(&target).expect("committed target"), "committed\n");

        for index in 0..(crate::server::sessions::MAX_SESSIONS + 12) {
            let (_, guard) = service.session_for(&format!("churn-after-{index}")).await;
            drop(guard);
        }
        let registry = service.sessions.lock().await;
        assert!(
            registry.slots.values().any(|candidate| Arc::ptr_eq(candidate, &slot)),
            "prepared call must pin its exact session through receipt checkpointing"
        );
    }
}
