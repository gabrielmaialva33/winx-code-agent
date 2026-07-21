//! Winx MCP Server implementation using rmcp 1.8
//! Core MCP tools only - High performance shell and file management

use rand::RngExt;
use rmcp::{
    model::{
        Annotated, CallToolRequestParams, CallToolResult, CancelTaskParams, CancelTaskResult,
        Content, CreateTaskResult, GetPromptRequestParams, GetPromptResult, GetTaskInfoParams,
        GetTaskPayloadResult, GetTaskResult, GetTaskResultParams, Implementation,
        ListPromptsResult, ListResourcesResult, ListTasksResult, ListToolsResult,
        PaginatedRequestParams, Prompt, PromptMessage, PromptMessageRole, ProtocolVersion,
        RawResource, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        ServerCapabilities, ServerInfo, Task, TaskStatus, TaskSupport, TasksCapability, Tool,
        ToolAnnotations, ToolExecution,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::schema_for;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::errors::WinxError;
use crate::state::bash_state::generate_thread_id;
use crate::state::task_state::{requested_ttl, TaskEntry, TaskRegistry};
use crate::state::BashState;
use crate::types::{
    normalize_thread_id, BashCommand, BashCommandAction, CodeMap, CodeMapStructuredOutput,
    ContextSave, FileWriteOrEdit, Initialize, InitializeType, ModeName, MultiFileEdit, ReadFiles,
    ReadImage, UndoEdit,
};

/// Map a domain [`WinxError`] to the right JSON-RPC error kind.
///
/// Client-caused failures (bad params, security violations, mode restrictions,
/// state-not-initialized, ambiguous/not-found edits) become `invalid_request`
/// so the model knows the fix is on its side; genuine server-side failures
/// (PTY spawn, IO, lock poisoning, persistence) stay `internal_error`.
fn to_mcp_error(tool: &str, err: &WinxError) -> McpError {
    let msg = format!("{tool} failed: {err}");
    // Exhaustive on purpose — NO wildcard arm. A new `WinxError` variant must be
    // classified by hand (client vs server) or the build breaks. This is the
    // guard that keeps the JSON-RPC error codes honest over time.
    match err {
        // Client-caused: the model can fix its own input or usage. -> invalid_request
        WinxError::BashStateNotInitialized
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
        | WinxError::CommandAlreadyRunning { .. } => McpError::invalid_request(msg, None),
        // Server-caused: genuine internal faults the model cannot fix. -> internal_error
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
        | WinxError::FileError(_) => McpError::internal_error(msg, None),
    }
}

/// Type alias for the shared bash state - uses `tokio::sync::Mutex` for async safety
pub type SharedBashState = Arc<Mutex<Option<BashState>>>;

/// Helper function to create JSON schema from schemars Schema
fn schema_to_input_schema<T: schemars::JsonSchema>() -> Arc<serde_json::Map<String, Value>> {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or(Value::Object(serde_json::Map::new()));
    // schemars stamps a redundant `title` (usually just the type/field name) on
    // every schema node; the LLM pays tokens for it on every tool call for zero
    // signal. Strip it — context-aware, so a user data field literally named
    // "title" is never touched.
    strip_schema_titles(&mut value);
    strip_unsupported_schema_formats(&mut value);
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Remove integer formats that are valid JSON Schema annotations but rejected
/// by some MCP clients (notably Claude Code's schema compiler).
fn strip_unsupported_schema_formats(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("format").and_then(Value::as_str) == Some("uint") {
                map.remove("format");
            }
            for child in map.values_mut() {
                strip_unsupported_schema_formats(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_unsupported_schema_formats(item);
            }
        }
        _ => {}
    }
}

/// Recursively remove `title` keys from JSON-Schema nodes only.
///
/// A dict is treated as a schema node when it carries a schema-shaped key
/// (`type`/`$ref`/`properties`/`items`/`enum`/`const`/`anyOf`/`allOf`/`oneOf`/
/// `additionalProperties`). This mirrors wcgw's `recursive_purge_dict_key` so a
/// property whose *name* is "title" keeps its value.
fn strip_schema_titles(value: &mut Value) {
    match value {
        Value::Object(map) => {
            const SCHEMA_KEYS: &[&str] = &[
                "type",
                "$ref",
                "properties",
                "items",
                "additionalProperties",
                "enum",
                "const",
                "anyOf",
                "allOf",
                "oneOf",
            ];
            if SCHEMA_KEYS.iter().any(|k| map.contains_key(*k)) {
                map.remove("title");
            }
            for child in map.values_mut() {
                strip_schema_titles(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_schema_titles(item);
            }
        }
        _ => {}
    }
}

fn mcp_tool<T: schemars::JsonSchema>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool {
    Tool::new(name, description, schema_to_input_schema::<T>()).with_annotations(annotations)
}

fn with_output_schema<T: schemars::JsonSchema>(mut tool: Tool) -> Tool {
    tool.output_schema = Some(schema_to_input_schema::<T>());
    tool
}

const INITIALIZE_DESCRIPTION: &str =
    "- Call this at the start of the conversation before using shell tools, unless a local MCP client supplied Roots and Winx initialized that workspace automatically. \
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
     - On running a bg command you'll get a bg command id that you should use to get status or interact. \
     - MCP Tasks are also supported for a single foreground `command`: send the protocol-level `task` object and an explicit `thread_id`; poll with tasks/get and fetch the final CallToolResult with tasks/result. Do not combine an MCP Task with `is_background=true`. \
     - Piloting an interactive full-screen TUI (the `claude` CLI, vim, htop, fzf, a REPL)? Run it in the background, then drive it with these two actions: \
     - `screen` ({\"screen\":true,\"bg_command_id\":\"...\",\"lines\":N,\"diff\":true}) returns a STABLE snapshot of the live terminal screen (cursor moves, redraws, alternate-screen and synchronized-output already applied; ANSI stripped), with the cursor position in the header. Use this to read the current frame — unlike `status_check`, it never stacks redraw generations and never waits. Pass \"diff\":true to get back ONLY the lines that changed since your last `screen` look (large token savings when polling a TUI frame-by-frame; first look or a big change still returns the full frame). \
     - `wait_for_turn` ({\"wait_for_turn\":true,\"bg_command_id\":\"...\",\"recognizer\":\"auto|claude|codex|antigravity|generic\",\"quiet_ms\":600,\"timeout_seconds\":30}) waits for the TUI's turn and returns the stable snapshot plus the detected state (busy / awaiting_input / awaiting_approval). By default it returns as soon as it confirms the app is actively working (state=busy) so a long-running child never pins you for the whole timeout — poll again to keep watching; pass \"wait_through_busy\":true to instead block through busy until it is ready for input (or the timeout fires). Typical REPL loop: run the app in bg -> wait_for_turn until awaiting_input -> send_text(submit:true) -> wait_for_turn -> screen, repeat.";

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
     Preserve leading spaces and indentations in both SEARCH and REPLACE blocks. \
     If a short block would match multiple places, anchor it to a line number from ReadFiles instead of padding with context: write the marker as \"<<<<<<< SEARCH @42\" (or a range \"@42-50\") to target that 1-based location. A stale anchor falls back to the normal search, so it never makes a valid edit fail.";

const MULTI_FILE_EDIT_DESCRIPTION: &str =
    "- Edits SEVERAL files together, all-or-nothing. Use this over multiple FileWriteOrEdit calls when a change spans files and a partial apply would be bad (e.g. rename a symbol across files). \
     - Every file's edit is validated and computed in memory FIRST; only if ALL succeed is anything written, so a SEARCH that fails to match in the last file leaves the earlier files untouched. \
     - Each entry has the same fields as FileWriteOrEdit: file_path (absolute, ~ allowed), percentage_to_change, and text_or_search_replace_blocks. Each file must have been read with ReadFiles first. \
     - Provide 2+ files; for a single file use FileWriteOrEdit. Do not list the same file twice. \
     - If a write fails mid-batch (rare: disk/permissions), it stops and reports which files were already written; those are not rolled back.";

const UNDO_EDIT_DESCRIPTION: &str =
    "- Reverts a file to the content it had before the last FileWriteOrEdit/MultiFileEdit you made to it THIS session. \
     - Use this to back out a wrong edit instead of re-typing the old content. \
     - Per-file: call it again on the same file to walk further back through its edits (the last ~10 edits per session are kept, in memory only). \
     - Refused if the file changed on disk since your edit (so an undo never discards newer changes), and a brand-new file's creation cannot be undone (no prior content) - use BashCommand rm for that. \
     - Provide file_path (absolute, ~ allowed).";

const CONTEXT_SAVE_DESCRIPTION: &str =
    "Saves provided description and file contents of all the relevant file paths or globs in a single text file. \
     - Provide random 3 word unqiue id or whatever user provided. \
     - Leave project path as empty string if no project path";

const CODE_MAP_DESCRIPTION: &str =
    "- Navigate code structure via tree-sitter - the semantic layer plain grep/rg can't give you. Pick an `operation`: \
     - operation=\"outline\": map symbols (functions, types, methods, classes, ...). `path` to a FILE returns that file's definitions; `path` to a DIRECTORY (or empty = the whole workspace) returns a relevance-ranked, token-budgeted symbol map across files. Use it instead of reading whole files just to learn their shape. \
     - operation=\"references\": find where a symbol is defined and referenced (called/used), by name. `name` is required (exact identifier). Counts only real symbol occurrences, never matches inside strings or comments. Output lists definitions first, then references, as `def|ref  file:line  kind  name`. \
     - Scope either operation with `path` (file or directory; empty = whole workspace); cap with `max_results`. gitignore-aware, workspace-confined, works in every mode. \
     - 11 languages (rust, js/ts, go, c, c++, java, ruby, c#, php, lua); other files return no symbols. Note: C/C++ grammars tag definitions only, so references reads 0 for `.c`/`.h`/`.cpp`. \
     - For plain-text/regex search or file discovery, use rg / grep / fd / find via BashCommand.";

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
        )
        .with_execution(ToolExecution::new().with_task_support(TaskSupport::Optional)),
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
        mcp_tool::<MultiFileEdit>(
            "MultiFileEdit",
            MULTI_FILE_EDIT_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(false),
        ),
        mcp_tool::<UndoEdit>(
            "UndoEdit",
            UNDO_EDIT_DESCRIPTION,
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
        with_output_schema::<CodeMapStructuredOutput>(mcp_tool::<CodeMap>(
            "CodeMap",
            CODE_MAP_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        )),
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

/// Upper bound on concurrently-live sessions. Each session owns a PTY (a real
/// bash process), so we evict the least-recently-used one past this to avoid
/// leaking shells across many short-lived `thread_id`s.
const MAX_SESSIONS: usize = 32;

/// Per-`thread_id` shell sessions. Each `thread_id` gets its own
/// `BashState`/PTY, so concurrent threads (or HTTP clients sharing the service)
/// never execute in each other's shell. Tools that don't carry a `thread_id`
/// (legacy clients) fall back to the most recently active session.
#[derive(Default)]
struct SessionRegistry {
    slots: HashMap<String, SharedBashState>,
    /// Last-use timestamps for LRU eviction.
    last_used: HashMap<String, Instant>,
    /// In-flight operation count per session. A session whose count is > 0 (it
    /// has a live [`SessionGuard`]) is never chosen as an LRU eviction victim, so
    /// a long-running call can't have its shell pulled out from under it.
    in_flight: HashMap<String, SessionPin>,
    /// Most recently addressed session, used as the fallback for tool calls
    /// that omit a `thread_id`.
    last_active: Option<String>,
}

/// Lock-free pin counter for one session. A live [`SessionGuard`] keeps the
/// count `> 0`, which marks the session as in-flight so LRU eviction skips it.
/// Clones share the same counter — the registry hands a clone to each
/// concurrent call. The acquire/release discipline (the guard's `Drop` runs
/// *outside* the registry lock) is loom model-checked by `loom_tests`, which
/// mirrors this exact logic; keep the two in lockstep.
#[derive(Clone)]
struct SessionPin {
    count: Arc<AtomicUsize>,
}

impl Default for SessionPin {
    fn default() -> Self {
        Self { count: Arc::new(AtomicUsize::new(0)) }
    }
}

impl SessionPin {
    /// Mark a new operation in flight, returning the RAII guard that releases it.
    fn acquire(&self) -> SessionGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        SessionGuard { count: self.count.clone() }
    }

    /// Whether any operation is currently in flight on this session.
    fn is_pinned(&self) -> bool {
        self.count.load(Ordering::SeqCst) > 0
    }
}

/// RAII marker that a session has an operation in flight. Bumps the session's
/// in-flight counter on creation and drops it on `Drop` — a plain synchronous
/// `Drop`, so it works even though the registry behind it lives in an async
/// `tokio::Mutex`. While any guard is alive the session is pinned against LRU
/// eviction.
struct SessionGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How an empty `thread_id` is resolved by the session registry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionIsolation {
    /// Local stdio transport — a single client. An empty `thread_id` falls back
    /// to the last-active session (legacy single-client convenience).
    Lenient,
    /// Remote HTTP transport — multiple clients behind one shared bearer token.
    /// The last-active fallback is disabled, so an empty `thread_id` from one
    /// client can never resolve to another client's shell; such calls get a
    /// dedicated anonymous slot instead. Clients must use the `thread_id` that
    /// `Initialize` hands back.
    ///
    /// Residual: two clients that deliberately send the *same* explicit
    /// `thread_id` still share a shell. Closing that needs per-client tokens,
    /// which the single shared-token model intentionally doesn't provide.
    Strict,
}

/// Winx service. Holds the per-thread session registry.
#[derive(Clone)]
pub struct WinxService {
    sessions: Arc<Mutex<SessionRegistry>>,
    tasks: Arc<Mutex<TaskRegistry>>,
    root_bootstrap: Arc<Mutex<()>>,
    /// Version information for the service
    pub version: String,
    /// How empty `thread_id`s are resolved (see [`SessionIsolation`]).
    isolation: SessionIsolation,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new `WinxService` for the local stdio transport (lenient,
    /// single-client session isolation).
    pub fn new() -> Self {
        Self::with_isolation(SessionIsolation::Lenient)
    }

    /// Create a `WinxService` with an explicit session-isolation policy. The
    /// HTTP transport uses [`SessionIsolation::Strict`].
    pub fn with_isolation(isolation: SessionIsolation) -> Self {
        info!(?isolation, "Creating new WinxService instance");
        Self {
            sessions: Arc::new(Mutex::new(SessionRegistry::default())),
            tasks: Arc::new(Mutex::new(TaskRegistry::default())),
            root_bootstrap: Arc::new(Mutex::new(())),
            version: env!("CARGO_PKG_VERSION").to_string(),
            isolation,
        }
    }

    /// Resolve the session slot for a `thread_id`, creating it if absent.
    ///
    /// An empty `thread_id` resolves, under [`SessionIsolation::Lenient`], to the
    /// most recently active session (the compatibility path for tools — and older
    /// clients — that don't send one); under [`SessionIsolation::Strict`] it gets
    /// a dedicated anonymous slot so remote clients can't land in each other's shell.
    /// Marks the slot as most-recently-used and evicts the LRU session when over
    /// [`MAX_SESSIONS`].
    async fn session_for(&self, thread_id: &str) -> (SharedBashState, SessionGuard) {
        let mut reg = self.sessions.lock().await;
        let key = if thread_id.is_empty() {
            match self.isolation {
                // Single local client: reuse the most recently active shell.
                SessionIsolation::Lenient => {
                    reg.last_active.clone().unwrap_or_else(|| "default".to_string())
                }
                // Many remote clients sharing one token: never resolve to
                // someone else's active shell. Empty thread_id gets its own slot.
                SessionIsolation::Strict => "anonymous".to_string(),
            }
        } else {
            thread_id.to_string()
        };

        // Evict the LRU session if adding a brand-new key would exceed the cap —
        // but never evict a session with an operation in flight (that would pull
        // the shell out from under a concurrent long-running call). If every other
        // session is busy we briefly exceed the cap rather than corrupt one.
        if !reg.slots.contains_key(&key) && reg.slots.len() >= MAX_SESSIONS {
            let victim = reg
                .last_used
                .iter()
                .filter(|(k, _)| **k != key)
                .filter(|(k, _)| reg.in_flight.get(k.as_str()).map_or(true, |p| !p.is_pinned()))
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone());
            if let Some(victim) = victim {
                reg.slots.remove(&victim);
                reg.last_used.remove(&victim);
                reg.in_flight.remove(&victim);
                if reg.last_active.as_deref() == Some(victim.as_str()) {
                    reg.last_active = None;
                }
                warn!("Evicted LRU shell session '{victim}' (session cap {MAX_SESSIONS})");
            } else {
                warn!(
                    "All {MAX_SESSIONS} sessions busy; exceeding the cap rather than evicting an in-flight session"
                );
            }
        }

        let slot =
            reg.slots.entry(key.clone()).or_insert_with(|| Arc::new(Mutex::new(None))).clone();
        let pin = reg.in_flight.entry(key.clone()).or_default().clone();
        reg.last_used.insert(key.clone(), Instant::now());
        if !thread_id.is_empty() {
            reg.last_active = Some(key);
        }
        (slot, pin.acquire())
    }

    /// The most recently active session slot, without creating one. Used by
    /// session-agnostic surfaces (e.g. the handoff prompt).
    async fn active_slot(&self) -> Option<SharedBashState> {
        let reg = self.sessions.lock().await;
        reg.last_active.as_ref().and_then(|key| reg.slots.get(key).cloned())
    }

    /// Run one task-augmented foreground shell command through to completion.
    /// Normal `BashCommand` calls intentionally yield after a short wait; an MCP
    /// Task keeps polling that same PTY and exposes one final `CallToolResult`.
    async fn run_bash_task(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .clone()
            .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
        let mut bash: BashCommand =
            serde_json::from_value(Value::Object(args)).map_err(|error| {
                McpError::invalid_request(
                    format!("Invalid BashCommand task parameters: {error}"),
                    None,
                )
            })?;
        let BashCommandAction::Command { is_background, .. } = &bash.action_json else {
            return Err(McpError::invalid_request(
                "MCP Tasks support BashCommand's foreground command action only",
                None,
            ));
        };
        if *is_background {
            return Err(McpError::invalid_request(
                "Do not combine an MCP Task with BashCommand is_background=true",
                None,
            ));
        }

        let thread_id = normalize_thread_id(&bash.thread_id);
        if thread_id.is_empty() {
            return Err(McpError::invalid_request(
                "Task-augmented BashCommand requires an explicit thread_id",
                None,
            ));
        }
        bash.thread_id.clone_from(&thread_id);
        bash.wait_for_seconds = Some(20.0);
        request.arguments = Some(task_arguments(&bash)?);
        request.task = None;

        let mut output = String::new();
        loop {
            let result = ServerHandler::call_tool(self, request, context.clone()).await?;
            let chunk = result
                .content
                .iter()
                .filter_map(|content| content.raw.as_text().map(|text| text.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            append_task_output(&mut output, &chunk);
            if !chunk.contains("status = still running") {
                return Ok(CallToolResult::success(vec![Content::text(output)]));
            }

            let status = BashCommand {
                action_json: BashCommandAction::StatusCheck {
                    status_check: true,
                    bg_command_id: None,
                    scrollback_lines: None,
                    verbose: false,
                },
                wait_for_seconds: Some(20.0),
                thread_id: thread_id.clone(),
            };
            request =
                CallToolRequestParams::new("BashCommand").with_arguments(task_arguments(&status)?);
        }
    }

    async fn interrupt_task_thread(&self, thread_id: &str) {
        let slot = {
            let registry = self.sessions.lock().await;
            registry.slots.get(thread_id).cloned()
        };
        let Some(slot) = slot else { return };
        let shell = {
            let state = slot.lock().await;
            state.as_ref().map(|state| state.pty_shell.clone())
        };
        let Some(shell) = shell else { return };
        {
            let mut guard = shell.lock().await;
            if let Some(pty) = guard.as_mut() {
                if let Err(error) = pty.send_interrupt() {
                    warn!(thread_id, %error, "failed to interrupt cancelled MCP task");
                    return;
                }
            }
        }

        // Ctrl-C is asynchronous. Drain until the shell's prompt marker arrives
        // before tasks/cancel returns, otherwise the very next tool call sees
        // the stale `command_running=true` flag and reports a false conflict.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let recovered = {
                let mut guard = shell.lock().await;
                match guard.as_mut() {
                    Some(pty) => pty.poll_output_nonblocking() || !pty.command_running,
                    None => true,
                }
            };
            if recovered {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        {
            let guard = shell.lock().await;
            if guard.as_ref().is_some_and(|pty| pty.command_running) {
                warn!(thread_id, "cancelled MCP task did not return to a prompt within 3 seconds");
            }
        }
    }

    async fn has_initialized_session(&self) -> bool {
        let slots = {
            let registry = self.sessions.lock().await;
            registry.slots.values().cloned().collect::<Vec<_>>()
        };
        for slot in slots {
            if slot.lock().await.as_ref().is_some_and(|state| state.initialized) {
                return true;
            }
        }
        false
    }

    /// Bootstrap a local single-client session from the client's first MCP Root.
    /// Roots are part of stable 2025-11-25, although rmcp 1.8 marks the API as
    /// deprecated in anticipation of the later SEP-2577 draft.
    #[allow(deprecated)]
    async fn initialize_from_client_roots(&self, context: NotificationContext<RoleServer>) {
        if self.isolation != SessionIsolation::Lenient {
            return;
        }
        let _bootstrap_guard = self.root_bootstrap.lock().await;
        if self.has_initialized_session().await {
            return;
        }
        let supports_roots =
            context.peer.peer_info().is_some_and(|info| info.capabilities.roots.is_some());
        if !supports_roots {
            return;
        }
        let roots = match context.peer.list_roots().await {
            Ok(result) => result.roots,
            Err(error) => {
                warn!(%error, "client advertised Roots but roots/list failed");
                return;
            }
        };
        let cwd = std::env::current_dir().ok();
        let mut paths =
            roots.iter().filter_map(|root| root_uri_to_path(&root.uri)).collect::<Vec<_>>();
        let selected = cwd
            .as_ref()
            .and_then(|cwd| paths.iter().position(|path| cwd.starts_with(path)))
            .map(|index| paths.remove(index))
            .or_else(|| paths.into_iter().next());
        let Some(workspace) = selected else {
            warn!("client returned no usable local file Roots; Initialize remains required");
            return;
        };

        let initialize = Initialize {
            init_type: InitializeType::FirstCall,
            any_workspace_path: workspace.to_string_lossy().into_owned(),
            initial_files_to_read: Vec::new(),
            task_id_to_resume: String::new(),
            mode_name: ModeName::Wcgw,
            thread_id: generate_thread_id(),
            code_writer_config: None,
        };
        let arguments = match serde_json::to_value(initialize) {
            Ok(arguments) => arguments,
            Err(error) => {
                warn!(%error, "failed to serialize Roots bootstrap request");
                return;
            }
        };
        match self.handle_initialize(Some(arguments)).await {
            Ok(_) => info!(workspace = %workspace.display(), "initialized Winx from MCP Roots"),
            Err(error) => warn!(%error.message, "failed to initialize Winx from MCP Roots"),
        }
    }
}

const MAX_TASK_RUNTIME: Duration = Duration::from_secs(60 * 60);
const MAX_TASK_OUTPUT_BYTES: usize = 1_000_000;

fn task_arguments<T: serde::Serialize>(
    value: &T,
) -> Result<serde_json::Map<String, Value>, McpError> {
    match serde_json::to_value(value) {
        Ok(Value::Object(arguments)) => Ok(arguments),
        Ok(_) => Err(McpError::internal_error("Task arguments were not an object", None)),
        Err(error) => Err(McpError::internal_error(
            format!("Failed to serialize task arguments: {error}"),
            None,
        )),
    }
}

fn append_task_output(output: &mut String, chunk: &str) {
    if !output.is_empty() && !chunk.is_empty() {
        output.push('\n');
    }
    output.push_str(chunk);
    if output.len() <= MAX_TASK_OUTPUT_BYTES {
        return;
    }
    let cut = crate::utils::floor_char_boundary(output, output.len() - MAX_TASK_OUTPUT_BYTES);
    output.replace_range(..cut, "(...earlier task output truncated...)\n");
}

fn root_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let encoded = if let Some(path) = encoded.strip_prefix("localhost/") {
        format!("/{path}")
    } else if encoded.starts_with('/') {
        encoded.to_string()
    } else {
        // Refuse non-local file authorities (file://host/path).
        return None;
    };
    let decoded = percent_encoding::percent_decode_str(&encoded).decode_utf8().ok()?;
    let path = std::path::PathBuf::from(decoded.as_ref());
    path.is_absolute().then_some(path)
}

/// `ServerHandler` implementation
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_tasks_with(TasksCapability::server_default())
                .build(),
        )
        .with_server_info(
            Implementation::new("winx-mcp-server", self.version.clone())
                .with_title("Winx High-Performance MCP"),
        )
        // Tasks were added in the stable 2025-11-25 revision. rmcp negotiates
        // with older clients during initialize, so raising the server ceiling
        // preserves compatibility with clients on earlier revisions.
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .with_instructions(
                "Winx is a high-performance Rust implementation of MCP tools for shell and file management."
        )
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.initialize_from_client_roots(context).await;
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        // A changed Roots list is only used as a retry if Winx is still
        // uninitialized. Never switch an active shell's workspace implicitly.
        self.initialize_from_client_roots(context).await;
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        winx_tools().into_iter().find(|tool| tool.name == name)
    }

    async fn enqueue_task(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CreateTaskResult, McpError> {
        if request.name != "BashCommand" {
            return Err(McpError::invalid_request(
                "Only BashCommand advertises MCP Task support",
                None,
            ));
        }
        let args = request
            .arguments
            .clone()
            .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
        let bash: BashCommand = serde_json::from_value(Value::Object(args)).map_err(|error| {
            McpError::invalid_request(format!("Invalid BashCommand task parameters: {error}"), None)
        })?;
        let BashCommandAction::Command { is_background, .. } = bash.action_json else {
            return Err(McpError::invalid_request(
                "MCP Tasks support BashCommand's foreground command action only",
                None,
            ));
        };
        if is_background {
            return Err(McpError::invalid_request(
                "Do not combine an MCP Task with BashCommand is_background=true",
                None,
            ));
        }
        let thread_id = normalize_thread_id(&bash.thread_id);
        if thread_id.is_empty() {
            return Err(McpError::invalid_request(
                "Task-augmented BashCommand requires an explicit thread_id",
                None,
            ));
        }

        let task_id = format!("task_{:032x}", rand::rng().random::<u128>());
        let ttl = requested_ttl(request.task.as_ref());
        let now = rmcp::task_manager::current_timestamp();
        let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
            .with_status_message("Running BashCommand")
            .with_ttl(ttl)
            .with_poll_interval(1_000);
        self.tasks
            .lock()
            .await
            .insert(task_id.clone(), TaskEntry::working(task.clone(), thread_id.clone()))
            .map_err(|message| McpError::internal_error(message, None))?;

        let service = self.clone();
        let worker_task_id = task_id.clone();
        let handle = tokio::spawn(async move {
            let outcome =
                tokio::time::timeout(MAX_TASK_RUNTIME, service.run_bash_task(request, context))
                    .await;

            let (status, message, result) = match outcome {
                Ok(Ok(result)) => match serde_json::to_value(result) {
                    Ok(result) => (
                        TaskStatus::Completed,
                        Some("BashCommand completed".to_string()),
                        Some(result),
                    ),
                    Err(error) => (
                        TaskStatus::Failed,
                        Some(format!("Failed to serialize task result: {error}")),
                        None,
                    ),
                },
                Ok(Err(error)) => (
                    TaskStatus::Failed,
                    Some(crate::utils::redact::redact(&error.message).into_owned()),
                    None,
                ),
                Err(_) => {
                    service.interrupt_task_thread(&thread_id).await;
                    (
                        TaskStatus::Failed,
                        Some("BashCommand task exceeded the one-hour runtime limit".to_string()),
                        None,
                    )
                }
            };

            let mut tasks = service.tasks.lock().await;
            if let Some(entry) = tasks.get_mut(&worker_task_id) {
                // Cancellation is terminal. A worker racing with tasks/cancel
                // must never overwrite Cancelled with Completed.
                if entry.task.status == TaskStatus::Working {
                    entry.finish(status, message, result);
                }
            }
        });
        let abort_handle = handle.abort_handle();
        drop(handle);
        if let Some(entry) = self.tasks.lock().await.get_mut(&task_id) {
            if entry.task.status == TaskStatus::Working {
                entry.abort_handle = Some(abort_handle);
            }
        }

        Ok(CreateTaskResult::new(task))
    }

    async fn list_tasks(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListTasksResult, McpError> {
        let tasks = self.tasks.lock().await.list();
        let total = tasks.len() as u64;
        let mut result = ListTasksResult::new(tasks);
        result.total = Some(total);
        Ok(result)
    }

    async fn get_task_info(
        &self,
        request: GetTaskInfoParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.get(&request.task_id).ok_or_else(|| {
            McpError::invalid_request(format!("Unknown or expired task: {}", request.task_id), None)
        })?;
        Ok(GetTaskResult { meta: None, task: entry.task.clone() })
    }

    async fn get_task_result(
        &self,
        request: GetTaskResultParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskPayloadResult, McpError> {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.get(&request.task_id).ok_or_else(|| {
            McpError::invalid_request(format!("Unknown or expired task: {}", request.task_id), None)
        })?;
        match entry.task.status {
            TaskStatus::Completed => entry
                .result
                .clone()
                .map(GetTaskPayloadResult::new)
                .ok_or_else(|| McpError::internal_error("Completed task has no result", None)),
            TaskStatus::Working | TaskStatus::InputRequired => Err(McpError::invalid_request(
                format!("Task {} is not complete", request.task_id),
                None,
            )),
            TaskStatus::Failed | TaskStatus::Cancelled => Err(McpError::invalid_request(
                entry
                    .task
                    .status_message
                    .clone()
                    .unwrap_or_else(|| format!("Task {} did not complete", request.task_id)),
                None,
            )),
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CancelTaskResult, McpError> {
        let (abort_handle, thread_id, task) = {
            let mut tasks = self.tasks.lock().await;
            let entry = tasks.get_mut(&request.task_id).ok_or_else(|| {
                McpError::invalid_request(
                    format!("Unknown or expired task: {}", request.task_id),
                    None,
                )
            })?;
            if entry.task.status != TaskStatus::Working
                && entry.task.status != TaskStatus::InputRequired
            {
                return Err(McpError::invalid_request(
                    format!("Task {} is already terminal", request.task_id),
                    None,
                ));
            }
            let abort_handle = entry.abort_handle.take();
            let thread_id = entry.thread_id.clone();
            entry.finish(TaskStatus::Cancelled, Some("Cancelled by client".to_string()), None);
            (abort_handle, thread_id, entry.task.clone())
        };
        if let Some(abort_handle) = abort_handle {
            abort_handle.abort();
        }
        self.interrupt_task_thread(&thread_id).await;
        Ok(CancelTaskResult { meta: None, task })
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

        // This prompt embeds git status/diff and file context, so scrub it too —
        // it returns a GetPromptResult and bypasses the call_tool choke point.
        let text =
            crate::utils::redact::redact(&self.knowledge_transfer_prompt_text().await).into_owned();

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
                Ok(content) => vec![ResourceContents::text(
                    crate::utils::redact::redact(&content).into_owned(),
                    param.uri.clone(),
                )],
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
        // Redact the audit line too: it embeds tool arguments (a BashCommand's
        // command line, a file path) that can carry secrets into the logs.
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

        // Scrub credentials (provider keys, JWTs, PEM blocks, user:pass URLs)
        // from everything leaving the server — shell output, file contents,
        // search results — before it reaches the model. `WINX_NO_REDACT` opts out.
        let result = match result {
            Ok(mut call) => {
                redact_result(&mut call);
                Ok(call)
            }
            Err(mut error) => {
                // The error message is returned to the client/model verbatim and
                // often echoes a path, env value, or command output that can carry
                // a secret — scrub it the same as success content.
                error.message = crate::utils::redact::redact(&error.message).into_owned().into();
                Err(error)
            }
        };

        let ms = started.elapsed().as_millis();
        match &result {
            Ok(_) => info!(tool = %tool, ms, "tool call ok — {summary}"),
            // error.message was already redacted above.
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
    match tool {
        "BashCommand" => {
            let action = args.get("action_json");
            let cmd = action
                .and_then(|a| a.get("command"))
                .and_then(Value::as_str)
                .or_else(|| args.get("command").and_then(Value::as_str));
            if let Some(cmd) = cmd {
                format!("command bytes={}", cmd.len())
            } else {
                let kind =
                    action.and_then(|a| a.get("type")).and_then(Value::as_str).unwrap_or("?");
                format!("action={kind}")
            }
        }
        "FileWriteOrEdit" | "ReadImage" | "UndoEdit" => format!("path={}", s("file_path")),
        "MultiFileEdit" => {
            format!("files={}", args.get("files").and_then(Value::as_array).map_or(0, Vec::len))
        }
        "ReadFiles" => {
            format!(
                "files={}",
                args.get("file_paths").and_then(Value::as_array).map_or(0, Vec::len)
            )
        }
        "Initialize" => format!("ws={} mode={}", s("any_workspace_path"), s("mode_name")),
        "ContextSave" => format!("id={}", s("id")),
        "CodeMap" => format!("op={} path={} name={}", s("operation"), s("path"), s("name")),
        _ => String::new(),
    }
}

/// Scrub credential patterns from a tool result's text blocks and structured
/// content, in place. Central choke point so every tool is covered (see
/// [`crate::utils::redact`]).
fn redact_result(result: &mut CallToolResult) {
    use rmcp::model::RawContent;
    for content in &mut result.content {
        if let RawContent::Text(text) = &mut content.raw {
            if let std::borrow::Cow::Owned(scrubbed) = crate::utils::redact::redact(&text.text) {
                text.text = scrubbed;
            }
        }
    }
    if let Some(structured) = result.structured_content.as_mut() {
        crate::utils::redact::redact_json(structured);
    }
}

#[cfg(test)]
mod audit_tests {
    use super::audit_summary;
    use serde_json::json;

    #[test]
    fn bash_audit_summary_never_contains_command_content() {
        let args = json!({
            "action_json": {
                "type": "command",
                "command": "curl -H 'Authorization: secret-value' https://example.invalid"
            }
        });
        let summary = audit_summary("BashCommand", Some(&args));
        assert!(summary.starts_with("command bytes="));
        assert!(!summary.contains("secret-value"));
        assert!(!summary.contains("example.invalid"));
    }
}

impl WinxService {
    async fn knowledge_transfer_prompt_text(&self) -> String {
        let mut text = String::from(
            "Prepare a concise handoff for another agent. Include active objective, current state, important files, changed files, blockers, validation already run, and exact next commands.\n",
        );

        let state_snapshot = if let Some(slot) = self.active_slot().await {
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

    async fn persist_state(&self, slot: &SharedBashState) {
        let guard = slot.lock().await;
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
        let mut initialize: Initialize = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid Initialize parameters: {e}"), None)
        })?;

        // Resolve the session key. A first_call may omit thread_id; generate one
        // here and write it back so the handler and the registry agree on the id.
        let mut thread_id = normalize_thread_id(&initialize.thread_id);
        if thread_id.is_empty() {
            thread_id = generate_thread_id();
            initialize.thread_id.clone_from(&thread_id);
        }
        let (slot, _session_guard) = self.session_for(&thread_id).await;

        match crate::tools::initialize::handle_tool_call(&slot, initialize).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("Initialize", &e)),
        }
    }

    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let mut bash_command: BashCommand = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_request(
                format!(
                    "Invalid BashCommand parameters: {e}. Accepted forms include {{\"action_json\": {{\"command\": \"pwd\"}}}}, {{\"command\": \"pwd\"}}, or {{\"action_json\": {{\"type\": \"status_check\", \"status_check\": true}}}}."
                ),
                None,
            )
        })?;

        let requested_thread_id = normalize_thread_id(&bash_command.thread_id);
        let (slot, _session_guard) = self.session_for(&requested_thread_id).await;
        if requested_thread_id.is_empty() {
            // Initialize historically returns a generated thread_id, but local
            // clients that rely on MCP Roots may not know it. Resolve an omitted
            // id to the session selected by `session_for` before the lower-level
            // BashCommand validation runs.
            if let Some(thread_id) =
                slot.lock().await.as_ref().map(|state| state.current_thread_id.clone())
            {
                bash_command.thread_id = thread_id;
            }
        }
        match crate::tools::bash_command::handle_tool_call(&slot, bash_command).await {
            Ok(output) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(to_mcp_error("BashCommand", &e)),
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_files: ReadFiles = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadFiles parameters: {e}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_files.thread_id)).await;
        match crate::tools::read_files::handle_tool_call(&slot, read_files).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("ReadFiles", &e)),
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

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&file_write_or_edit.thread_id)).await;
        match crate::tools::file_write_or_edit::handle_tool_call(&slot, file_write_or_edit).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("FileWriteOrEdit", &e)),
        }
    }

    async fn handle_multi_file_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let multi: MultiFileEdit = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid MultiFileEdit parameters: {e}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&multi.thread_id)).await;
        match crate::tools::multi_file_edit::handle_tool_call(&slot, multi).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("MultiFileEdit", &e)),
        }
    }

    async fn handle_undo_edit(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let undo: UndoEdit = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid UndoEdit parameters: {e}"), None)
        })?;

        let (slot, _session_guard) = self.session_for(&normalize_thread_id(&undo.thread_id)).await;
        match crate::tools::undo_edit::handle_tool_call(&slot, undo).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("UndoEdit", &e)),
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let context_save: ContextSave = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ContextSave parameters: {e}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&context_save.thread_id)).await;
        match crate::tools::context_save::handle_tool_call(&slot, context_save).await {
            Ok(result) => {
                self.persist_state(&slot).await;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => Err(to_mcp_error("ContextSave", &e)),
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let read_image: ReadImage = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid ReadImage parameters: {e}"), None)
        })?;

        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&read_image.thread_id)).await;
        match crate::tools::read_image::handle_tool_call(&slot, read_image).await {
            Ok((mime_type, base64_data)) => {
                self.persist_state(&slot).await;
                // Return a real image content block (not base64 as text) so the
                // model can actually see the image. rmcp's `Content::image`
                // takes (data, mime_type).
                Ok(CallToolResult::success(vec![Content::image(base64_data, mime_type)]))
            }
            Err(e) => Err(to_mcp_error("ReadImage", &e)),
        }
    }

    async fn handle_code_map(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let code_map: CodeMap = Self::lenient_from_value(args).map_err(|e| {
            McpError::invalid_request(format!("Invalid CodeMap parameters: {e}"), None)
        })?;

        // Read-only: no persist_state (nothing in the session changes).
        let (slot, _session_guard) =
            self.session_for(&normalize_thread_id(&code_map.thread_id)).await;
        match crate::tools::code_map::handle_tool_call(&slot, code_map).await {
            Ok((text, structured)) => {
                let mut result = CallToolResult::success(vec![Content::text(text)]);
                result.structured_content = Some(structured);
                Ok(result)
            }
            Err(e) => Err(to_mcp_error("CodeMap", &e)),
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

#[cfg(test)]
mod session_registry_tests {
    use super::*;

    #[tokio::test]
    async fn distinct_threads_get_distinct_sessions() {
        let svc = WinxService::new();
        let (a, _) = svc.session_for("thread_a").await;
        let (b, _) = svc.session_for("thread_b").await;
        // Two thread_ids must own two separate slots — the whole point of the
        // registry: thread B never executes in thread A's shell.
        assert!(!Arc::ptr_eq(&a, &b));
        // Same id round-trips to the same slot.
        let (a2, _) = svc.session_for("thread_a").await;
        assert!(Arc::ptr_eq(&a, &a2));
    }

    #[tokio::test]
    async fn empty_thread_id_falls_back_to_last_active() {
        let svc = WinxService::new();
        let (_a, _) = svc.session_for("thread_a").await;
        let (b, _) = svc.session_for("thread_b").await; // now last-active
                                                        // A tool call without a thread_id reuses the most recently active slot.
        let (fallback, _) = svc.session_for("").await;
        assert!(Arc::ptr_eq(&b, &fallback));
        assert!(svc.active_slot().await.is_some());
    }

    #[tokio::test]
    async fn strict_isolation_empty_thread_id_does_not_steal_active_session() {
        let svc = WinxService::with_isolation(SessionIsolation::Strict);
        let (a, _) = svc.session_for("thread_a").await; // would be "last-active" under Lenient
        let (anon, _) = svc.session_for("").await;
        // Strict mode: an empty thread_id must NOT resolve to another client's shell.
        assert!(!Arc::ptr_eq(&a, &anon));
        // Anonymous calls share one dedicated slot rather than hijacking named ones.
        let (anon2, _) = svc.session_for("").await;
        assert!(Arc::ptr_eq(&anon, &anon2));
        // Explicit thread_ids stay isolated as always.
        let (b, _) = svc.session_for("thread_b").await;
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn lru_eviction_caps_live_sessions() {
        let svc = WinxService::new();
        for i in 0..(MAX_SESSIONS + 5) {
            let (_, _) = svc.session_for(&format!("t{i}")).await;
        }
        let reg = svc.sessions.lock().await;
        assert!(reg.slots.len() <= MAX_SESSIONS, "session count {} over cap", reg.slots.len());
    }

    #[tokio::test]
    async fn in_flight_session_is_not_evicted() {
        let svc = WinxService::new();
        // "keep" is created first (so it's the LRU candidate) and held busy.
        let (_keep_slot, _keep_guard) = svc.session_for("keep").await;
        // Saturate the cap and then churn well past it; eviction must skip the
        // in-flight "keep" and evict idle fillers instead.
        for i in 0..(MAX_SESSIONS + 10) {
            let (_, _) = svc.session_for(&format!("filler{i}")).await;
        }
        let reg = svc.sessions.lock().await;
        assert!(
            reg.slots.contains_key("keep"),
            "an in-flight session must survive LRU eviction churn"
        );
    }
}

#[cfg(test)]
mod task_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn interrupt_waits_until_main_shell_is_reusable() {
        let service = WinxService::new();
        let initialized = service
            .handle_initialize(Some(serde_json::json!({
                "type": "first_call",
                "any_workspace_path": std::env::temp_dir(),
                "mode_name": "wcgw",
                "thread_id": "task_cancel_regression"
            })))
            .await;
        assert!(initialized.is_ok(), "test session failed to initialize: {initialized:?}");

        let worker = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .handle_bash_command(Some(serde_json::json!({
                        "action_json": {
                            "type": "command",
                            "command": "sleep 30",
                            "is_background": false
                        },
                        "thread_id": "task_cancel_regression"
                    })))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        worker.abort();
        service.interrupt_task_thread("task_cancel_regression").await;
        let interrupted = tokio::time::timeout(Duration::from_secs(5), worker).await;
        assert!(
            interrupted.is_ok_and(|joined| joined.is_err_and(|error| error.is_cancelled())),
            "aborted task worker did not settle"
        );

        let recovery = service
            .handle_bash_command(Some(serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "printf task-shell-recovered",
                    "is_background": false
                },
                "thread_id": "task_cancel_regression"
            })))
            .await;
        let rendered = format!("{recovery:?}");
        assert!(
            recovery.is_ok() && rendered.contains("task-shell-recovered"),
            "shell was not reusable after cancellation: {rendered}"
        );
    }
}

