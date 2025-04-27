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
    /// (e.g., from "wcgw" to "architect" or "code_writer").
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
                "Unknown mode name: {}",
                s
            ))),
        }
    }
}

// Implement schema generation for JSON schema since we removed the derive
impl JsonSchema for ModeName {
    fn schema_name() -> String {
        "ModeName".to_string()
    }

    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = schemars::schema::SchemaObject::default();
        schema.metadata().description = Some("The mode name for initialization".to_string());
        let enum_values = vec![
            serde_json::Value::String("wcgw".to_string()),
            serde_json::Value::String("architect".to_string()),
            serde_json::Value::String("code_writer".to_string()),
        ];
        schema.enum_values = Some(enum_values);
        schema.into()
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
                        format!("{}/{}", workspace_root, glob)
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
    /// If provided during a first_call, the task with this ID will be resumed.
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

    /// ID of the chat session
    ///
    /// If not provided for a first_call, a new ID will be generated.
    /// This ID must be included in all subsequent tool calls.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub chat_id: String,

    /// Configuration for code_writer mode
    ///
    /// Only used when mode_name is "code_writer".
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

/// Default mode_name for Initialize
fn default_mode_name() -> ModeName {
    ModeName::Wcgw
}

/// Default init_type for Initialize
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
    fn schema_name() -> String {
        "Modes".to_string()
    }

    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = schemars::schema::SchemaObject::default();
        schema.metadata().description = Some("Internal representation of modes".to_string());
        let enum_values = vec![
            serde_json::Value::String("wcgw".to_string()),
            serde_json::Value::String("architect".to_string()),
            serde_json::Value::String("code_writer".to_string()),
        ];
        schema.enum_values = Some(enum_values);
        schema.into()
    }
}

/// Special key types for shell interaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SpecialKey {
    Enter,
    KeyUp,
    KeyDown,
    KeyLeft,
    KeyRight,
    CtrlC,
    CtrlD,
}

/// Parameters for the ReadFiles tool
///
/// This struct represents the parameters needed to read one or more files,
/// optionally with line numbers and line range filtering.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFiles {
    /// List of file paths to read
    ///
    /// These can be absolute paths or paths relative to the current working directory.
    /// They can also include line range specifications for filtering.
    pub file_paths: Vec<String>,

    /// Optional reason to show line numbers
    ///
    /// If provided and non-empty, line numbers will be shown in the output.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_line_numbers_reason: Option<String>,

    /// Optional maximum number of tokens to include
    ///
    /// If provided, the output will be truncated to fit within this limit.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,

    /// Optional start line numbers for each file
    ///
    /// Vector of optional start line numbers corresponding to each file path.
    /// If provided, only lines from this number (1-indexed) will be included.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub start_line_nums: Vec<Option<usize>>,

    /// Optional end line numbers for each file
    ///
    /// Vector of optional end line numbers corresponding to each file path.
    /// If provided, only lines up to and including this number will be included.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub end_line_nums: Vec<Option<usize>>,
}

impl ReadFiles {
    /// Checks if line numbers should be shown
    ///
    /// Line numbers are shown if show_line_numbers_reason is Some and non-empty
    pub fn show_line_numbers(&self) -> bool {
        self.show_line_numbers_reason
            .as_ref()
            .map(|reason| !reason.is_empty())
            .unwrap_or(false)
    }

