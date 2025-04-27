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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BashCommand {
    /// The action to perform (command, status check, etc.)
    pub action_json: BashCommandAction,

    /// Optional timeout in seconds to wait for command completion
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_seconds: Option<f32>,

    /// The chat ID for this session
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub chat_id: String,
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
