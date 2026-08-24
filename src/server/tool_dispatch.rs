use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::{outcomes, SharedBashState, WinxService};
use crate::errors::WinxError;
use crate::runtime::{ShellActionOptions, ShellExecutionToken};
use crate::state::bash_state::generate_thread_id;
use crate::types::{
    normalize_thread_id, BashCommand, CodeMap, ContextSave, FileWriteOrEdit, Initialize,
    MultiFileEdit, ReadFiles, ReadImage, UndoEdit,
};

const MAX_VERIFY_WAIT_SECONDS: f32 = 60.0;

#[derive(Clone, Debug)]
struct EditVerification {
    command: String,
    wait_for_seconds: Option<f32>,
}

fn take_edit_verification(args: &mut Value) -> Result<Option<EditVerification>, McpError> {
    let map = args
        .as_object_mut()
        .ok_or_else(|| McpError::invalid_request("Edit tool parameters must be an object", None))?;
    let command = match map.remove("verify_command") {
        None | Some(Value::Null) => None,
        Some(Value::String(command)) if !command.trim().is_empty() => {
            Some(command.trim().to_string())
        }
        Some(Value::String(_)) => {
            return Err(McpError::invalid_request("verify_command must not be empty", None));
        }
        Some(_) => {
            return Err(McpError::invalid_request("verify_command must be a string", None));
        }
    };
    let wait_for_seconds = match map.remove("verify_wait_for_seconds") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let wait = serde_json::from_value::<f32>(value).map_err(|error| {
                McpError::invalid_request(format!("Invalid verify_wait_for_seconds: {error}"), None)
            })?;
            if !wait.is_finite() || !(0.0..=MAX_VERIFY_WAIT_SECONDS).contains(&wait) {
                return Err(McpError::invalid_request(
                    format!(
                        "verify_wait_for_seconds must be between 0 and {MAX_VERIFY_WAIT_SECONDS}"
                    ),
                    None,
                ));
            }
            Some(wait)
        }
    };

    match command {
        Some(command) => Ok(Some(EditVerification { command, wait_for_seconds })),
        None if wait_for_seconds.is_some() => {
            Err(McpError::invalid_request("verify_wait_for_seconds requires verify_command", None))
        }
        None => Ok(None),
    }
}

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
    matches!(status, "needs_read" | "needs_initialize" | "not_found" | "invalid_input" | "conflict")
}

pub(super) struct BashCallExecution {
    result: CallToolResult,
    command_generation: Option<u64>,
    execution_token: Option<ShellExecutionToken>,
    generation_bound_actions: bool,
}

