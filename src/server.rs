//! Winx MCP Server implementation using rmcp 0.12.0
//! Core MCP tools only - High performance shell and file management

use rmcp::{
    model::{
        Annotated, CallToolRequestParams, CallToolResult, Content, GetPromptRequestParams,
        GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, Prompt, PromptMessage, PromptMessageRole, ProtocolVersion,
        RawResource, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::schema_for;
use serde_json::Value;
use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::state::BashState;
use crate::types::{BashCommand, ContextSave, FileWriteOrEdit, Initialize, ReadFiles, ReadImage};

/// Type alias for the shared bash state - uses `tokio::sync::Mutex` for async safety
pub type SharedBashState = Arc<Mutex<Option<BashState>>>;

/// Helper function to create JSON schema from schemars Schema
fn schema_to_input_schema<T: schemars::JsonSchema>() -> Arc<serde_json::Map<String, Value>> {
    let schema = schema_for!(T);
    let value = serde_json::to_value(schema).unwrap_or(Value::Object(serde_json::Map::new()));
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

fn mcp_tool<T: schemars::JsonSchema>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool {
    Tool::new(name, description, schema_to_input_schema::<T>()).with_annotations(annotations)
}

const INITIALIZE_DESCRIPTION: &str =
    "- Always call this at the start of the conversation before using any of the shell tools from wcgw. \
     - Use `any_workspace_path` to initialize the shell in the appropriate project directory. \
     - If the user has mentioned a workspace or project root or any other file or folder use it to set `any_workspace_path`. \
     - If user has mentioned any files use `initial_files_to_read` to read, use absolute paths only (~ allowed) \
     - By default use mode \"wcgw\" \
     - In \"code-writer\" mode, set the commands and globs which user asked to set, otherwise use 'all'. \
     - Use type=\"first_call\" if it's the first call to this tool. \
     - Use type=\"user_asked_mode_change\" if in a conversation user has asked to change mode. \
     - Use type=\"reset_shell\" if in a conversation shell is not working after multiple tries. \
     - Use type=\"user_asked_change_workspace\" if in a conversation user asked to change workspace";

const BASH_COMMAND_DESCRIPTION: &str =
    "- Execute a bash command. This is stateful (beware with subsequent calls). \
     - Accepted payloads include action_json with an explicit type, action_json shorthand such as {\"command\":\"pwd\"}, or top-level shorthand such as {\"command\":\"pwd\"}. \
     - Status of the command and the current working directory will always be returned at the end. \
     - The first or the last line might be `(...truncated)` if the output is too long. \
     - Always run `pwd` if you get any file or directory not found error to make sure you're not lost. \
     - Do not run bg commands using \"&\", instead use this tool. \
     - You must not use echo/cat to read/write files, use ReadFiles/FileWriteOrEdit \
     - In order to check status of previous command, use `status_check` with empty command argument. \
     - Only command is allowed to run at a time. You need to wait for any previous command to finish before running a new one. \
     - Programs don't hang easily, so most likely explanation for no output is usually that the program is still running, and you need to check status again. \
     - Do not send Ctrl-c before checking for status till 10 minutes or whatever is appropriate for the program to finish. \
     - Only run long running commands in background. Each background command is run in a new non-reusable shell. \
     - On running a bg command you'll get a bg command id that you should use to get status or interact.";

const READ_FILES_DESCRIPTION: &str =
    "- Read full file content of one or more files. \
     - Prefer this over reading files with BashCommand (cat/head/tail): the output is token-budgeted and the read is recorded so FileWriteOrEdit can edit the file afterward. \
     - Do NOT use this for binary files or images — use ReadImage for images. \
     - Provide absolute paths only (~ allowed) \
     - Only if the task requires line numbers understanding: \
     - You may extract a range of lines. E.g., `/path/to/file:1-10` for lines 1-10. You can drop start or end like `/path/to/file:1-` or `/path/to/file:-10`";

const FILE_WRITE_OR_EDIT_DESCRIPTION: &str =
    "- Writes or edits a file based on the percentage of changes. \
     - Prefer this over writing/editing files with BashCommand (echo/sed/redirects/heredocs). \
     - For an edit, the file must have been read first with ReadFiles, otherwise the edit is rejected. \
     - Use absolute path only (~ allowed). \
     - First write down percentage of lines that need to be replaced in the file (between 0-100) in percentage_to_change \
     - percentage_to_change should be low if mostly new code is to be added. It should be high if a lot of things are to be replaced. \
     - If percentage_to_change > 50, provide full file content in text_or_search_replace_blocks \
     - If percentage_to_change <= 50, text_or_search_replace_blocks should be search/replace blocks. \
     \
     Instructions for editing files. \
     # Example \
     ## Input file \
     ``` \
     import numpy as np \
     from impls import impl1, impl2 \
     \
     def hello(): \
         \"print a greeting\" \
         print(\"hello\") \
     \
     def call_hello(): \
         \"call hello\" \
         hello() \
         print(\"Called\") \
         impl1() \
         hello() \
         impl2() \
     ``` \
     ## Edit format on the input file \
     ``` \
     <<<<<<< SEARCH \
     from impls import impl1, impl2 \
     ======= \
     from impls import impl1, impl2 \
     from hello import hello as hello_renamed \
     >>>>>>> REPLACE \
     <<<<<<< SEARCH \
     def hello(): \
         \"print a greeting\" \
         print(\"hello\") \
     ======= \
     >>>>>>> REPLACE \
     <<<<<<< SEARCH \
     def call_hello(): \
         \"call hello\" \
         hello() \
     ======= \
     def call_hello_renamed(): \
         \"call hello renamed\" \
         hello_renamed() \
     >>>>>>> REPLACE \
     <<<<<<< SEARCH \
     impl1() \
     hello() \
     impl2() \
     ======= \
     impl1() \
     hello_renamed() \
     impl2() \
     >>>>>>> REPLACE \
     ``` \
     # *SEARCH/REPLACE block* Rules: \
     Every \"<<<<<<< SEARCH\" section must *EXACTLY MATCH* the existing file content, character for character, including all comments, docstrings, whitespaces, etc. \
     Including multiple unique *SEARCH/REPLACE* blocks if needed. \
     Include enough and only enough lines in each SEARCH section to uniquely match each set of lines that need to change. \
     Keep *SEARCH/REPLACE* blocks concise. \
     Break large *SEARCH/REPLACE* blocks into a series of smaller blocks that each change a small portion of the file. \
     Include just the changing lines, and a few surrounding lines (0-3 lines) if needed for uniqueness. \
     Other than for uniqueness, avoid including those lines which do not change in search (and replace) blocks. Target 0-3 non trivial extra lines per block. \
     Preserve leading spaces and indentations in both SEARCH and REPLACE blocks.";

const CONTEXT_SAVE_DESCRIPTION: &str =
    "Saves provided description and file contents of all the relevant file paths or globs in a single text file. \
     - Provide random 3 word unqiue id or whatever user provided. \
     - Leave project path as empty string if no project path";

static WINX_TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
static WINX_PROMPTS: OnceLock<Vec<Prompt>> = OnceLock::new();

fn winx_tools() -> Vec<Tool> {
    WINX_TOOLS.get_or_init(build_winx_tools).clone()
}

fn build_winx_tools() -> Vec<Tool> {
    vec![
        mcp_tool::<Initialize>(
            "Initialize",
            INITIALIZE_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        ),
        mcp_tool::<BashCommand>(
            "BashCommand",
            BASH_COMMAND_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(true),
        ),
        mcp_tool::<ReadFiles>(
            "ReadFiles",
            READ_FILES_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        ),
        mcp_tool::<FileWriteOrEdit>(
            "FileWriteOrEdit",
            FILE_WRITE_OR_EDIT_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(false),
        ),
        mcp_tool::<ContextSave>(
            "ContextSave",
            CONTEXT_SAVE_DESCRIPTION,
            ToolAnnotations::new().destructive(false).open_world(false),
        ),
        mcp_tool::<ReadImage>(
            "ReadImage",
            "Read an image from the shell.",
            ToolAnnotations::new().read_only(true).open_world(false),
        ),
    ]
}

fn winx_prompts() -> Vec<Prompt> {
    WINX_PROMPTS
        .get_or_init(|| {
            vec![Prompt::new(
                "KnowledgeTransfer",
                Some("Summarize current Winx state, workspace context, and handoff notes."),
                None,
            )]
        })
        .clone()
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

/// Winx service with shared bash state
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment (async-safe)
    pub bash_state: SharedBashState,
    /// Version information for the service
    pub version: String,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new `WinxService` instance
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// `ServerHandler` implementation
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(
            Implementation::new("winx-mcp-server", self.version.clone())
                .with_title("Winx High-Performance MCP"),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_instructions(
                "Winx is a high-performance Rust implementation of MCP tools for shell and file management."
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult { tools: winx_tools(), next_cursor: None, meta: None })
    }

    async fn list_resources(
        &self,
        _param: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![Annotated {
                raw: RawResource {
                    uri: "file://readme".into(),
                    name: "README".into(),
                    description: Some("Project README documentation".into()),
                    mime_type: Some("text/markdown".into()),
                    size: None,
                    title: None,
                    icons: None,
                    meta: None,
                },
                annotations: None,
            }],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult { prompts: winx_prompts(), next_cursor: None, meta: None })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != "KnowledgeTransfer" {
            return Err(McpError::invalid_request(
                format!("Unknown prompt: {}", request.name),
                None,
            ));
        }

        let text = self.knowledge_transfer_prompt_text().await;

        Ok(GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)])
            .with_description("Knowledge transfer handoff prompt"))
    }

    async fn read_resource(
        &self,
        param: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let content = match param.uri.as_ref() {
            "file://readme" => match tokio::fs::read_to_string("README.md").await {
                Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                Err(_) => vec![ResourceContents::text(
                    "README.md not found".to_string(),
                    param.uri.clone(),
                )],
            },
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown resource URI: {}", param.uri),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult::new(content))
    }

    async fn call_tool(
        &self,
        param: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool = param.name.to_string();
        let args_value = param.arguments.map(Value::Object);
        // Audit trail: one structured line per tool call, including the outcome
        // and wall-clock. Successes were previously silent — only errors logged —
        // which made debugging remote (ChatGPT) sessions guesswork.
        let summary = audit_summary(&tool, args_value.as_ref());
        let started = std::time::Instant::now();

        let result = match tool.as_str() {
            "Initialize" => self.handle_initialize(args_value).await,
            "BashCommand" => self.handle_bash_command(args_value).await,
            "ReadFiles" => self.handle_read_files(args_value).await,
            "FileWriteOrEdit" => self.handle_file_write_or_edit(args_value).await,
            "ContextSave" => self.handle_context_save(args_value).await,
            "ReadImage" => self.handle_read_image(args_value).await,
            _ => Err(McpError::invalid_request(format!("Unknown tool: {tool}"), None)),
        };

        let ms = started.elapsed().as_millis();
        match &result {
            Ok(_) => info!(tool = %tool, ms, "tool call ok — {summary}"),
            Err(error) => warn!(tool = %tool, ms, "tool call error — {summary}: {}", error.message),
        }
        result
    }
}