#[cfg(test)]
mod schema_tests {
    #![allow(clippy::expect_used)]

    use super::{root_uri_to_path, schema_to_input_schema, strip_schema_titles, winx_tools};
    use rmcp::model::{ProtocolVersion, TaskSupport};
    use rmcp::ServerHandler;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn strips_titles_from_schema_nodes_only() {
        let mut v = json!({
            "type": "object",
            "title": "ShouldGo",
            "properties": {
                // a user field literally named "title" must survive as a key,
                // and its inner schema's own title must be stripped.
                "title": { "type": "string", "title": "InnerGoes" }
            }
        });
        strip_schema_titles(&mut v);
        assert!(v.get("title").is_none(), "schema-node title not stripped");
        let props = v.get("properties").and_then(serde_json::Value::as_object);
        assert!(
            props.is_some_and(|p| p.contains_key("title")),
            "property key named 'title' must be preserved"
        );
        assert!(
            props.and_then(|p| p.get("title")).and_then(|t| t.get("title")).is_none(),
            "inner schema title not stripped"
        );
    }

    #[test]
    fn real_tool_schema_carries_no_titles() {
        let schema = schema_to_input_schema::<crate::types::Initialize>();
        let blob = serde_json::to_string(&*schema).unwrap_or_default();
        assert!(!blob.contains("\"title\""), "tool schema still contains titles: {blob}");
    }

