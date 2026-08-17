use std::sync::{Arc, OnceLock};

use rmcp::model::{Prompt, Tool, ToolAnnotations};
use schemars::schema_for;
use serde_json::Value;

use crate::types::{
    BashCommand, CodeMap, CodeMapStructuredOutput, ContextSave, FileWriteOrEdit, Initialize,
    MultiFileEdit, ReadFiles, ReadImage, UndoEdit,
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
     - MCP Tasks are supported for a single foreground `command` when the client declares the `io.modelcontextprotocol/tasks` extension. Use an explicit `thread_id`; poll with tasks/get, whose completed state contains the final CallToolResult. Do not combine an MCP Task with `is_background=true`. \
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
     - Scope either operation with `path` (file or directory; empty = the whole workspace); cap with `max_results`. gitignore-aware, workspace-confined, works in every mode. \
     - 11 languages (rust, js/ts, go, c, c++, java, ruby, c#, php, lua); other files return no symbols. Note: C/C++ grammars tag definitions only, so references reads 0 for `.c`/`.h`/`.cpp`. \
     - For plain-text/regex search or file discovery, use rg / grep / fd / find via BashCommand.";

static WINX_TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
static WINX_PROMPTS: OnceLock<Vec<Prompt>> = OnceLock::new();

pub(super) fn winx_tools() -> Vec<Tool> {
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