    /// Parses file paths for line ranges
    ///
    /// This method extracts line range specifications from file paths and updates
    /// the start_line_nums and end_line_nums vectors accordingly.
    ///
    /// File paths can include line range specifications like:
    /// - file.py:10      (specific line)
    /// - file.py:10-20   (line range)
    /// - file.py:10-     (from line 10 to end)
    /// - file.py:-20     (from start to line 20)
    #[allow(dead_code)]
    pub fn parse_line_ranges(&mut self) {
        // Initialize vectors if they're empty
        if self.start_line_nums.is_empty() {
            self.start_line_nums = vec![None; self.file_paths.len()];
        }
        if self.end_line_nums.is_empty() {
            self.end_line_nums = vec![None; self.file_paths.len()];
        }

        // Create new file_paths list without line ranges
        let mut clean_file_paths = Vec::new();

        for (i, file_path) in self.file_paths.iter().enumerate() {
            let mut start_line_num = None;
            let mut end_line_num = None;
            let mut path_part = file_path.clone();

            // Check if the path ends with a line range pattern
            if file_path.contains(':') {
                let parts: Vec<&str> = file_path.rsplitn(2, ':').collect();
                if parts.len() == 2 {
                    let potential_path = parts[1];
                    let line_spec = parts[0];

                    // Check if it's a valid line range format
                    if let Ok(line_num) = line_spec.parse::<usize>() {
                        // Format: file.py:10
                        start_line_num = Some(line_num);
                        end_line_num = Some(line_num);
                        path_part = potential_path.to_string();
                    } else if line_spec.contains('-') {
                        // Could be file.py:10-20, file.py:10-, or file.py:-20
                        let line_parts: Vec<&str> = line_spec.split('-').collect();

                        if line_parts[0].is_empty() && !line_parts[1].is_empty() {
                            // Format: file.py:-20
                            if let Ok(end) = line_parts[1].parse::<usize>() {
                                end_line_num = Some(end);
                                path_part = potential_path.to_string();
                            }
                        } else if !line_parts[0].is_empty() {
                            // Format: file.py:10-20 or file.py:10-
                            if let Ok(start) = line_parts[0].parse::<usize>() {
                                start_line_num = Some(start);

                                if !line_parts[1].is_empty() {
                                    // file.py:10-20
                                    if let Ok(end) = line_parts[1].parse::<usize>() {
                                        end_line_num = Some(end);
                                    }
                                }
                                path_part = potential_path.to_string();
                            }
                        }
                    }
                }
            }

            // Update start and end line numbers
            if i < self.start_line_nums.len() {
                self.start_line_nums[i] = start_line_num;
            } else {
                self.start_line_nums.push(start_line_num);
            }

            if i < self.end_line_nums.len() {
                self.end_line_nums[i] = end_line_num;
            } else {
                self.end_line_nums.push(end_line_num);
            }

            clean_file_paths.push(path_part);
        }

        // Update file_paths with clean paths
        self.file_paths = clean_file_paths;
    }
}

/// Types of actions that can be performed with the BashCommand tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BashCommandAction {
    /// Execute a shell command
    Command { command: String },

    /// Check the status of a running command
    StatusCheck { status_check: bool },

    /// Send text to a running command
    SendText { send_text: String },

    /// Send special keys to a running command
    SendSpecials { send_specials: Vec<SpecialKey> },

    /// Send ASCII characters to a running command
    SendAscii { send_ascii: Vec<u8> },
}

/// Parameters for the BashCommand tool
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BashCommand {
    /// The action to perform (command, status check, etc.)
    pub action_json: BashCommandAction,

    /// Optional timeout in seconds to wait for command completion
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_seconds: Option<f32>,

    /// The chat ID for this session
    #[serde(default)]
    pub chat_id: String,
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
            chat_id: String,
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
                        if s.contains("command") && s.contains('{') && s.contains('}') {
                            // It looks like JSON but has issues, let's try to sanitize it
                            let re_sanitized = s
                                .replace('\n', "\\n") // Replace newlines with escaped newlines
                                .replace('\r', "\\r"); // Replace carriage returns with escaped versions
                            match serde_json::from_str(&re_sanitized) {
                                Ok(json) => json,
                                Err(_) => {
                                    // Last resort fallback - assume it's a command string
                                    serde_json::json!({"command": s})
                                }
                            }
                        } else {
                            // Assume it's a simple command string
                            serde_json::json!({"command": s})
                        }
                    }
                }
            }
            // If it's already an object or other JSON value, use it directly
            value => value,
        };

        // Now deserialize the action_json to our BashCommandAction enum
        let action: BashCommandAction = serde_json::from_value(action_json).map_err(|e| {
            tracing::error!(
                "Failed to deserialize action_json to BashCommandAction: {}",
                e
            );
            serde::de::Error::custom(format!("Invalid action_json: {}", e))
        })?;

        // Return the properly constructed BashCommand
        Ok(BashCommand {
            action_json: action,
            wait_for_seconds: helper.wait_for_seconds,
            chat_id: helper.chat_id,
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

/// Parameters for the FileWriteOrEdit tool
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
    /// If percentage_to_change > 50%, this is the entire file content.
    /// If percentage_to_change <= 50%, this contains search/replace blocks
    /// in the format:
    /// ```
    /// <<<<<<< SEARCH
    /// old content to find
    /// =======
    /// new content to replace with
    /// >>>>>>> REPLACE
    /// ```
    pub file_content_or_search_replace_blocks: String,

    /// The chat ID for this session
    pub chat_id: String,
}
