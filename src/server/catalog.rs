use std::sync::{Arc, OnceLock};

use rmcp::model::{Prompt, Tool, ToolAnnotations};
use schemars::schema_for;
use serde_json::Value;

use super::outcomes::{CodeMapToolResultEnvelope, ToolResultEnvelope};
use crate::tool_policy::ToolPolicy;
use crate::types::{
    BashCommand, CodeMap, ContextSave, FileWriteOrEdit, Initialize, MultiFileEdit, ReadFiles,
    ReadImage, UndoEdit,
};

/// Convert a schemars schema into the MCP tool input-schema representation.
pub(super) fn schema_to_input_schema<T: schemars::JsonSchema>(
) -> Arc<serde_json::Map<String, Value>> {
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
            let unsupported_integer_format =
                map.get("format").and_then(Value::as_str).is_some_and(|format| {
                    format.starts_with("uint")
                        || matches!(
                            format,
                            "int8" | "int16" | "int32" | "int64" | "isize" | "usize"
                        )
                });
            if unsupported_integer_format {
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
pub(super) fn strip_schema_titles(value: &mut Value) {
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
            if SCHEMA_KEYS.iter().any(|key| map.contains_key(*key)) {
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

/// `wait_policy` belongs to the MCP adapter rather than the stable public
/// `BashCommand` Rust struct. Inject it only into the advertised wire schema so
/// existing library callers do not acquire a new required struct field.
fn bash_command_input_schema() -> Arc<serde_json::Map<String, Value>> {
    let mut schema = (*schema_to_input_schema::<BashCommand>()).clone();
    let properties = schema
        .entry("properties".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(properties) = properties {
        if let Some(Value::Object(wait)) = properties.get_mut("wait_for_seconds") {
            wait.insert(
                "description".to_string(),
                Value::String(
                    "Requested inline wait in seconds. Adaptive defaults to 15 seconds without Tasks and uses a short promotion window with Tasks; return_early is capped at 5 seconds; the synchronous until_complete fallback uses 60 seconds. The selected wait_policy always supplies the final cap."
                        .to_string(),
                ),
            );
        }
        properties.insert(
            "wait_policy".to_string(),
            serde_json::json!({
                "type": "string",
                "enum": ["adaptive", "return_early", "until_complete"],
                "default": "adaptive",
                "description": "Execution delivery policy. Adaptive is bounded inline and may promote a foreground Command to an MCP Task; return_early never creates a Task; until_complete is valid only for a foreground Command, creates a Task when safely supported, and otherwise waits synchronously for at most 60 seconds."
            }),
        );
    }
    Arc::new(schema)
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
    "Use this for stateful shell and process work after Initialize. `wait_policy=adaptive` (default) keeps short commands inline and, when MCP Tasks and generation-bound runtime actions are negotiated, promotes only a foreground command the runtime already reported as running. Without safe Task support it waits synchronously for at most 60 seconds. Use `until_complete` only for a finite foreground Command: capable clients receive a Task immediately and other clients get a synchronous wait capped at 60 seconds. Use `return_early` when the caller needs prompt control; it never creates a Task and waits at most 5 seconds. `wait_for_seconds` is a request within those policy caps, not an override. Treat structuredContent.status as authoritative: when it is `running`, execute next_action (`status_check`) and never submit the original command again. Run long-lived or interactive programs with is_background=true, then use wait_for_turn/screen and send_text/send_specials. Never use shell redirection, echo, or cat for file edits/reads; use the file tools.";

const READ_FILES_DESCRIPTION: &str =
    "- Read full file content of one or more files. \
     - Prefer this over reading files with BashCommand (cat/head/tail): the output is token-budgeted and the read is recorded so FileWriteOrEdit can edit the file afterward. \
     - Do NOT use this for binary files or images — use ReadImage for images. \
     - Provide absolute paths only (~ allowed) \
     - Only if the task requires line numbers understanding: \
     - You may extract a range of lines. E.g., `/path/to/file:1-10` for lines 1-10. You can drop start or end like `/path/to/file:1-` or `/path/to/file:-10`";

const FILE_WRITE_OR_EDIT_DESCRIPTION: &str =
    "Use this to edit one file; use MultiFileEdit when a change spans files. Read the target with ReadFiles first. If structuredContent.status is `needs_read`, perform every required_read or the supplied next_action before retrying; never repeat a failed edit unchanged. For percentage_to_change <= 50, provide concise exact SEARCH/REPLACE blocks with only enough context for uniqueness. For percentage_to_change > 50, provide the complete file and ensure the whole file was read. Preserve whitespace exactly. Use `<<<<<<< SEARCH @42` or `@42-50` to disambiguate repeated snippets. A stale file, missing block, or ambiguous block requires a fresh read before a corrected retry.";

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
     - Scope either operation with `path` (file or directory; empty = the whole workspace); cap with `max_results`. gitignore-aware, workspace-confined, works in every mode. \
     - 11 languages (rust, js/ts, go, c, c++, java, ruby, c#, php, lua); other files return no symbols. Note: C/C++ grammars tag definitions only, so references reads 0 for `.c`/`.h`/`.cpp`. \
     - For plain-text/regex search or file discovery, use rg / grep / fd / find via BashCommand.";

static WINX_TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
static WINX_PROMPTS: OnceLock<Vec<Prompt>> = OnceLock::new();

pub(super) fn winx_tools() -> Vec<Tool> {
    WINX_TOOLS.get_or_init(build_winx_tools).clone()
}

pub(super) fn winx_tools_for_policy(policy: ToolPolicy) -> Vec<Tool> {
    WINX_TOOLS
        .get_or_init(build_winx_tools)
        .iter()
        .filter(|tool| policy.allows(tool.name.as_ref()))
        .cloned()
        .collect()
}

fn build_winx_tools() -> Vec<Tool> {
    vec![
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<Initialize>(
            "Initialize",
            INITIALIZE_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(
            Tool::new("BashCommand", BASH_COMMAND_DESCRIPTION, bash_command_input_schema())
                .with_annotations(ToolAnnotations::new().destructive(true).open_world(true)),
        ),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<ReadFiles>(
            "ReadFiles",
            READ_FILES_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<FileWriteOrEdit>(
            "FileWriteOrEdit",
            FILE_WRITE_OR_EDIT_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<MultiFileEdit>(
            "MultiFileEdit",
            MULTI_FILE_EDIT_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<UndoEdit>(
            "UndoEdit",
            UNDO_EDIT_DESCRIPTION,
            ToolAnnotations::new().destructive(true).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<ContextSave>(
            "ContextSave",
            CONTEXT_SAVE_DESCRIPTION,
            ToolAnnotations::new().destructive(false).open_world(false),
        )),
        with_output_schema::<ToolResultEnvelope>(mcp_tool::<ReadImage>(
            "ReadImage",
            "Read an image from the workspace. The structured result reports the resolved tool state while the image remains in MCP content.",
            ToolAnnotations::new().read_only(true).open_world(false),
        )),
        with_output_schema::<CodeMapToolResultEnvelope>(mcp_tool::<CodeMap>(
            "CodeMap",
            CODE_MAP_DESCRIPTION,
            ToolAnnotations::new().read_only(true).open_world(false),
        )),
    ]
}

pub(super) fn winx_prompts() -> Vec<Prompt> {
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

/// Server icon (96x96 PNG) as a self-contained data URI, per MCP 2026-07-28.
/// A data URI works over stdio and HTTP alike — no extra endpoint or auth
/// exemption is needed for clients to fetch it.
pub(super) fn server_icon_data_uri() -> &'static str {
    static URI: OnceLock<String> = OnceLock::new();
    URI.get_or_init(|| {
        use base64::Engine as _;
        let png = include_bytes!("../../.github/assets/icon-96.png");
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
    })
}