    #[test]
    fn initialize_schema_has_no_dangling_refs() {
        // The Initialize input embeds ModeName. A manual JsonSchema impl used to
        // emit a `$ref` to `#/definitions/ModeName` that was never populated —
        // strict MCP clients (Cline, and likely ChatGPT) reject the tool call
        // with "Reference not found: #/definitions/ModeName". Guard against any
        // dangling/definitions-style ref reappearing.
        let schema = schema_to_input_schema::<crate::types::Initialize>();
        let blob = serde_json::to_string(&*schema).unwrap_or_default();
        assert!(
            !blob.contains("#/definitions/"),
            "schema has a draft-07 #/definitions ref (dangling for 2020-12 clients): {blob}"
        );
        assert!(
            !blob.contains("definitions/ModeName"),
            "ModeName is referenced but never defined: {blob}"
        );
        // mode_name must be a concrete, self-contained string enum.
        assert!(
            blob.contains("wcgw") && blob.contains("architect") && blob.contains("code_writer"),
            "mode_name enum not inlined: {blob}"
        );
    }

    #[test]
    fn advertises_stable_tasks_protocol_and_capability() {
        let info = ServerHandler::get_info(&super::WinxService::new());
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
        let tasks = info.capabilities.tasks.expect("tasks capability");
        assert!(tasks.supports_tools_call());
        assert!(tasks.supports_list());
        assert!(tasks.supports_cancel());
    }

