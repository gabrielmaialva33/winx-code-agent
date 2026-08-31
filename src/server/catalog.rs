use std::sync::{Arc, OnceLock};

use rmcp::model::{Prompt, Tool, ToolAnnotations};
use schemars::schema_for;
use serde_json::Value;

use super::outcomes::{CodeMapToolResultEnvelope, ToolResultEnvelope};
use crate::tool_policy::ToolPolicy;
use crate::tool_registry::{ToolAccess, ToolKind, ToolOutputContract, ToolWorld};
use crate::tools::edit_files::EditFilesWire;
use crate::types::{
    ApplyPatch, BashCommand, CodeMap, ContextSave, FileWriteOrEdit, Initialize, MultiFileEdit,
    ReadFiles, ReadImage, UndoEdit, VerifyEdit,
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

const WORKSPACE_ROOT_DESCRIPTION: &str =
    "Exact canonical workspace_root returned by Initialize. Before every call, confirm it matches the project in the user's current task; if uncertain, Initialize that project. Use it only with the thread_id returned by the same Initialize call. It identifies the project session and is not a filesystem sandbox: target paths may be outside it when WINX_ALLOW_PATHS and the active mode permit. Never infer it from a target file path.";

fn with_workspace_binding(
    schema: Arc<serde_json::Map<String, Value>>,
) -> Arc<serde_json::Map<String, Value>> {
    let mut schema = Arc::unwrap_or_clone(schema);
    let properties = schema
        .entry("properties".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(properties) = properties {
        properties.insert(
            "workspace_root".to_string(),
            serde_json::json!({
                "type": "string",
                "minLength": 1,
                "description": WORKSPACE_ROOT_DESCRIPTION,
            }),
        );
        if let Some(Value::Object(thread_id)) = properties.get_mut("thread_id") {
            thread_id.insert(
                "description".to_string(),
                Value::String(
                    "Exact thread_id returned by the same Initialize call as workspace_root. Select it only after confirming workspace_root matches the user's current project. Never borrow it from another chat or project."
                        .to_string(),
                ),
            );
        }
    }
    let required = schema.entry("required".to_string()).or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(required) = required {
        for field in ["thread_id", "workspace_root"] {
            if !required.iter().any(|item| item.as_str() == Some(field)) {
                required.push(Value::String(field.to_string()));
            }
        }
    }
    Arc::new(schema)
}

fn mcp_session_tool<T: schemars::JsonSchema>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool {
    Tool::new(name, description, with_workspace_binding(schema_to_input_schema::<T>()))
        .with_annotations(annotations)
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
    with_workspace_binding(Arc::new(schema))
}

/// Optional edit verification is implemented by the MCP adapter so the stable
/// public Rust edit structs remain source-compatible for library callers.
fn edit_input_schema<T: schemars::JsonSchema>() -> Arc<serde_json::Map<String, Value>> {
    let mut schema = (*schema_to_input_schema::<T>()).clone();
    let properties = schema
        .entry("properties".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(properties) = properties {
        properties.insert(
            "verify_command".to_string(),
            serde_json::json!({
                "type": "string",
                "minLength": 1,
                "description": "Optional finite foreground shell command to run immediately after a successful edit. Compose related fail-fast checks with &&. The edit remains applied if verification exits non-zero. Uses the same mode command policy as BashCommand."
            }),
        );
        properties.insert(
            "verify_wait_for_seconds".to_string(),
            serde_json::json!({
                "type": "number",
                "minimum": 0,
                "maximum": 60,
                "default": 15,
                "description": "Inline wait for verify_command. If it is still running, the result supplies a BashCommand status_check next action."
            }),
        );
    }
    with_workspace_binding(Arc::new(schema))
}

fn edit_files_input_schema() -> Arc<serde_json::Map<String, Value>> {
    let mut schema = Arc::unwrap_or_clone(schema_to_input_schema::<EditFilesWire>());
    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        if let Some(Value::Object(files)) = properties.get_mut("files") {
            files.insert("minItems".to_string(), Value::from(1));
            files.insert("maxItems".to_string(), Value::from(100));
        }
        if let Some(Value::Object(command)) = properties.get_mut("verify_command") {
            command.insert("minLength".to_string(), Value::from(1));
        }
        if let Some(Value::Object(wait)) = properties.get_mut("verify_wait_for_seconds") {
            wait.insert("minimum".to_string(), Value::from(0));
            wait.insert("maximum".to_string(), Value::from(60));
            wait.insert("default".to_string(), Value::from(15));
        }
    }
    if let Some(properties) = schema
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .and_then(|definitions| definitions.get_mut("EditFileWire"))
        .and_then(|entry| entry.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        if let Some(Value::Object(path)) = properties.get_mut("file_path") {
            path.insert("minLength".to_string(), Value::from(1));
        }
        if let Some(Value::Object(revision)) = properties.get_mut("expected_revision") {
            revision.insert("pattern".to_string(), Value::String("^sha256:[0-9a-f]{64}$".into()));
        }
        if let Some(Value::Object(patches)) = properties.get_mut("patches") {
            patches.insert("minItems".to_string(), Value::from(1));
            patches.insert("maxItems".to_string(), Value::from(256));
        }
        if let Some(Value::Object(undo_id)) = properties.get_mut("undo_id") {
            undo_id.insert("minLength".to_string(), Value::from(1));
        }
    }
    with_workspace_binding(Arc::new(schema))
}

fn apply_patch_input_schema() -> Arc<serde_json::Map<String, Value>> {
    let mut schema = Arc::unwrap_or_clone(edit_input_schema::<ApplyPatch>());
    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        if let Some(Value::Object(revision)) = properties.get_mut("expected_revision") {
            revision
                .insert("pattern".to_string(), Value::String("^sha256:[0-9a-f]{64}$".to_string()));
        }
        if let Some(Value::Object(patches)) = properties.get_mut("patches") {
            patches.insert("minItems".to_string(), Value::from(1));
            patches.insert("maxItems".to_string(), Value::from(256));
        }
    }
    Arc::new(schema)
}

fn verify_edit_input_schema() -> Arc<serde_json::Map<String, Value>> {
    let mut schema = (*schema_to_input_schema::<VerifyEdit>()).clone();
    let properties = schema
        .entry("properties".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(properties) = properties {
        if let Some(Value::Object(command)) = properties.get_mut("command") {
            command.insert("minLength".to_string(), Value::from(1));
        }
        if let Some(Value::Object(receipt)) = properties.get_mut("verification_id") {
            receipt.insert("pattern".to_string(), Value::String("^verify_[0-9a-f]{24}$".into()));
        }
        if let Some(Value::Object(wait)) = properties.get_mut("wait_for_seconds") {
            wait.insert("minimum".to_string(), Value::from(0));
            wait.insert("maximum".to_string(), Value::from(60));
        }
    }
    with_workspace_binding(Arc::new(schema))
}

fn with_output_schema<T: schemars::JsonSchema>(mut tool: Tool) -> Tool {
    tool.output_schema = Some(schema_to_input_schema::<T>());
    tool
}

const INITIALIZE_DESCRIPTION: &str =
    "Open one project before stateful tools unless MCP Roots did. Use first_call + wcgw with the user's project/file and preserve the returned thread_id/workspace_root pair; workspace_root is identity, not a path sandbox. Never reinitialize a valid pair. Use user_asked_mode_change only when asked. Use reset_shell only after BashCommand reports a shell-runtime failure; a redundant reset within the cooldown preserves the healthy PTY. State loss safely recreates the intended session; another project needs a new conversation. Put derived helpers only in temporary_artifact_dir.";

const BASH_COMMAND_DESCRIPTION: &str =
    "Run commands, tests, builds, servers, formatters, generators, and TUIs after Initialize. Combine finite checks with &&. adaptive is default; until_complete is for finite foreground commands, return_early gives prompt control. For running results, wait retry_after_ms and execute next_action/status_check; never resubmit the command. Use background/interactive actions for long-lived work. Use ReadFiles/EditFiles, not shell/sed/Python, for ordinary canonical source reads and edits. Keep helpers under $WINX_TEMP_DIR, reuse names, never create CodeMap-only carriers, and clean obsolete helpers when directed; Winx may reclaim TTL-expired helpers near quota.";

const READ_FILES_DESCRIPTION: &str =
    "Read exact canonical text and record visible edit coverage. Prefer this over cat/head/tail. Use :start-end targeted ranges; bounds may be omitted. Each file returns path, revision, and visibleRanges; default existing-file edits to EditFiles line_patch with that receipt. Truncation never records unseen lines. Use ReadImage for images.";

const EDIT_FILES_DESCRIPTION: &str =
    "Create, change, or undo one or many files. Provide one unique files entry per target: line_patch is the default for an existing file after ReadFiles (copy its revision and visible coordinates); reserve search_replace for intentional text anchoring copied exactly from the current read; use replace for new files or deliberate whole-file rewrites. Omit operation for normal edits; exactly one mode=undo entry with its exact undo_id infers undo. Every existing target must be freshly read and every apply entry validates before writing. On SEARCH conflict, run the exact ReadFiles nextAction and switch the corrected retry to line_patch, never shell editing. verify_command runs once after commit; if it fails, keep the edit and follow nextAction instead of repeating it.";

const FILE_WRITE_OR_EDIT_DESCRIPTION: &str =
    "Edit one read file; use MultiFileEdit for batches. For <=50%, use exact SEARCH/REPLACE blocks (optional @line); otherwise read and send the complete file. A SEARCH conflict revokes the read permit: execute its ReadFiles next_action (shell reads do not count), rebuild SEARCH, and retry once. Put helpers in temporary_artifact_dir. verify_command is post-commit; completed_with_issues means the edit remains applied - diagnose it, never repeat it.";

const MULTI_FILE_EDIT_DESCRIPTION: &str =
    "Atomically edit 2+ read, unique files. Validation failure writes nothing and revokes only the conflicting target's permit; execute its ReadFiles next_action before one corrected retry. Uses FileWriteOrEdit semantics. verify_command is post-commit; completed_with_issues means edits remain applied - never repeat them.";

const APPLY_PATCH_DESCRIPTION: &str =
    "Patch one exact ReadFiles revision with ordered non-overlapping 1-based ranges. Copy path/revision; delete_lines=0 inserts and totalLines+1 appends. Only visible lines may change. On revision_mismatch execute the exact ReadFiles nextAction; never retry stale input. Prefer this over SEARCH when coordinates are known.";

const VERIFY_EDIT_DESCRIPTION: &str =
    "Rerun a post-edit check without repeating the committed edit. Use the exact nextAction receipt after completed_with_issues, correct the code first, and never retry a failing check unchanged.";

const UNDO_EDIT_DESCRIPTION: &str =
    "Restore one file to its previous Winx edit checkpoint in this session. Repeated calls walk backward per file. Undo is refused after an external change and cannot remove a newly created file.";

const CONTEXT_SAVE_DESCRIPTION: &str =
    "Save a concise description and selected file/glob contents as one context artifact. Use the user's id when provided, otherwise a short unique id.";

const CODE_MAP_DESCRIPTION: &str =
    "Navigate syntax-aware code structure in 13 languages. outline on a file returns definitions; on a directory it returns a query/activity-ranked, byte-budgeted page. Pass a concise query and continue truncated maps with next_cursor using the same path/query. references requires an exact name and returns definitions before uses. Results are gitignore-aware and contain symbols, not source. For exact text or unsupported languages, use the structured fallback to ReadFiles; plain-text search belongs to rg via BashCommand; never transform source solely to make CodeMap parse it or turn command output into a carrier. A genuinely useful derived helper must be one existing file in temporary_artifact_dir returned by Initialize; reuse stable names because helper maps have smaller per-response, unique-file, and aggregate-call budgets while canonical maps remain unrestricted.";

static WINX_TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
static WINX_PROMPTS: OnceLock<Vec<Prompt>> = OnceLock::new();

pub(super) fn winx_tools() -> Vec<Tool> {
    winx_tools_for_policy(ToolPolicy::default())
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
    ToolKind::ALL.into_iter().map(build_winx_tool).collect()
}

fn annotations(kind: ToolKind) -> ToolAnnotations {
    let descriptor = kind.descriptor();
    let annotations = ToolAnnotations::new().open_world(descriptor.world == ToolWorld::Open);
    match descriptor.access {
        ToolAccess::ReadOnly => annotations.read_only(true),
        ToolAccess::Neutral => annotations.destructive(false),
        ToolAccess::Destructive => annotations.destructive(true),
    }
}

fn build_winx_tool(kind: ToolKind) -> Tool {
    let tool = match kind {
        ToolKind::Initialize => mcp_tool::<Initialize>(
            kind.as_str(),
            INITIALIZE_DESCRIPTION,
            annotations(kind),
        ),
        ToolKind::BashCommand => Tool::new(
            kind.as_str(),
            BASH_COMMAND_DESCRIPTION,
            bash_command_input_schema(),
        )
        .with_annotations(annotations(kind)),
        ToolKind::ReadFiles => mcp_session_tool::<ReadFiles>(
            kind.as_str(),
            READ_FILES_DESCRIPTION,
            annotations(kind),
        ),
        ToolKind::FileWriteOrEdit => Tool::new(
            kind.as_str(),
            FILE_WRITE_OR_EDIT_DESCRIPTION,
            edit_input_schema::<FileWriteOrEdit>(),
        )
        .with_annotations(annotations(kind)),
        ToolKind::MultiFileEdit => Tool::new(
            kind.as_str(),
            MULTI_FILE_EDIT_DESCRIPTION,
            edit_input_schema::<MultiFileEdit>(),
        )
        .with_annotations(annotations(kind)),
        ToolKind::VerifyEdit => {
            Tool::new(kind.as_str(), VERIFY_EDIT_DESCRIPTION, verify_edit_input_schema())
                .with_annotations(annotations(kind))
        }
        ToolKind::UndoEdit => mcp_session_tool::<UndoEdit>(
            kind.as_str(),
            UNDO_EDIT_DESCRIPTION,
            annotations(kind),
        ),
        ToolKind::ContextSave => mcp_session_tool::<ContextSave>(
            kind.as_str(),
            CONTEXT_SAVE_DESCRIPTION,
            annotations(kind),
        ),
        ToolKind::ReadImage => mcp_session_tool::<ReadImage>(
            kind.as_str(),
            "Read validated JPEG/PNG/GIF/WebP as native MCP image content. Large images are bounded; unchanged session repeats return a compact reference. Set force=true only for an intentional resend.",
            annotations(kind),
        ),
        ToolKind::CodeMap => mcp_session_tool::<CodeMap>(
            kind.as_str(),
            CODE_MAP_DESCRIPTION,
            annotations(kind),
        ),
        ToolKind::ApplyPatch => Tool::new(
            kind.as_str(),
            APPLY_PATCH_DESCRIPTION,
            apply_patch_input_schema(),
        )
        .with_annotations(annotations(kind)),
        ToolKind::EditFiles => Tool::new(
            kind.as_str(),
            EDIT_FILES_DESCRIPTION,
            edit_files_input_schema(),
        )
        .with_annotations(annotations(kind)),
    };
    match kind.descriptor().output_contract {
        ToolOutputContract::Shared => with_output_schema::<ToolResultEnvelope>(tool),
        ToolOutputContract::CodeMap => with_output_schema::<CodeMapToolResultEnvelope>(tool),
    }
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
