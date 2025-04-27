use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Wcgw,
    Architect,
    CodeWriter,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeWriterMode {
    pub allowed_globs: AllowedGlobs,
    pub allowed_commands: AllowedCommands,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AllowedGlobs {
    All(String),
    Specific(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AllowedCommands {
    All(String),
    Specific(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InitializeType {
    FirstCall,
    UserAskedModeChange,
    ResetShell,
    UserAskedChangeWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Initialize {
    pub r#type: InitializeType,
    pub any_workspace_path: String,
    pub initial_files_to_read: Vec<String>,
    pub task_id_to_resume: String,
    pub mode_name: Mode,
    pub chat_id: String,
    pub code_writer_config: Option<CodeWriterMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Command {
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusCheck {
    pub status_check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendText {
    pub send_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Special {
    Enter,
    KeyUp,
    KeyDown,
    KeyLeft,
    KeyRight,
    CtrlC,
    CtrlD,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendSpecials {
    pub send_specials: Vec<Special>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendAscii {
    pub send_ascii: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BashAction {
    Command(Command),
    StatusCheck(StatusCheck),
    SendText(SendText),
    SendSpecials(SendSpecials),
    SendAscii(SendAscii),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BashCommand {
    pub action_json: BashAction,
    pub wait_for_seconds: Option<f64>,
    pub chat_id: String,
}