/// Build a short, non-sensitive audit summary of a tool call's arguments.
fn audit_summary(tool: &str, args: Option<&Value>) -> String {
    let Some(args) = args else {
        return "(no args)".to_string();
    };
    let s = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let clip = |text: String| text.chars().take(100).collect::<String>();
    match tool {
        "BashCommand" => {
            let action = args.get("action_json");
            let cmd = action
                .and_then(|a| a.get("command"))
                .and_then(Value::as_str)
                .or_else(|| args.get("command").and_then(Value::as_str));
            if let Some(cmd) = cmd {
                format!("cmd={:?}", clip(cmd.to_string()))
            } else {
                let kind =
                    action.and_then(|a| a.get("type")).and_then(Value::as_str).unwrap_or("?");
                format!("action={kind}")
            }
        }
        "FileWriteOrEdit" | "ReadImage" => format!("path={}", s("file_path")),
        "ReadFiles" => {
            format!(
                "files={}",
                args.get("file_paths").and_then(Value::as_array).map_or(0, Vec::len)
            )
        }
        "Initialize" => format!("ws={} mode={}", s("any_workspace_path"), s("mode_name")),
        "ContextSave" => format!("id={}", s("id")),
        _ => String::new(),
    }
}

impl WinxService {
    async fn knowledge_transfer_prompt_text(&self) -> String {
        let mut text = String::from(
            "Prepare a concise handoff for another agent. Include active objective, current state, important files, changed files, blockers, validation already run, and exact next commands.\n",
        );

        let state_snapshot = {
            let guard = self.bash_state.lock().await;
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
        };

        let Some((thread_id, workspace_root, cwd, mode, whitelist, whitelist_count)) =
            state_snapshot
        else {
            text.push_str("\n# Current Winx state\nWinx is not initialized.\n");
            return text;
        };

        let _ = writeln!(
            text,
            "\n# Current Winx state\nThread: {thread_id}\nWorkspace: {}\nCwd: {}\nMode: {mode}\nWhitelisted files: {whitelist_count}",
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

        // Sections the ContextSave `description` should contain, tailored to the
        // mode: architect produces a plan (no edits), the others produce a status
        // + pending-issues handoff (wcgw parity: WCGW_KT vs ARCHITECT_KT).
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

    async fn persist_state(&self) {
        let guard = self.bash_state.lock().await;
        if let Some(state) = guard.as_ref() {
            if let Err(error) = state.save_state_to_disk() {
                warn!("Failed to persist bash state: {}", error);
            }
        }
    }

    /// Deserialize `args` into `T`, retrying once after JSON-decoding any string
    /// field that is itself an encoded object/array. LLMs sometimes send a nested
    /// param (e.g. `code_writer_config`) as a JSON string instead of an object;
    /// wcgw applies the same leniency in its tool dispatch.
    fn lenient_from_value<T: serde::de::DeserializeOwned>(
        args: Value,
    ) -> Result<T, serde_json::Error> {
        match serde_json::from_value::<T>(args.clone()) {
            Ok(value) => Ok(value),
            Err(first_err) => {
                let Value::Object(mut map) = args else {
                    return Err(first_err);
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
                    Err(first_err)
                }
            }
        }
    }

    async fn handle_initialize(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let initialize: Initialize = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid Initialize parameters: {e}"), None)
        })?;

        match crate::tools::initialize::handle_tool_call(&self.bash_state, initialize).await {
            Ok(result) => {
                self.persist_state().await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(McpError::internal_error(format!("Initialize failed: {e}"), None)),
        }
    }

    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let bash_command: BashCommand = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(
                format!(
                    "Invalid BashCommand parameters: {e}. Accepted forms include {{\"action_json\": {{\"command\": \"pwd\"}}}}, {{\"command\": \"pwd\"}}, or {{\"action_json\": {{\"type\": \"status_check\", \"status_check\": true}}}}."
                ),
                None,
            )
        })?;

        match crate::tools::bash_command::handle_tool_call(&self.bash_state, bash_command).await {
            Ok(output) => {
                self.persist_state().await;
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(McpError::internal_error(format!("BashCommand failed: {e}"), None)),
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_files: ReadFiles = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {e}"), None)
        })?;

        match crate::tools::read_files::handle_tool_call(&self.bash_state, read_files).await {
            Ok(result) => {
                self.persist_state().await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(McpError::internal_error(format!("ReadFiles failed: {e}"), None)),
        }
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let file_write_or_edit: FileWriteOrEdit = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid FileWriteOrEdit parameters: {e}"), None)
        })?;

        match crate::tools::file_write_or_edit::handle_tool_call(
            &self.bash_state,
            file_write_or_edit,
        )
        .await
        {
            Ok(result) => {
                self.persist_state().await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(McpError::internal_error(format!("FileWriteOrEdit failed: {e}"), None)),
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let context_save: ContextSave = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ContextSave parameters: {e}"), None)
        })?;

        match crate::tools::context_save::handle_tool_call(&self.bash_state, context_save).await {
            Ok(result) => {
                self.persist_state().await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(McpError::internal_error(format!("ContextSave failed: {e}"), None)),
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_image: ReadImage = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadImage parameters: {e}"), None)
        })?;

        match crate::tools::read_image::handle_tool_call(&self.bash_state, read_image).await {
            Ok((mime_type, base64_data)) => {
                self.persist_state().await;
                // Return a real image content block (not base64 as text) so the
                // model can actually see the image. rmcp's `Content::image`
                // takes (data, mime_type).
                Ok(CallToolResult::success(vec![Content::image(base64_data, mime_type)]))
            }
            Err(e) => Err(McpError::internal_error(format!("ReadImage failed: {e}"), None)),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server");
    let service = WinxService::new();
    let server = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