    #[test]
    fn bash_task_support_and_code_map_output_schema_are_advertised() {
        let tools = winx_tools();
        let bash = tools.iter().find(|tool| tool.name == "BashCommand").expect("BashCommand");
        assert_eq!(bash.task_support(), TaskSupport::Optional);

        let code_map = tools.iter().find(|tool| tool.name == "CodeMap").expect("CodeMap");
        let output = code_map.output_schema.as_ref().expect("CodeMap outputSchema");
        assert!(output.get("properties").is_some_and(|properties| {
            properties.as_object().is_some_and(|properties| properties.contains_key("truncated"))
        }));
        let blob = serde_json::to_string(output).unwrap_or_default();
        assert!(!blob.contains("\"format\":\"uint\""), "unsupported uint format: {blob}");
    }

    #[test]
    fn local_root_uri_decoding_rejects_remote_authorities() {
        assert_eq!(
            root_uri_to_path("file:///tmp/a%20project"),
            Some(PathBuf::from("/tmp/a project"))
        );
        assert_eq!(
            root_uri_to_path("file://localhost/tmp/project"),
            Some(PathBuf::from("/tmp/project"))
        );
        assert_eq!(root_uri_to_path("file://remote.example/tmp/project"), None);
        assert_eq!(root_uri_to_path("https://example.com/project"), None);
    }
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;
    use rmcp::model::ErrorCode;
    use std::path::PathBuf;