impl WinxService {
    pub(super) async fn execute_tool_call(
        &self,
        param: CallToolRequestParams,
        bash_options: ShellActionOptions,
    ) -> Result<ToolCallExecution, McpError> {
        let tool = param.name.to_string();
        let args_value = param.arguments.map(Value::Object);
        let orchestration_args = args_value.clone();
        let summary =
            crate::utils::redact::redact(&audit_summary(&tool, args_value.as_ref())).into_owned();
        let started = std::time::Instant::now();

        let (result, bash_runtime) = match tool.as_str() {
            "Initialize" => (self.handle_initialize(args_value).await, None),
            "BashCommand" => {
                match self.handle_bash_command_with_output(args_value, bash_options.clone()).await {
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
            "ReadFiles" => (self.handle_read_files(args_value).await, None),
            "FileWriteOrEdit" => {
                (self.handle_file_write_or_edit(args_value, bash_options.clone()).await, None)
            }
            "MultiFileEdit" => (self.handle_multi_file_edit(args_value, bash_options).await, None),
            "UndoEdit" => (self.handle_undo_edit(args_value).await, None),
            "ContextSave" => (self.handle_context_save(args_value).await, None),
            "ReadImage" => (self.handle_read_image(args_value).await, None),
            "CodeMap" => (self.handle_code_map(args_value).await, None),
            _ => (Err(McpError::invalid_request(format!("Unknown tool: {tool}"), None)), None),
        };

        let result = match result {
            Ok(mut call) => {
                outcomes::decorate_success(&tool, orchestration_args.as_ref(), &mut call);
                redact_result(&mut call);
                Ok(call)
            }
            Err(mut error) => {
                error.message = crate::utils::redact::redact(&error.message).into_owned().into();
                Err(error)
            }
        };

        let elapsed_ms = started.elapsed().as_millis();
        match &result {
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
                    "initialize_reused": outcome.transition
                        == crate::tools::initialize::InitializeTransition::AttachedExisting,
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
                    "temporary_artifact_dir": outcome.temporary_artifact_dir,
                    "temporary_artifact_env": "WINX_TEMP_DIR",
                    "temporary_artifact_ttl_seconds": outcome.temporary_artifact_ttl_seconds,
                    "temporary_artifact_max_bytes": outcome.temporary_artifact_max_bytes,
                    "temporary_artifact_max_session_bytes": outcome.temporary_artifact_max_session_bytes,
                    "temporary_artifact_max_file_bytes": outcome.temporary_artifact_max_file_bytes,
                    "temporary_artifact_max_files": outcome.temporary_artifact_max_files,
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

    pub(super) async fn handle_bash_command_with_output(
        &self,
        args: Option<Value>,
        options: ShellActionOptions,
    ) -> Result<BashCallExecution, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let mut bash_command: BashCommand = serde_json::from_value(args).map_err(|error| {
            McpError::invalid_request(
                format!(
                    "Invalid BashCommand parameters: {error}. Accepted forms include {{\"action_json\": {{\"command\": \"pwd\"}}}}, {{\"command\": \"pwd\"}}, or {{\"action_json\": {{\"type\": \"status_check\", \"status_check\": true}}}}."
                ),
                None,
            )
        })?;

        let requested_thread_id = normalize_thread_id(&bash_command.thread_id);
        let (slot, _session_guard) = self.session_for(&requested_thread_id).await;
        if requested_thread_id.is_empty() {
            if let Some(thread_id) =
                slot.lock().await.as_ref().map(|state| state.current_thread_id.clone())
            {
                bash_command.thread_id = thread_id;
            }
        }
        match self.shell_runtime.run_action_detailed(&slot, bash_command, options.clone()).await {
            Ok(outcome) => {
                let audit_target =
                    slot.lock().await.as_ref().map(|state| {
                        (state.workspace_root.clone(), state.current_thread_id.clone())
                    });
                let temporary_artifact_usage =
                    if let Some((workspace_root, thread_id)) = audit_target {
                        match tokio::task::spawn_blocking(move || {
                            crate::utils::agent_temp::audit_session(&workspace_root, &thread_id)
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
                if let Some(usage) = temporary_artifact_usage.as_ref() {
                    outcomes::attach_temporary_artifact_usage(
                        &mut result,
                        Some(&recovery_args),
                        usage,
                    );
                }
                Ok(BashCallExecution {
                    result,
                    command_generation,
                    execution_token,
                    generation_bound_actions,
                })
            }
            Err(error) => Ok(BashCallExecution {
                result: outcomes::tool_failure("BashCommand", &error, Some(&recovery_args))?,
                command_generation: None,
                execution_token: None,
                generation_bound_actions: false,
            }),
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let read_files: ReadFiles = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_files.thread_id)).await;
        match crate::tools::read_files::handle_tool_call_detailed(&slot, read_files).await {
            Ok(outcome) => {
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
                        }
                    }
                    Ok(result)
                } else {
                    Ok(CallToolResult::success(vec![ContentBlock::text(outcome.text)]))
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
        tool: &str,
        recovery_args: &Value,
        thread_id: &str,
        edit_result: String,
        verification: Option<EditVerification>,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let Some(verification) = verification else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(edit_result)]));
        };
        let mut arguments = json!({
            "action_json": {
                "type": "command",
                "command": verification.command,
                "is_background": false,
                "allow_multi": false
            },
            "thread_id": thread_id
        });
        if let Some(workspace_root) = recovery_args.get("workspace_root").and_then(Value::as_str) {
            arguments["workspace_root"] = Value::String(workspace_root.to_string());
        }
        if let Some(wait) = verification.wait_for_seconds {
            arguments["wait_for_seconds"] = json!(wait);
        }
        let execution = self.handle_bash_command_with_output(Some(arguments), bash_options).await?;
        Ok(outcomes::edit_verification_result(
            tool,
            Some(recovery_args),
            &edit_result,
            execution.result,
        ))
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let mut args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let verification = take_edit_verification(&mut args)?;
        let edit: FileWriteOrEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid FileWriteOrEdit parameters: {error}"), None)
        })?;

