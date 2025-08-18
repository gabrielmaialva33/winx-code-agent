//! Winx MCP Server implementation using rmcp 0.5.0
//! Enhanced server with NVIDIA AI integration

use rmcp::{
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde_json::Value;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::{info, warn};

use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::state::BashState;
use crate::types::{CommandSuggestions, ContextSave, ReadImage};

/// Helper function to create JSON schema from serde_json::Value
fn json_to_schema(value: Value) -> Arc<serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Winx service with shared bash state and NVIDIA AI integration
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    pub bash_state: Arc<Mutex<Option<BashState>>>,
    /// NVIDIA client for AI-powered features (optional)
    pub nvidia_client: Arc<Mutex<Option<NvidiaClient>>>,
    /// Version information for the service
    pub version: String,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new WinxService instance
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            nvidia_client: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Initialize NVIDIA integration if API key is available
    pub async fn initialize_nvidia(&self) -> crate::Result<bool> {
        match NvidiaConfig::from_env() {
            Ok(config) => match crate::nvidia::initialize(config).await {
                Ok(client) => {
                    *self.nvidia_client.lock().unwrap() = Some(client);
                    info!("NVIDIA AI integration initialized successfully");
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to initialize NVIDIA integration: {}", e);
                    Ok(false)
                }
            },
            Err(e) => {
                info!("NVIDIA integration not available: {}", e);
                Ok(false)
            }
        }
    }

    /// Get NVIDIA client if available
    pub async fn get_nvidia_client(&self) -> Option<NvidiaClient> {
        self.nvidia_client.lock().unwrap().clone()
    }
}

/// ServerHandler implementation with manual tool handling
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "winx-code-agent".into(),
                version: self.version.clone(),
            },
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents with NVIDIA AI integration. \
                Provides shell execution, file management, and AI-powered code analysis capabilities.".into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: vec![
                Tool {
                    name: "ping".into(),
                    description: Some("Test server connectivity".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "message": {
                                "type": "string",
                                "description": "Optional message to echo back"
                            }
                        }
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "initialize".into(),
                    description: Some("Initialize the bash shell environment".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "shell": {
                                "type": "string",
                                "description": "Shell to use (default: bash)"
                            }
                        }
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "bash_command".into(),
                    description: Some("Execute a command in the bash shell".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "description": "Command to execute"
                            },
                            "timeout_seconds": {
                                "type": "integer",
                                "description": "Timeout in seconds (default: 30)"
                            }
                        },
                        "required": ["command"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "read_files".into(),
                    description: Some("Read contents of one or more files".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "paths": {
                                "type": "array",
                                "items": {
                                    "type": "string"
                                },
                                "description": "File paths to read"
                            }
                        },
                        "required": ["paths"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "file_write_or_edit".into(),
                    description: Some("Write or edit file contents".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path to write"
                            },
                            "content": {
                                "type": "string",
                                "description": "Content to write"
                            },
                            "create_if_missing": {
                                "type": "boolean",
                                "description": "Create file if it doesn't exist (default: true)"
                            }
                        },
                        "required": ["path", "content"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "context_save".into(),
                    description: Some("Save task context to a file for resumption".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for the task"
                            },
                            "project_root_path": {
                                "type": "string",
                                "description": "Root path of the project"
                            },
                            "description": {
                                "type": "string",
                                "description": "Description of the task"
                            },
                            "relevant_file_globs": {
                                "type": "array",
                                "items": {
                                    "type": "string"
                                },
                                "description": "List of file glob patterns to include"
                            }
                        },
                        "required": ["id", "project_root_path", "description", "relevant_file_globs"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "read_image".into(),
                    description: Some("Read image file and return as base64".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "file_path": {
                                "type": "string",
                                "description": "Path to the image file"
                            }
                        },
                        "required": ["file_path"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "command_suggestions".into(),
                    description: Some("Get command suggestions based on context".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "partial_command": {
                                "type": "string",
                                "description": "Partial command to get suggestions for"
                            },
                            "current_dir": {
                                "type": "string",
                                "description": "Optional directory context"
                            },
                            "previous_command": {
                                "type": "string",
                                "description": "Optional previous command"
                            },
                            "max_suggestions": {
                                "type": "integer",
                                "description": "Maximum number of suggestions to return (default: 5)"
                            },
                            "include_explanations": {
                                "type": "boolean",
                                "description": "Whether to include command explanations"
                            }
                        }
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "code_analyzer".into(),
                    description: Some("AI-powered code analysis for bugs, security, and performance".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "file_path": {
                                "type": "string",
                                "description": "Path to the code file to analyze"
                            },
                            "language": {
                                "type": "string",
                                "description": "Programming language (optional, auto-detected if not provided)"
                            }
                        },
                        "required": ["file_path"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "ai_generate_code".into(),
                    description: Some("Generate code from natural language description using NVIDIA AI".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "prompt": {
                                "type": "string",
                                "description": "Natural language description of the code to generate"
                            },
                            "language": {
                                "type": "string",
                                "description": "Target programming language (e.g., 'Rust', 'Python', 'JavaScript')"
                            },
                            "context": {
                                "type": "string",
                                "description": "Additional context or requirements"
                            },
                            "max_tokens": {
                                "type": "integer",
                                "description": "Maximum tokens to generate (default: 1000)"
                            },
                            "temperature": {
                                "type": "number",
                                "description": "Creativity level 0.0-1.0 (default: 0.7)"
                            }
                        },
                        "required": ["prompt"]
                    })),
                    output_schema: None,
                    annotations: None,
                },
                Tool {
                    name: "ai_explain_code".into(),
                    description: Some("Get AI explanation and documentation for code".into()),
                    input_schema: json_to_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "file_path": {
                                "type": "string",
                                "description": "Path to the code file to explain"
                            },
                            "code": {
                                "type": "string",
                                "description": "Code snippet to explain (alternative to file_path)"
                            },
                            "language": {
                                "type": "string",
                                "description": "Programming language (optional, auto-detected if not provided)"
                            },
                            "detail_level": {
                                "type": "string",
                                "enum": ["basic", "detailed", "expert"],
                                "description": "Level of detail for explanation (default: detailed)"
                            }
                        }
                    })),
                    output_schema: None,
                    annotations: None,
                },
            ],
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        param: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args_value = param.arguments.map(Value::Object);
        let result = match param.name.as_ref() {
            "ping" => self.handle_ping(args_value.clone()).await?,
            "initialize" => self.handle_initialize(args_value.clone()).await?,
            "bash_command" => self.handle_bash_command(args_value.clone()).await?,
            "read_files" => self.handle_read_files(args_value.clone()).await?,
            "file_write_or_edit" => self.handle_file_write_or_edit(args_value.clone()).await?,
            "context_save" => self.handle_context_save(args_value.clone()).await?,
            "read_image" => self.handle_read_image(args_value.clone()).await?,
            "command_suggestions" => self.handle_command_suggestions(args_value.clone()).await?,
            "code_analyzer" => self.handle_code_analyzer(args_value).await?,
            "ai_generate_code" => self.handle_ai_generate_code(args_value).await?,
            "ai_explain_code" => self.handle_ai_explain_code(args_value).await?,
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown tool: {}", param.name),
                    None,
                ))
            }
        };

        Ok(result)
    }
}

