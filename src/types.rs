use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InitializeType {
    FirstCall,
    UserAskedModeChange,
    ResetShell,
    UserAskedChangeWorkspace,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ModeName {
    Wcgw,
    Architect,
    #[serde(alias = "code_write", alias = "code-writer")]
    CodeWriter,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq)]
pub struct CodeWriterConfig {
    #[serde(default)]
    pub allowed_globs: AllowedGlobs,
    #[serde(default)]
    pub allowed_commands: AllowedCommands,
}

impl Default for CodeWriterConfig {
    fn default() -> Self {
        Self {
            allowed_globs: AllowedGlobs::default(),
            allowed_commands: AllowedCommands::default(),
        }
    }
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
    pub fn is_allowed(&self, command: &str) -> bool {
        match self {
            AllowedCommands::All(s) if s == "all" => true,
            AllowedCommands::List(commands) => commands.iter().any(|c| command == c),
            _ => false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct Initialize {
    #[serde(rename = "type")]
    #[serde(default = "default_init_type")]
    pub init_type: InitializeType,
    pub any_workspace_path: String,
    #[serde(default)]
    pub initial_files_to_read: Vec<String>,
    #[serde(default = "String::new")]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub task_id_to_resume: String,
    #[serde(default = "default_mode_name")]
    pub mode_name: ModeName,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_string_or_null")]
    pub chat_id: String,
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
            match serde_json::from_value::<CodeWriterConfig>(value) {
                Ok(config) => Ok(Some(config)),
                Err(_) => Ok(None), // Fall back to None on parse error
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
#[derive(Debug, Clone, Copy, JsonSchema, PartialEq)]
pub enum Modes {
    Wcgw,
    Architect,
    CodeWriter,
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