        let thread_id = normalize_thread_id(&edit.thread_id);
        let (slot, _session_guard) = self.session_for(&thread_id).await;
        if let Some(verification) = verification.as_ref() {
            if let Err(error) = Self::validate_edit_verification(&slot, verification).await {
                return outcomes::tool_failure("FileWriteOrEdit", &error, Some(&recovery_args));
            }
        }
        match crate::tools::file_write_or_edit::handle_tool_call(&slot, edit).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                self.finish_edit_verification(
                    "FileWriteOrEdit",
                    &recovery_args,
                    &thread_id,
                    result,
                    verification,
                    bash_options,
                )
                .await
            }
            Err(error) => outcomes::tool_failure("FileWriteOrEdit", &error, Some(&recovery_args)),
        }
    }

    async fn handle_multi_file_edit(
        &self,
        args: Option<Value>,
        bash_options: ShellActionOptions,
    ) -> Result<CallToolResult, McpError> {
        let mut args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let verification = take_edit_verification(&mut args)?;
        let multi: MultiFileEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid MultiFileEdit parameters: {error}"), None)
        })?;

        let thread_id = normalize_thread_id(&multi.thread_id);
        let (slot, _session_guard) = self.session_for(&thread_id).await;
        if let Some(verification) = verification.as_ref() {
            if let Err(error) = Self::validate_edit_verification(&slot, verification).await {
                return outcomes::tool_failure("MultiFileEdit", &error, Some(&recovery_args));
            }
        }
        match crate::tools::multi_file_edit::handle_tool_call(&slot, multi).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                self.finish_edit_verification(
                    "MultiFileEdit",
                    &recovery_args,
                    &thread_id,
                    result,
                    verification,
                    bash_options,
                )
                .await
            }
            Err(error) => outcomes::tool_failure("MultiFileEdit", &error, Some(&recovery_args)),
        }
    }

    async fn handle_undo_edit(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let recovery_args = args.clone();
        let undo: UndoEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid UndoEdit parameters: {error}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&undo.thread_id)).await;
        match crate::tools::undo_edit::handle_tool_call(&slot, undo).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => outcomes::tool_failure("UndoEdit", &error, Some(&recovery_args)),
        }
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
        "BashCommand" => {
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
        "FileWriteOrEdit" => {
            format!(
                "path={} verify={}",
                string("file_path"),
                args.get("verify_command").is_some_and(|value| !value.is_null())
            )
        }
        "ReadImage" | "UndoEdit" => format!("path={}", string("file_path")),
        "MultiFileEdit" => {
            format!(
                "files={} verify={}",
                args.get("files").and_then(Value::as_array).map_or(0, Vec::len),
                args.get("verify_command").is_some_and(|value| !value.is_null())
            )
        }
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

    use super::{audit_summary, is_expected_recovery_status, take_edit_verification};
    use serde_json::json;

    #[test]
    fn edit_verification_fields_are_removed_before_stable_struct_deserialization() {
        let mut arguments = json!({
            "file_path": "/workspace/lib.rs",
            "percentage_to_change": 100,
            "text_or_search_replace_blocks": "fn main() {}",
            "thread_id": "thread",
            "verify_command": " cargo check ",
            "verify_wait_for_seconds": 12.5
        });
        let verification =
            take_edit_verification(&mut arguments).expect("valid verification").expect("present");
        assert_eq!(verification.command, "cargo check");
        assert_eq!(verification.wait_for_seconds, Some(12.5));
        assert!(arguments.get("verify_command").is_none());
        assert!(arguments.get("verify_wait_for_seconds").is_none());
    }

    #[test]
    fn malformed_verification_is_rejected_before_an_edit_can_run() {
        assert!(take_edit_verification(&mut json!({"verify_command":""})).is_err());
        assert!(take_edit_verification(&mut json!({"verify_wait_for_seconds":1})).is_err());
        assert!(take_edit_verification(
            &mut json!({"verify_command":"true","verify_wait_for_seconds":61})
        )
        .is_err());
    }

    #[test]
    fn edit_audit_records_verification_without_command_content() {
        let arguments = json!({
            "file_path": "/workspace/lib.rs",
            "verify_command": "secret command content"
        });
        let summary = audit_summary("FileWriteOrEdit", Some(&arguments));
        assert!(summary.contains("verify=true"));
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
}