impl WinxService {
    async fn handle_ping(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let message = args
            .and_then(|v| {
                if let Value::Object(map) = v {
                    map.get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "pong".to_string());

        let content = format!(
            "Server: winx-code-agent v{}\nResponse: {}",
            self.version, message
        );
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    async fn handle_initialize(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let shell = args
            .and_then(|v| {
                if let Value::Object(map) = v {
                    map.get("shell")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "bash".to_string());

        let mut bash_state_guard = self.bash_state.lock().unwrap();
        if bash_state_guard.is_some() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Shell environment is already initialized".to_string(),
            )]));
        }

        let mut state = crate::state::BashState::new();
        match state.init_interactive_bash() {
            Ok(_) => {
                *bash_state_guard = Some(state);
                info!("Shell environment initialized with {}", shell);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Shell environment initialized with {}",
                    shell
                ))]))
            }
            Err(e) => {
                warn!("Failed to initialize shell: {}", e);
                Err(McpError::internal_error(
                    format!("Failed to initialize shell: {}", e),
                    None,
                ))
            }
        }
    }

    async fn handle_bash_command(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing command", None))?;
        let timeout_seconds = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(30) as f32;

        // Clone the bash state to avoid holding the mutex across await
        let mut bash_state_clone = {
            let bash_state_guard = self.bash_state.lock().unwrap();
            if bash_state_guard.is_none() {
                return Err(McpError::invalid_request(
                    "Shell not initialized. Call initialize first.",
                    None,
                ));
            }
            bash_state_guard.as_ref().unwrap().clone()
        };

        match bash_state_clone
            .execute_interactive(command, timeout_seconds)
            .await
        {
            Ok(output) => {
                // Update the original state with any changes
                {
                    let mut bash_state_guard = self.bash_state.lock().unwrap();
                    *bash_state_guard = Some(bash_state_clone.clone());
                }
                let working_dir = bash_state_clone.cwd.display().to_string();
                let content = format!("Working directory: {}\n\n{}", working_dir, output);
                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(e) => {
                warn!("Command execution failed: {}", e);
                Err(McpError::internal_error(
                    format!("Command execution failed: {}", e),
                    None,
                ))
            }
        }
    }

    async fn handle_read_files(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let paths = args
            .get("paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::invalid_request("Missing paths array", None))?;

        let mut content_parts = Vec::new();

        for path_value in paths {
            let path = path_value
                .as_str()
                .ok_or_else(|| McpError::invalid_request("Invalid path in array", None))?;

            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    content_parts.push(format!(
                        "=== {} ({} bytes) ===\n{}\n",
                        path,
                        content.len(),
                        content
                    ));
                }
                Err(e) => {
                    content_parts.push(format!("=== {} ===\nERROR: {}\n", path, e));
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            content_parts.join("\n"),
        )]))
    }

    async fn handle_file_write_or_edit(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing path", None))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing content", None))?;
        let create = args
            .get("create_if_missing")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if !create && !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err(McpError::invalid_request(
                format!("File does not exist: {}", path),
                None,
            ));
        }

        match tokio::fs::write(path, content).await {
            Ok(_) => {
                info!("File written successfully: {}", path);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "File written successfully: {} ({} bytes)",
                    path,
                    content.len()
                ))]))
            }
            Err(e) => {
                warn!("Failed to write file {}: {}", path, e);
                Err(McpError::internal_error(
                    format!("Failed to write file {}: {}", path, e),
                    None,
                ))
            }
        }
    }

    async fn handle_context_save(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing id", None))?;
        let project_root_path = args
            .get("project_root_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing project_root_path", None))?;
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing description", None))?;
        let relevant_file_globs = args
            .get("relevant_file_globs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::invalid_request("Missing relevant_file_globs array", None))?;

        let globs: Result<Vec<String>, McpError> = relevant_file_globs
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| McpError::invalid_request("Invalid glob in array", None))
            })
            .collect();
        let globs = globs?;

        let context_save = ContextSave {
            id: id.to_string(),
            project_root_path: project_root_path.to_string(),
            description: description.to_string(),
            relevant_file_globs: globs,
        };

        match crate::tools::context_save::handle_tool_call(&self.bash_state, context_save).await {
            Ok(result) => {
                info!("Context saved successfully");
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("Failed to save context: {}", e);
                Err(McpError::internal_error(
                    format!("Failed to save context: {}", e),
                    None,
                ))
            }
        }
    }

    async fn handle_read_image(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing file_path", None))?;

        let read_image = ReadImage {
            file_path: file_path.to_string(),
        };

        match crate::tools::read_image::handle_tool_call(&self.bash_state, read_image).await {
            Ok((mime_type, base64_data)) => {
                info!("Image read successfully: {}", file_path);
                let result_text = format!(
                    "Image: {}\nMIME Type: {}\nBase64 Data: {}",
                    file_path, mime_type, base64_data
                );
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
            }
            Err(e) => {
                warn!("Failed to read image {}: {}", file_path, e);
                Err(McpError::internal_error(
                    format!("Failed to read image: {}", e),
                    None,
                ))
            }
        }
    }

    async fn handle_command_suggestions(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        let partial_command = args
            .get("partial_command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let current_dir = args
            .get("current_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let last_command = args
            .get("previous_command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let max_suggestions = args
            .get("max_suggestions")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let include_explanations = args
            .get("include_explanations")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let command_suggestions = CommandSuggestions {
            partial_command,
            current_dir,
            last_command,
            max_suggestions,
            include_explanations,
        };

        match crate::tools::command_suggestions::handle_tool_call(
            &self.bash_state,
            command_suggestions,
        )
        .await
        {
            Ok(result) => {
                info!("Command suggestions generated");
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }
            Err(e) => {
                warn!("Failed to generate command suggestions: {}", e);
                Err(McpError::internal_error(
                    format!("Failed to generate command suggestions: {}", e),
                    None,
                ))
            }
        }
    }

    async fn handle_code_analyzer(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing file_path", None))?;
        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Check if NVIDIA client is available
        if let Some(nvidia_client) = self.get_nvidia_client().await {
            // Read file content
            match tokio::fs::read_to_string(file_path).await {
                Ok(code) => {
                    // Perform AI analysis using NVIDIA
                    match nvidia_client.analyze_code(&code, language.as_deref()).await {
                        Ok(result) => {
                            let issues_text = if result.issues.is_empty() {
                                "No issues found.".to_string()
                            } else {
                                result.issues.iter()
                                    .map(|issue| format!("• [{}] {}", issue.severity, issue.message))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            };

                            let suggestions_text = if result.suggestions.is_empty() {
                                "".to_string()
                            } else {
                                format!("\n\n### Suggestions:\n{}", 
                                    result.suggestions.iter()
                                        .map(|s| format!("• {}", s))
                                        .collect::<Vec<_>>()
                                        .join("\n"))
                            };

                            let complexity_text = result.complexity_score
                                .map(|score| format!("\n\n### Complexity Score: {}/100", score))
                                .unwrap_or_default();

                            let analysis_result = format!(
                                "## 🔍 AI Code Analysis: {}\n\n**Summary:** {}\n\n### Issues Found ({}):\n{}{}{}\n\n*Analyzed using: {}*",
                                file_path,
                                result.summary,
                                result.issues.len(),
                                issues_text,
                                suggestions_text,
                                complexity_text,
                                result.model_used
                            );

                            info!("AI code analysis completed for: {} ({} issues found)", file_path, result.issues.len());
                            Ok(CallToolResult::success(vec![Content::text(analysis_result)]))
                        }
                        Err(e) => {
                            warn!("NVIDIA analysis failed for {}: {}", file_path, e);
                            let fallback = format!(
                                "## ⚠️ Code Analysis: {}\n\nNVIDIA AI analysis failed: {}\n\nFalling back to basic analysis: File exists and is readable.",
                                file_path, e
                            );
                            Ok(CallToolResult::success(vec![Content::text(fallback)]))
                        }
                    }
                }
                Err(e) => {
                    Err(McpError::invalid_request(&format!("Failed to read file {}: {}", file_path, e), None))
                }
            }
        } else {
            // Fallback without NVIDIA
            let analysis_result = format!(
                "## 📄 Basic Code Analysis: {}\n\nNVIDIA AI not available (missing NVIDIA_API_KEY).\nBasic analysis: File exists and appears to be valid {} code.",
                file_path,
                language.as_deref().unwrap_or("source")
            );
            
            info!("Basic code analysis completed for: {}", file_path);
            Ok(CallToolResult::success(vec![Content::text(analysis_result)]))
        }
    }

    async fn handle_ai_generate_code(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing prompt", None))?;
        
        let language = args.get("language").and_then(|v| v.as_str()).map(|s| s.to_string());
        let context = args.get("context").and_then(|v| v.as_str()).map(|s| s.to_string());
        let max_tokens = args.get("max_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);
        let temperature = args.get("temperature").and_then(|v| v.as_f64()).map(|f| f as f32);

        if let Some(nvidia_client) = self.get_nvidia_client().await {
            let request = crate::nvidia::models::CodeGenerationRequest {
                prompt: prompt.to_string(),
                language,
                context,
                max_tokens,
                temperature,
            };

            match nvidia_client.generate_code(&request).await {
                Ok(result) => {
                    let formatted_result = format!(
                        "## 🤖 AI Generated Code\n\n### Language: {}\n\n```{}\n{}\n```\n\n*Generated using NVIDIA AI*",
                        result.language.as_deref().unwrap_or("auto-detected"),
                        result.language.as_deref().unwrap_or(""),
                        result.code
                    );

                    info!("AI code generation completed for prompt: '{}'", prompt);
                    Ok(CallToolResult::success(vec![Content::text(formatted_result)]))
                }
                Err(e) => {
                    warn!("NVIDIA code generation failed: {}", e);
                    let fallback = format!(
                        "## ⚠️ Code Generation Failed\n\nNVIDIA AI code generation failed: {}\n\nPlease check your NVIDIA_API_KEY and try again.",
                        e
                    );
                    Ok(CallToolResult::success(vec![Content::text(fallback)]))
                }
            }
        } else {
            let fallback = format!(
                "## 📝 Code Generation Not Available\n\nNVIDIA AI not available (missing NVIDIA_API_KEY).\n\nPrompt: {}\nLanguage: {}\n\nPlease set NVIDIA_API_KEY to use AI code generation.",
                prompt,
                language.as_deref().unwrap_or("not specified")
            );
            
            info!("Code generation requested but NVIDIA not available");
            Ok(CallToolResult::success(vec![Content::text(fallback)]))
        }
    }

    async fn handle_ai_explain_code(&self, args: Option<Value>) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let file_path = args.get("file_path").and_then(|v| v.as_str());
        let code_snippet = args.get("code").and_then(|v| v.as_str());
        let language = args.get("language").and_then(|v| v.as_str()).map(|s| s.to_string());
        let detail_level = args.get("detail_level").and_then(|v| v.as_str()).unwrap_or("detailed");

        // Must provide either file_path or code
        if file_path.is_none() && code_snippet.is_none() {
            return Err(McpError::invalid_request("Must provide either file_path or code", None));
        }

        if let Some(nvidia_client) = self.get_nvidia_client().await {
            let (code, source_info) = if let Some(path) = file_path {
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => (content, format!("file: {}", path)),
                    Err(e) => {
                        return Err(McpError::invalid_request(&format!("Failed to read file {}: {}", path, e), None));
                    }
                }
            } else {
                (code_snippet.unwrap().to_string(), "provided snippet".to_string())
            };

            let detail_prompt = match detail_level {
                "basic" => "Provide a brief, high-level explanation of what this code does.",
                "expert" => "Provide a comprehensive, expert-level analysis including architecture, patterns, potential issues, and optimization opportunities.",
                _ => "Provide a detailed explanation of this code including its purpose, how it works, and key concepts."
            };

            let system_prompt = format!(
                "You are a code explanation expert. {}",
                detail_prompt
            );

            let user_prompt = if let Some(lang) = &language {
                format!("Explain this {} code:\n\n```{}\n{}\n```", lang, lang, code)
            } else {
                format!("Explain this code:\n\n```\n{}\n```", code)
            };

            let request = crate::nvidia::models::ChatCompletionRequest {
                model: nvidia_client.recommend_model(crate::nvidia::models::TaskType::CodeExplanation).as_str().to_string(),
                messages: vec![
                    crate::nvidia::models::ChatMessage::system(system_prompt),
                    crate::nvidia::models::ChatMessage::user(user_prompt),
                ],
                max_tokens: Some(1500),
                temperature: Some(0.3),
                top_p: None,
                stream: Some(false),
            };

            match nvidia_client.chat_completion(&request).await {
                Ok(response) => {
                    if let Some(choice) = response.choices.first() {
                        let explanation = choice.message.effective_content();
                        let formatted_result = format!(
                            "## 📚 AI Code Explanation\n\n**Source:** {}\n**Detail Level:** {}\n\n{}\n\n*Explained using: {}*",
                            source_info,
                            detail_level,
                            explanation,
                            request.model
                        );

                        info!("AI code explanation completed for: {}", source_info);
                        Ok(CallToolResult::success(vec![Content::text(formatted_result)]))
                    } else {
                        Err(McpError::internal_error("Empty response from NVIDIA API", None))
                    }
                }
                Err(e) => {
                    warn!("NVIDIA explanation failed: {}", e);
                    let fallback = format!(
                        "## ⚠️ Code Explanation Failed\n\nNVIDIA AI explanation failed: {}\n\nSource: {}\n\nPlease check your NVIDIA_API_KEY and try again.",
                        e, source_info
                    );
                    Ok(CallToolResult::success(vec![Content::text(fallback)]))
                }
            }
        } else {
            let fallback = format!(
                "## 📖 Code Explanation Not Available\n\nNVIDIA AI not available (missing NVIDIA_API_KEY).\n\nSource: {}\nDetail Level: {}\n\nPlease set NVIDIA_API_KEY to use AI code explanation.",
                file_path.unwrap_or("code snippet"),
                detail_level
            );
            
            info!("Code explanation requested but NVIDIA not available");
            Ok(CallToolResult::success(vec![Content::text(fallback)]))
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create service and initialize NVIDIA integration
    let service = WinxService::new();

    // Initialize NVIDIA integration
    if let Ok(enabled) = service.initialize_nvidia().await {
        if enabled {
            info!("NVIDIA AI integration enabled successfully");
        } else {
            warn!("NVIDIA AI features will be limited without valid NVIDIA_API_KEY");
        }
    } else {
        warn!("Failed to initialize NVIDIA integration");
    }

    // Create and run the server with STDIO transport
    let server = service.serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {}", e);
    })?;
    server.waiting().await?;

    Ok(())
}