    fn code_of(err: &WinxError) -> ErrorCode {
        to_mcp_error("Tool", err).code
    }

    #[test]
    fn client_caused_errors_map_to_invalid_request() {
        // A validation suggestion is the model's to fix, not a server fault.
        assert_eq!(
            code_of(&WinxError::RecoverableSuggestionError {
                message: "bad arg".into(),
                suggestion: "try x".into(),
            }),
            ErrorCode::INVALID_REQUEST,
        );
        // Parse failures come from the model's input.
        assert_eq!(
            code_of(&WinxError::ParseError("unexpected token".into())),
            ErrorCode::INVALID_REQUEST,
        );
        // A bad path the model handed us is something it can correct.
        assert_eq!(
            code_of(&WinxError::FileAccessError {
                path: PathBuf::from("/nope"),
                message: "no such file".into(),
            }),
            ErrorCode::INVALID_REQUEST,
        );
    }

    #[test]
    fn server_caused_errors_stay_internal_error() {
        assert_eq!(
            code_of(&WinxError::IoError(std::io::Error::other("disk gone"))),
            ErrorCode::INTERNAL_ERROR,
        );
        assert_eq!(
            code_of(&WinxError::BashStateLockError("poisoned".into())),
            ErrorCode::INTERNAL_ERROR,
        );
    }
}

