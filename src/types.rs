use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
            _ => Err(serde::de::Error::custom(format!(
                "Unknown mode name: {s}"
            ))),
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
    #[allow(dead_code)]
    pub fn is_allowed(&self, glob: &str) -> bool {
        match self {
            AllowedGlobs::All(s) if s == "all" => true,
            AllowedGlobs::List(globs) => globs.iter().any(|g| glob == g),
            _ => false,
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
    #[allow(dead_code)]
    pub fn is_allowed(&self, command: &str) -> bool {
        match self {
            AllowedCommands::All(s) if s == "all" => true,
            AllowedCommands::List(commands) => commands.iter().any(|c| command == c),
            _ => false,
        }
    }
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
                    // Log the error and the value for debugging
                    tracing::error!("Failed to parse CodeWriterConfig: {}. Value: {}", e, value);
                    Ok(None) // Fall back to None on parse error
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
        }

        let input = serde_json::Value::deserialize(deserializer)?;

        if !input.is_object() {
            if input.is_null() {
                return Err(serde::de::Error::custom(
                    "Cannot convert null to ReadFiles object.",
                ));
            }
            return Err(serde::de::Error::custom(format!(
                "Expected object, got {input}"
            )));
        }

        let helper: ReadFilesHelper = serde_json::from_value(input.clone())
            .map_err(|e| serde::de::Error::custom(format!("Failed to parse ReadFiles: {e}")))?;

        let file_paths = match helper.file_paths {
            Some(paths) if !paths.is_empty() => paths,
            Some(_) => {
                return Err(serde::de::Error::custom("file_paths must not be empty."))
            }
            None => {
                return Err(serde::de::Error::custom("file_paths is required."))
            }
        };

        // Parse line ranges from file paths (like wcgw Python's model_post_init)
        let mut start_line_nums = Vec::with_capacity(file_paths.len());
        let mut end_line_nums = Vec::with_capacity(file_paths.len());

        for path in &file_paths {
            let (start, end) = parse_line_range_from_path(path);
            start_line_nums.push(start);
            end_line_nums.push(end);
        }

        Ok(ReadFiles {
            file_paths,
            start_line_nums,
            end_line_nums,
        })
    }
}

/// Parse line range from a file path (e.g., "file.rs:10-20")
fn parse_line_range_from_path(path: &str) -> (Option<usize>, Option<usize>) {
    // Find the last colon that's followed by digits or a dash
    if let Some(colon_pos) = path.rfind(':') {
        let range_part = &path[colon_pos + 1..];

        // Check if it looks like a line range (not a Windows drive letter)
        if range_part.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
            if let Some(dash_pos) = range_part.find('-') {
                // Format: start-end or start- or -end
                let start_str = &range_part[..dash_pos];
                let end_str = &range_part[dash_pos + 1..];

                let start = if start_str.is_empty() {
                    None
                } else {
                    start_str.parse().ok()
                };

                let end = if end_str.is_empty() {
                    None
                } else {
                    end_str.parse().ok()
                };

                return (start, end);
            }
            // Single line number
            if let Ok(line) = range_part.parse::<usize>() {
                return (Some(line), Some(line));
            }
        }
    }
    (None, None)
}

impl ReadFiles {
    /// Line numbers are always shown (like wcgw Python)
    pub fn show_line_numbers(&self) -> bool {
        true
    }

    /// Get the clean file path without line range suffix
    pub fn get_clean_path(&self, index: usize) -> String {
        let path = &self.file_paths[index];
        if let Some(colon_pos) = path.rfind(':') {
            let range_part = &path[colon_pos + 1..];
            if range_part.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
                return path[..colon_pos].to_string();
            }
        }
        path.clone()
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
    },

    /// Check the status of a running command
    StatusCheck {
        #[serde(default = "default_true")]
        status_check: bool,
        bg_command_id: Option<String>,
    },

    /// Send text to a running command
    SendText {
        send_text: String,
        bg_command_id: Option<String>,
    },

    /// Send special keys to a running command
    SendSpecials {
        send_specials: Vec<SpecialKey>,
        bg_command_id: Option<String>,
    },

    /// Send ASCII characters to a running command
    SendAscii {
        send_ascii: Vec<u8>,
        bg_command_id: Option<String>,
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
        // Define an intermediate struct with the same fields but different types
        #[derive(Deserialize)]
        struct BashCommandHelper {
            action_json: serde_json::Value,
            #[serde(default)]
            wait_for_seconds: Option<f32>,
            #[serde(default)]
            #[serde(deserialize_with = "deserialize_string_or_null")]
            thread_id: String,
        }

        // Deserialize to the helper struct first
        let helper = BashCommandHelper::deserialize(deserializer)?;

        // Process action_json which could be a string or an object
        let action_json = match helper.action_json {
            serde_json::Value::String(s) => {
                // If it's a string, normalize newlines and try to parse it as JSON
                // Replace literal newlines with space to avoid JSON parsing errors
                let sanitized = s.replace('\n', " ");
                match serde_json::from_str(&sanitized) {
                    Ok(json) => json,
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
                                Ok(json) => json,
                                Err(err) => {
                                    // Log the specific parsing error for debugging
                                    tracing::error!("Secondary JSON parse error: {}", err);
                                    // Last resort fallback - assume it's a command string
                                    // MUST include "type": "command" for serde tagged enum
                                    serde_json::json!({"type": "command", "command": s})
                                }
                            }
                        } else {
                            // Assume it's a simple command string
                            // MUST include "type": "command" for serde tagged enum
                            tracing::info!("Treating as simple command: {}", s);
                            serde_json::json!({"type": "command", "command": s})
                        }
                    }
                }
            }
            // If it's already an object or other JSON value, use it directly
            value => value,
        };

        // Now deserialize the action_json to our BashCommandAction enum
        let action: BashCommandAction = serde_json::from_value(action_json.clone()).map_err(|e| {
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
            wait_for_seconds: helper.wait_for_seconds,
            thread_id: helper.thread_id,
        })
    }
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
}

