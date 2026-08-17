use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use tracing::{info, warn};

use super::{SharedBashState, WinxService};
use crate::errors::WinxError;
use crate::state::bash_state::generate_thread_id;
use crate::types::{
    normalize_thread_id, BashCommand, CodeMap, ContextSave, FileWriteOrEdit, Initialize,
    MultiFileEdit, ReadFiles, ReadImage, UndoEdit,
};

/// Map a domain [`WinxError`] to the right JSON-RPC error kind.
///
/// Client-caused failures become `invalid_request`; genuine internal faults
/// remain `internal_error`. The match is exhaustive so new variants must be
/// classified deliberately.
pub(super) fn to_mcp_error(tool: &str, error: &WinxError) -> McpError {
    let message = format!("{tool} failed: {error}");
    match error {
        WinxError::BashStateNotInitialized
        | WinxError::NoActiveCommand(_)
        | WinxError::BackgroundSessionNotFound(_)
        | WinxError::EmptyInteractiveInput { .. }
        | WinxError::InteractiveTargetNotRunning(_)
        | WinxError::CommandNotAllowed(_)
        | WinxError::PathSecurityError { .. }
        | WinxError::ThreadIdMismatch(_)
        | WinxError::ParameterValidationError { .. }
        | WinxError::MissingParameterError { .. }
        | WinxError::NullValueError { .. }
        | WinxError::ArgumentParseError(_)
        | WinxError::JsonParseError(_)
        | WinxError::DeserializationError(_)
        | WinxError::WorkspacePathError(_)
        | WinxError::InvalidInput(_)
        | WinxError::ParseError(_)
        | WinxError::FileAccessError { .. }
        | WinxError::RecoverableSuggestionError { .. }
        | WinxError::SearchReplaceSyntaxError(_)
        | WinxError::SearchReplaceSyntaxErrorDetailed { .. }
        | WinxError::SearchBlockNotFound(_)
        | WinxError::SearchBlockAmbiguous { .. }
        | WinxError::FileTooLarge { .. }
        | WinxError::InteractiveCommandDetected { .. }
        | WinxError::CommandAlreadyRunning { .. } => McpError::invalid_request(message, None),
        WinxError::ShellInitializationError(_)
        | WinxError::BashStateLockError(_)
        | WinxError::CommandExecutionError(_)
        | WinxError::SerializationError(_)
        | WinxError::FileWriteError { .. }
        | WinxError::DataLoadingError(_)
        | WinxError::ContextSaveError(_)
        | WinxError::CommandTimeout { .. }
        | WinxError::ProcessCleanupError { .. }
        | WinxError::BufferOverflow { .. }
        | WinxError::SessionRecoveryError { .. }
        | WinxError::ResourceAllocationError { .. }
        | WinxError::IoError(_)
        | WinxError::ConfigurationError(_)
        | WinxError::FileError(_) => McpError::internal_error(message, None),
    }
}

impl WinxService {
    pub(super) async fn execute_tool_call(
        &self,
        param: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let tool = param.name.to_string();
        let args_value = param.arguments.map(Value::Object);
        let summary =
            crate::utils::redact::redact(&audit_summary(&tool, args_value.as_ref())).into_owned();
        let started = std::time::Instant::now();

        let result = match tool.as_str() {
            "Initialize" => self.handle_initialize(args_value).await,
            "BashCommand" => self.handle_bash_command(args_value).await,
            "ReadFiles" => self.handle_read_files(args_value).await,
            "FileWriteOrEdit" => self.handle_file_write_or_edit(args_value).await,
            "MultiFileEdit" => self.handle_multi_file_edit(args_value).await,
            "UndoEdit" => self.handle_undo_edit(args_value).await,
            "ContextSave" => self.handle_context_save(args_value).await,
            "ReadImage" => self.handle_read_image(args_value).await,
            "CodeMap" => self.handle_code_map(args_value).await,
            _ => Err(McpError::invalid_request(format!("Unknown tool: {tool}"), None)),
        };

        let result = match result {
            Ok(mut call) => {
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
            Ok(_) => info!(tool = %tool, ms = elapsed_ms, "tool call ok — {summary}"),
            Err(error) => warn!(
                tool = %tool,
                ms = elapsed_ms,
                "tool call error — {summary}: {}",
                error.message
            ),
        }
        result
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
        let mut initialize: Initialize = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid Initialize parameters: {error}"), None)
        })?;

        let mut thread_id = normalize_thread_id(&initialize.thread_id);
        if thread_id.is_empty() {
            thread_id = generate_thread_id();
            initialize.thread_id.clone_from(&thread_id);
        }
        let (slot, _session_guard) = self.session_for(&thread_id).await;

        match crate::tools::initialize::handle_tool_call_with_runtime(
            self.shell_runtime.as_ref(),
            &slot,
            initialize,
        )
        .await
        {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => Err(to_mcp_error("Initialize", &error)),
        }
    }

    pub(super) async fn handle_bash_command(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
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
        match crate::tools::bash_command::handle_tool_call_with_runtime(
            self.shell_runtime.as_ref(),
            &slot,
            bash_command,
        )
        .await
        {
            Ok(output) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(output)]))
            }
            Err(error) => Err(to_mcp_error("BashCommand", &error)),
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_files: ReadFiles = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_files.thread_id)).await;
        match crate::tools::read_files::handle_tool_call(&slot, read_files).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => Err(to_mcp_error("ReadFiles", &error)),
        }
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let edit: FileWriteOrEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid FileWriteOrEdit parameters: {error}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&edit.thread_id)).await;
        match crate::tools::file_write_or_edit::handle_tool_call(&slot, edit).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => Err(to_mcp_error("FileWriteOrEdit", &error)),
        }
    }

    async fn handle_multi_file_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let multi: MultiFileEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid MultiFileEdit parameters: {error}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&multi.thread_id)).await;
        match crate::tools::multi_file_edit::handle_tool_call(&slot, multi).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => Err(to_mcp_error("MultiFileEdit", &error)),
        }
    }

    async fn handle_undo_edit(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let undo: UndoEdit = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid UndoEdit parameters: {error}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&undo.thread_id)).await;
        match crate::tools::undo_edit::handle_tool_call(&slot, undo).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
            }
            Err(error) => Err(to_mcp_error("UndoEdit", &error)),
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
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
            Err(error) => Err(to_mcp_error("ContextSave", &error)),
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_image: ReadImage = Self::lenient_from_value(args).map_err(|error| {
            McpError::invalid_request(format!("Invalid ReadImage parameters: {error}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_image.thread_id)).await;
        match crate::tools::read_image::handle_tool_call(&slot, read_image).await {
            Ok((mime_type, base64_data)) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![ContentBlock::image(base64_data, mime_type)]))
            }
            Err(error) => Err(to_mcp_error("ReadImage", &error)),
        }
    }

    async fn handle_code_map(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
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
            Err(error) => Err(to_mcp_error("CodeMap", &error)),
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
        "FileWriteOrEdit" | "ReadImage" | "UndoEdit" => {
            format!("path={}", string("file_path"))
        }
        "MultiFileEdit" => {
            format!("files={}", args.get("files").and_then(Value::as_array).map_or(0, Vec::len))
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