/// Loom model-check of the [`SessionPin`] counting discipline — the one piece
/// of session state touched off the registry lock (a guard's `Drop` decrements
/// while a concurrent eviction may be reading it via `is_pinned`). Loom can't
/// model the surrounding `tokio::Mutex`, so we isolate the lock-free counter and
/// exhaustively check it under every interleaving.
///
/// `SessionPin` itself stays on std atomics: instrumenting it would force every
/// `WinxService` test (which constructs pins outside `loom::model`) to panic
/// under `--all-features`. Instead `PinModel` below mirrors `SessionPin`'s exact
/// logic — `acquire` = `fetch_add(SeqCst)`, guard `Drop` = `fetch_sub(SeqCst)`,
/// `is_pinned` = `load(SeqCst) > 0`. Keep the two in lockstep.
///
/// Built only under the `loom` feature. Run with:
///   cargo test --features loom --lib loom_
#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    use loom::sync::atomic::{AtomicUsize, Ordering};
    use loom::sync::Arc;

    #[derive(Clone)]
    struct PinModel {
        count: Arc<AtomicUsize>,
    }

    struct GuardModel {
        count: Arc<AtomicUsize>,
    }

    impl PinModel {
        fn new() -> Self {
            Self { count: Arc::new(AtomicUsize::new(0)) }
        }
        fn acquire(&self) -> GuardModel {
            self.count.fetch_add(1, Ordering::SeqCst);
            GuardModel { count: self.count.clone() }
        }
        fn is_pinned(&self) -> bool {
            self.count.load(Ordering::SeqCst) > 0
        }
    }

    impl Drop for GuardModel {
        fn drop(&mut self) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Two concurrent in-flight ops on the same session: under *every* thread
    /// interleaving each `acquire` is balanced by its guard's `Drop`, so the pin
    /// returns to exactly 0 and never underflows. An underflow would wrap the
    /// counter to a huge value, making a finished session read as permanently
    /// in-flight and leak past LRU eviction forever.
    #[test]
    fn loom_concurrent_guards_balance_to_zero() {
        loom::model(|| {
            let pin = PinModel::new();
            let p1 = pin.clone();
            let p2 = pin.clone();
            let h1 = loom::thread::spawn(move || drop(p1.acquire()));
            let h2 = loom::thread::spawn(move || drop(p2.acquire()));
            assert!(h1.join().is_ok());
            assert!(h2.join().is_ok());
            assert_eq!(pin.count.load(Ordering::SeqCst), 0, "pin must settle back to 0");
        });
    }

    /// One op stays in flight while another runs concurrently: the observer must
    /// never see the live session as unpinned (which the eviction filter would
    /// read as "safe to evict", pulling the shell out from under the running op).
    #[test]
    fn loom_live_guard_always_reads_pinned() {
        loom::model(|| {
            let pin = PinModel::new();
            let observer = pin.clone();
            let held = pin.acquire();
            // A second op starts and finishes concurrently; throughout, the
            // first guard keeps the session pinned for the observer.
            let worker = pin.clone();
            let h = loom::thread::spawn(move || drop(worker.acquire()));
            assert!(observer.is_pinned(), "session with a live guard must read pinned");
            assert!(h.join().is_ok());
            assert!(observer.is_pinned(), "still pinned while the first guard lives");
            drop(held);
            assert!(!observer.is_pinned(), "all guards gone -> unpinned");
        });
    }
}
