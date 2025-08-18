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

use crate::dashscope::{DashScopeClient, DashScopeConfig};
use crate::gemini::{GeminiClient, GeminiConfig};
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

/// Winx service with shared bash state and AI integration (DashScope + NVIDIA + Gemini)
#[derive(Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    pub bash_state: Arc<Mutex<Option<BashState>>>,
    /// DashScope client for AI-powered features (primary)
    pub dashscope_client: Arc<Mutex<Option<DashScopeClient>>>,
    /// NVIDIA client for AI-powered features (fallback 1)
    pub nvidia_client: Arc<Mutex<Option<NvidiaClient>>>,
    /// Gemini client for AI-powered features (fallback 2)
    pub gemini_client: Arc<Mutex<Option<GeminiClient>>>,
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
            dashscope_client: Arc::new(Mutex::new(None)),
            nvidia_client: Arc::new(Mutex::new(None)),
            gemini_client: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Initialize DashScope integration if API key is available
    pub async fn initialize_dashscope(&self) -> crate::Result<bool> {
        match DashScopeConfig::from_env() {
            Ok(config) => match DashScopeClient::new(config) {
                Ok(client) => {
                    *self.dashscope_client.lock().unwrap() = Some(client);
                    info!("DashScope AI integration initialized successfully");
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to initialize DashScope integration: {}", e);
                    Ok(false)
                }
            },
            Err(e) => {
                info!("DashScope integration not available: {}", e);
                Ok(false)
            }
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

    /// Get DashScope client if available
    pub async fn get_dashscope_client(&self) -> Option<DashScopeClient> {
        self.dashscope_client.lock().unwrap().clone()
    }

    /// Get NVIDIA client if available
    pub async fn get_nvidia_client(&self) -> Option<NvidiaClient> {
        self.nvidia_client.lock().unwrap().clone()
    }

    /// Initialize Gemini integration if API key is available
    pub async fn initialize_gemini(&self) -> crate::Result<bool> {
        match GeminiConfig::from_env() {
            Ok(config) => match GeminiClient::new(config) {
                Ok(client) => {
                    *self.gemini_client.lock().unwrap() = Some(client);
                    info!("Gemini AI integration initialized successfully");
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to initialize Gemini integration: {}", e);
                    Ok(false)
                }
            },
            Err(e) => {
                info!("Gemini integration not available: {}", e);
                Ok(false)
            }
        }
    }

    /// Get Gemini client if available
    pub async fn get_gemini_client(&self) -> Option<GeminiClient> {
        self.gemini_client.lock().unwrap().clone()
    }

    /// Get project structure overview
    async fn get_project_structure(&self) -> Result<String, McpError> {
        let mut structure = String::new();
        structure.push_str("# Winx Code Agent - Project Structure\n\n");
        structure.push_str("## Root Files\n");
        structure.push_str("- Cargo.toml - Project configuration and dependencies\n");
        structure.push_str("- README.md - Project documentation\n");
        structure.push_str("- CLAUDE.md - Claude integration guide\n\n");
        structure.push_str("## Source Code Structure\n");
        structure.push_str("- src/main.rs - Application entry point\n");
        structure.push_str("- src/server.rs - MCP server implementation\n");
        structure.push_str("- src/tools/ - MCP tools implementation\n");
        structure.push_str("- src/state/ - Shell and terminal state management\n");
        structure.push_str("- src/nvidia/ - NVIDIA AI integration\n");
        structure.push_str("- src/dashscope/ - DashScope AI integration\n");
        structure.push_str("- src/gemini/ - Google Gemini AI integration\n");
        structure.push_str("- src/utils/ - Utility functions\n\n");
        structure.push_str("## Key Features\n");
        structure.push_str("- Multi-provider AI integration (DashScope, NVIDIA, Gemini)\n");
        structure.push_str("- Shell command execution with state management\n");
        structure.push_str("- File operations and context saving\n");
        structure.push_str("- AI-powered code analysis and generation\n");
        Ok(structure)
    }

    /// Get source code structure details
    async fn get_src_structure(&self) -> Result<String, McpError> {
        let mut structure = String::new();
        structure.push_str("# Source Code Organization\n\n");
        
        // Try to read actual directory structure
        if let Ok(mut read_dir) = tokio::fs::read_dir("src").await {
            structure.push_str("## src/ Directory Contents\n");
            let mut entries = Vec::new();
            while let Some(entry) = read_dir.next_entry().await.unwrap_or(None) {
                entries.push(entry);
            }
            entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
            
            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
                if is_dir {
                    structure.push_str(&format!("- {}/\n", name));
                } else {
                    structure.push_str(&format!("- {}\n", name));
                }
            }
        } else {
            structure.push_str("## Core Modules\n");
            structure.push_str("- main.rs - Application entry point\n");
            structure.push_str("- server.rs - MCP server implementation\n");
            structure.push_str("- tools/ - MCP tools\n");
            structure.push_str("- state/ - State management\n");
            structure.push_str("- nvidia/ - NVIDIA integration\n");
            structure.push_str("- dashscope/ - DashScope integration\n");
            structure.push_str("- gemini/ - Gemini integration\n");
            structure.push_str("- utils/ - Utilities\n");
        }
        
        Ok(structure)
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
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
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
                    description: Some(
                        "AI-powered code analysis for bugs, security, and performance".into(),
                    ),
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
                    description: Some(
                        "Generate code from natural language description using NVIDIA AI".into(),
                    ),
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

    async fn list_resources(
        &self,
        _param: ListResourcesRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Annotated {
                    raw: RawResource {
                        uri: "file://project-structure".into(),
                        name: "Project Structure".into(),
                        description: Some("Overview of the project structure and files".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                    },
                    annotations: None,
                },
                Annotated {
                    raw: RawResource {
                        uri: "file://readme".into(),
                        name: "README".into(),
                        description: Some("Project README documentation".into()),
                        mime_type: Some("text/markdown".into()),
                        size: None,
                    },
                    annotations: None,
                },
                Annotated {
                    raw: RawResource {
                        uri: "file://cargo-toml".into(),
                        name: "Cargo.toml".into(),
                        description: Some("Project configuration and dependencies".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                    },
                    annotations: None,
                },
                Annotated {
                    raw: RawResource {
                        uri: "file://src-structure".into(),
                        name: "Source Code Structure".into(),
                        description: Some("Overview of the source code organization".into()),
                        mime_type: Some("text/plain".into()),
                        size: None,
                    },
                    annotations: None,
                },
            ],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        param: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let content = match param.uri.as_ref() {
            "file://project-structure" => {
                let structure = self.get_project_structure().await?;
                vec![ResourceContents::text(structure, param.uri.clone())]
            }
            "file://readme" => {
                match tokio::fs::read_to_string("README.md").await {
                    Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                    Err(_) => vec![ResourceContents::text("README.md not found".to_string(), param.uri.clone())],
                }
            }
            "file://cargo-toml" => {
                match tokio::fs::read_to_string("Cargo.toml").await {
                    Ok(content) => vec![ResourceContents::text(content, param.uri.clone())],
                    Err(_) => vec![ResourceContents::text("Cargo.toml not found".to_string(), param.uri.clone())],
                }
            }
            "file://src-structure" => {
                let structure = self.get_src_structure().await?;
                vec![ResourceContents::text(structure, param.uri.clone())]
            }
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown resource URI: {}", param.uri),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult { contents: content })
    }

    async fn list_prompts(
        &self,
        _param: ListPromptsRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult {
            prompts: vec![
                Prompt {
                    name: "code_review".into(),
                    description: Some("Comprehensive code review and analysis".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "file_path".into(),
                            description: Some("Path to the code file to review".into()),
                            required: Some(true),
                        },
                        PromptArgument {
                            name: "focus_areas".into(),
                            description: Some("Specific areas to focus on (security, performance, bugs, style)".into()),
                            required: Some(false),
                        },
                    ]),
                },
                Prompt {
                    name: "bug_fix_assistant".into(),
                    description: Some("AI-powered bug detection and fix suggestions".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "error_message".into(),
                            description: Some("Error message or description of the bug".into()),
                            required: Some(true),
                        },
                        PromptArgument {
                            name: "code_context".into(),
                            description: Some("Relevant code context or file path".into()),
                            required: Some(false),
                        },
                    ]),
                },
                Prompt {
                    name: "performance_optimizer".into(),
                    description: Some("Performance analysis and optimization suggestions".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "code_snippet".into(),
                            description: Some("Code snippet or file path to optimize".into()),
                            required: Some(true),
                        },
                        PromptArgument {
                            name: "target_language".into(),
                            description: Some("Programming language (auto-detected if not provided)".into()),
                            required: Some(false),
                        },
                    ]),
                },
                Prompt {
                    name: "security_analyzer".into(),
                    description: Some("Security vulnerability analysis and recommendations".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "file_path".into(),
                            description: Some("Path to the code file to analyze for security issues".into()),
                            required: Some(true),
                        },
                    ]),
                },
                Prompt {
                    name: "documentation_generator".into(),
                    description: Some("Generate comprehensive documentation for code".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "code_file".into(),
                            description: Some("Path to the code file to document".into()),
                            required: Some(true),
                        },
                        PromptArgument {
                            name: "doc_style".into(),
                            description: Some("Documentation style (rustdoc, jsdoc, sphinx, etc.)".into()),
                            required: Some(false),
                        },
                    ]),
                },
                Prompt {
                    name: "test_generator".into(),
                    description: Some("Generate unit tests for code functions and modules".into()),
                    arguments: Some(vec![
                        PromptArgument {
                            name: "source_file".into(),
                            description: Some("Path to the source code file to generate tests for".into()),
                            required: Some(true),
                        },
                        PromptArgument {
                            name: "test_framework".into(),
                            description: Some("Testing framework to use (auto-detected if not provided)".into()),
                            required: Some(false),
                        },
                    ]),
                },
            ],
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        param: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let prompt_content = match param.name.as_ref() {
            "code_review" => {
                let file_path = param.arguments.as_ref()
                    .and_then(|args| args.get("file_path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[file_path]");
                let focus_areas = param.arguments.as_ref()
                    .and_then(|args| args.get("focus_areas"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("all areas");
                
                format!(
                    "Please perform a comprehensive code review of the file: {}\n\n\
                    Focus areas: {}\n\n\
                    Please analyze the code for:\n\
                    - Code quality and maintainability\n\
                    - Potential bugs and logic errors\n\
                    - Security vulnerabilities\n\
                    - Performance issues\n\
                    - Code style and best practices\n\
                    - Documentation completeness\n\n\
                    Provide specific suggestions for improvement with code examples where applicable.",
                    file_path, focus_areas
                )
            }
            "bug_fix_assistant" => {
                let error_message = param.arguments.as_ref()
                    .and_then(|args| args.get("error_message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[error_message]");
                let code_context = param.arguments.as_ref()
                    .and_then(|args| args.get("code_context"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[code_context]");
                
                format!(
                    "Help me fix this bug:\n\n\
                    Error: {}\n\n\
                    Code context: {}\n\n\
                    Please:\n\
                    1. Analyze the error message and identify the root cause\n\
                    2. Examine the code context for potential issues\n\
                    3. Provide a step-by-step solution\n\
                    4. Suggest code fixes with examples\n\
                    5. Recommend preventive measures to avoid similar issues",
                    error_message, code_context
                )
            }
            "performance_optimizer" => {
                let code_snippet = param.arguments.as_ref()
                    .and_then(|args| args.get("code_snippet"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[code_snippet]");
                let target_language = param.arguments.as_ref()
                    .and_then(|args| args.get("target_language"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-detect");
                
                format!(
                    "Optimize this code for better performance:\n\n\
                    Code: {}\n\n\
                    Language: {}\n\n\
                    Please:\n\
                    1. Identify performance bottlenecks\n\
                    2. Suggest algorithmic improvements\n\
                    3. Recommend data structure optimizations\n\
                    4. Provide optimized code examples\n\
                    5. Explain the performance gains expected",
                    code_snippet, target_language
                )
            }
            "security_analyzer" => {
                let file_path = param.arguments.as_ref()
                    .and_then(|args| args.get("file_path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[file_path]");
                
                format!(
                    "Perform a security analysis of the file: {}\n\n\
                    Please check for:\n\
                    - Input validation vulnerabilities\n\
                    - SQL injection risks\n\
                    - Cross-site scripting (XSS) vulnerabilities\n\
                    - Authentication and authorization issues\n\
                    - Data exposure risks\n\
                    - Cryptographic weaknesses\n\
                    - Buffer overflow possibilities\n\
                    - Dependency vulnerabilities\n\n\
                    Provide specific recommendations and secure code examples.",
                    file_path
                )
            }
            "documentation_generator" => {
                let code_file = param.arguments.as_ref()
                    .and_then(|args| args.get("code_file"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[code_file]");
                let doc_style = param.arguments.as_ref()
                    .and_then(|args| args.get("doc_style"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-detect");
                
                format!(
                    "Generate comprehensive documentation for: {}\n\n\
                    Documentation style: {}\n\n\
                    Please create:\n\
                    - Module/class overview\n\
                    - Function/method documentation\n\
                    - Parameter descriptions\n\
                    - Return value documentation\n\
                    - Usage examples\n\
                    - Error handling information\n\
                    - Performance considerations\n\n\
                    Follow the appropriate documentation standards for the language.",
                    code_file, doc_style
                )
            }
            "test_generator" => {
                let source_file = param.arguments.as_ref()
                    .and_then(|args| args.get("source_file"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("[source_file]");
                let test_framework = param.arguments.as_ref()
                    .and_then(|args| args.get("test_framework"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-detect");
                
                format!(
                    "Generate comprehensive unit tests for: {}\n\n\
                    Test framework: {}\n\n\
                    Please create tests that:\n\
                    - Cover all public functions/methods\n\
                    - Test edge cases and error conditions\n\
                    - Include positive and negative test cases\n\
                    - Test boundary conditions\n\
                    - Mock external dependencies\n\
                    - Follow testing best practices\n\
                    - Include setup and teardown if needed\n\n\
                    Provide complete, runnable test code with explanations.",
                    source_file, test_framework
                )
            }
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown prompt: {}", param.name),
                    None,
                ));
            }
        };

        Ok(GetPromptResult {
            description: Some(format!("Generated prompt for {}", param.name)),
            messages: vec![PromptMessage {
                role: PromptMessageRole::User,
                content: PromptMessageContent::Text {
                    text: prompt_content,
                },
            }],
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

        // Read file content
        let code = match tokio::fs::read_to_string(file_path).await {
            Ok(content) => content,
            Err(e) => {
                return Err(McpError::internal_error(
                    format!("Failed to read file {}: {}", file_path, e),
                    None,
                ));
            }
        };

        // Try DashScope first (primary)
        if let Some(dashscope_client) = self.get_dashscope_client().await {
            match dashscope_client
                .analyze_code(&code, language.as_deref())
                .await
            {
                Ok(result) => {
                    let analysis_result = format!(
                        "## 🔍 AI Code Analysis: {}\n\n{}\n\n*Analyzed using DashScope/Qwen3 AI*",
                        file_path, result
                    );
                    info!("DashScope code analysis completed for: {}", file_path);
                    return Ok(CallToolResult::success(vec![Content::text(
                        analysis_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "DashScope analysis failed for {}: {}, trying NVIDIA fallback",
                        file_path, e
                    );
                }
            }
        }

        // Try NVIDIA as fallback 1
        if let Some(nvidia_client) = self.get_nvidia_client().await {
            match nvidia_client.analyze_code(&code, language.as_deref()).await {
                Ok(result) => {
                    let issues_text = if result.issues.is_empty() {
                        "No issues found.".to_string()
                    } else {
                        result
                            .issues
                            .iter()
                            .map(|issue| format!("• [{}] {}", issue.severity, issue.message))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };

                    let suggestions_text = if result.suggestions.is_empty() {
                        "".to_string()
                    } else {
                        format!(
                            "\n\n### Suggestions:\n{}",
                            result
                                .suggestions
                                .iter()
                                .map(|s| format!("• {}", s))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };

                    let complexity_text = result
                        .complexity_score
                        .map(|score| format!("\n\n### Complexity Score: {}/100", score))
                        .unwrap_or_default();

                    let analysis_result = format!(
                        "## 🔍 AI Code Analysis: {}\n\n**Summary:** {}\n\n### Issues Found ({}):\n{}{}{}\n\n*DashScope failed, analyzed using NVIDIA AI*",
                        file_path,
                        result.summary,
                        result.issues.len(),
                        issues_text,
                        suggestions_text,
                        complexity_text
                    );

                    info!(
                        "NVIDIA fallback code analysis completed for: {} ({} issues found)",
                        file_path,
                        result.issues.len()
                    );
                    return Ok(CallToolResult::success(vec![Content::text(
                        analysis_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "NVIDIA analysis failed for {}: {}, trying Gemini fallback",
                        file_path, e
                    );
                }
            }
        }

        // Try Gemini as fallback 2
        if let Some(gemini_client) = self.get_gemini_client().await {
            match gemini_client.analyze_code(&code, language.as_deref()).await {
                Ok(gemini_result) => {
                    let analysis_result = format!(
                        "## 🔍 AI Code Analysis: {}\n\n{}\n\n*DashScope and NVIDIA failed, analyzed using Gemini AI*",
                        file_path, gemini_result
                    );
                    info!("Gemini fallback code analysis completed for: {}", file_path);
                    return Ok(CallToolResult::success(vec![Content::text(
                        analysis_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "All AI providers failed for analysis of {}: Gemini: {}",
                        file_path, e
                    );
                }
            }
        }

        // All AI providers failed - provide basic fallback
        let fallback = format!(
            "## 📄 Basic Code Analysis: {}\n\nAll AI providers unavailable:\n- DashScope: missing DASHSCOPE_API_KEY or failed\n- NVIDIA: missing NVIDIA_API_KEY or failed\n- Gemini: missing GEMINI_API_KEY or failed\n\nBasic analysis: File exists and appears to be valid {} code.",
            file_path,
            language.as_deref().unwrap_or("source")
        );

        info!(
            "Basic code analysis completed for: {} (all AI providers failed)",
            file_path
        );
        Ok(CallToolResult::success(vec![Content::text(fallback)]))
    }

    async fn handle_ai_generate_code(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_request("Missing prompt", None))?;

        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let max_tokens = args
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let temperature = args
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32);

        // Try DashScope first (primary)
        if let Some(dashscope_client) = self.get_dashscope_client().await {
            match dashscope_client
                .generate_code(
                    prompt,
                    language.as_deref(),
                    context.as_deref(),
                    max_tokens,
                    temperature,
                )
                .await
            {
                Ok(result) => {
                    let formatted_result = format!(
                        "## 🤖 AI Generated Code\n\n{}\n\n*Generated using DashScope/Qwen3 AI*",
                        result
                    );
                    info!(
                        "DashScope code generation completed for prompt: '{}'",
                        prompt
                    );
                    return Ok(CallToolResult::success(vec![Content::text(
                        formatted_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "DashScope code generation failed: {}, trying NVIDIA fallback",
                        e
                    );
                }
            }
        }

        // Try NVIDIA as fallback 1
        if let Some(nvidia_client) = self.get_nvidia_client().await {
            let request = crate::nvidia::models::CodeGenerationRequest {
                prompt: prompt.to_string(),
                language: language.clone(),
                context: context.clone(),
                max_tokens,
                temperature,
            };

            match nvidia_client.generate_code(&request).await {
                Ok(result) => {
                    let formatted_result = format!(
                        "## 🤖 AI Generated Code\n\n### Language: {}\n\n```{}\n{}\n```\n\n*DashScope failed, generated using NVIDIA AI*",
                        result.language.as_deref().unwrap_or("auto-detected"),
                        result.language.as_deref().unwrap_or(""),
                        result.code
                    );

                    info!(
                        "NVIDIA fallback code generation completed for prompt: '{}'",
                        prompt
                    );
                    return Ok(CallToolResult::success(vec![Content::text(
                        formatted_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "NVIDIA code generation failed: {}, trying Gemini fallback",
                        e
                    );
                }
            }
        }

        // Try Gemini as fallback 2
        if let Some(gemini_client) = self.get_gemini_client().await {
            match gemini_client
                .generate_code(
                    prompt,
                    language.as_deref(),
                    context.as_deref(),
                    max_tokens,
                    temperature,
                )
                .await
            {
                Ok(gemini_result) => {
                    let formatted_result = format!(
                        "## 🤖 AI Generated Code\n\n{}\n\n*DashScope and NVIDIA failed, generated using Gemini AI*",
                        gemini_result
                    );
                    info!(
                        "Gemini fallback code generation completed for prompt: '{}'",
                        prompt
                    );
                    return Ok(CallToolResult::success(vec![Content::text(
                        formatted_result,
                    )]));
                }
                Err(e) => {
                    warn!("All AI providers failed for code generation: Gemini: {}", e);
                }
            }
        }

        // All AI providers failed
        let fallback = format!(
            "## 📝 Code Generation Not Available\n\nAll AI providers unavailable:\n- DashScope: missing DASHSCOPE_API_KEY or failed\n- NVIDIA: missing NVIDIA_API_KEY or failed\n- Gemini: missing GEMINI_API_KEY or failed\n\nPrompt: {}\nLanguage: {}",
            prompt,
            language.as_deref().unwrap_or("not specified")
        );

        info!("Code generation requested but all AI providers failed");
        Ok(CallToolResult::success(vec![Content::text(fallback)]))
    }

    async fn handle_ai_explain_code(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;

        let file_path = args.get("file_path").and_then(|v| v.as_str());
        let code_snippet = args.get("code").and_then(|v| v.as_str());
        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let detail_level = args
            .get("detail_level")
            .and_then(|v| v.as_str())
            .unwrap_or("detailed");

        // Must provide either file_path or code
        if file_path.is_none() && code_snippet.is_none() {
            return Err(McpError::invalid_request(
                "Must provide either file_path or code",
                None,
            ));
        }

        let (code, source_info) = if let Some(path) = file_path {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => (content, format!("file: {}", path)),
                Err(e) => {
                    return Err(McpError::internal_error(
                        format!("Failed to read file {}: {}", path, e),
                        None,
                    ));
                }
            }
        } else {
            (
                code_snippet.unwrap().to_string(),
                "provided snippet".to_string(),
            )
        };

        // Try DashScope first (primary)
        if let Some(dashscope_client) = self.get_dashscope_client().await {
            match dashscope_client
                .explain_code(&code, language.as_deref(), detail_level)
                .await
            {
                Ok(result) => {
                    let formatted_result = format!(
                        "## 📚 AI Code Explanation\n\n**Source:** {}\n**Detail Level:** {}\n\n{}\n\n*Explained using DashScope/Qwen3 AI*",
                        source_info, detail_level, result
                    );
                    info!("DashScope code explanation completed for: {}", source_info);
                    return Ok(CallToolResult::success(vec![Content::text(
                        formatted_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "DashScope explanation failed: {}, trying NVIDIA fallback",
                        e
                    );
                }
            }
        }

        // Try NVIDIA as fallback 1
        if let Some(nvidia_client) = self.get_nvidia_client().await {
            let detail_prompt = match detail_level {
                "basic" => "Provide a brief, high-level explanation of what this code does.",
                "expert" => "Provide a comprehensive, expert-level analysis including architecture, patterns, potential issues, and optimization opportunities.",
                _ => "Provide a detailed explanation of this code including its purpose, how it works, and key concepts."
            };

            let system_prompt = format!("You are a code explanation expert. {}", detail_prompt);

            let user_prompt = if let Some(lang) = &language {
                format!("Explain this {} code:\n\n```{}\n{}\n```", lang, lang, code)
            } else {
                format!("Explain this code:\n\n```\n{}\n```", code)
            };

            let request = crate::nvidia::models::ChatCompletionRequest {
                model: nvidia_client
                    .recommend_model(crate::nvidia::models::TaskType::CodeExplanation)
                    .as_str()
                    .to_string(),
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
                            "## 📚 AI Code Explanation\n\n**Source:** {}\n**Detail Level:** {}\n\n{}\n\n*DashScope failed, explained using: {}*",
                            source_info,
                            detail_level,
                            explanation,
                            request.model
                        );

                        info!(
                            "NVIDIA fallback code explanation completed for: {}",
                            source_info
                        );
                        return Ok(CallToolResult::success(vec![Content::text(
                            formatted_result,
                        )]));
                    } else {
                        warn!("Empty response from NVIDIA API, trying Gemini fallback");
                    }
                }
                Err(e) => {
                    warn!("NVIDIA explanation failed: {}, trying Gemini fallback", e);
                }
            }
        }

        // Try Gemini as fallback 2
        if let Some(gemini_client) = self.get_gemini_client().await {
            match gemini_client
                .explain_code(&code, language.as_deref(), detail_level)
                .await
            {
                Ok(gemini_result) => {
                    let formatted_result = format!(
                        "## 📚 AI Code Explanation\n\n**Source:** {}\n**Detail Level:** {}\n\n{}\n\n*DashScope and NVIDIA failed, explained using Gemini AI*",
                        source_info, detail_level, gemini_result
                    );
                    info!(
                        "Gemini fallback code explanation completed for: {}",
                        source_info
                    );
                    return Ok(CallToolResult::success(vec![Content::text(
                        formatted_result,
                    )]));
                }
                Err(e) => {
                    warn!(
                        "All AI providers failed for code explanation: Gemini: {}",
                        e
                    );
                }
            }
        }

        // All AI providers failed
        let fallback = format!(
            "## 📖 Code Explanation Not Available\n\nAll AI providers unavailable:\n- DashScope: missing DASHSCOPE_API_KEY or failed\n- NVIDIA: missing NVIDIA_API_KEY or failed\n- Gemini: missing GEMINI_API_KEY or failed\n\nSource: {}\nDetail Level: {}",
            source_info,
            detail_level
        );

        info!("Code explanation requested but all AI providers failed");
        Ok(CallToolResult::success(vec![Content::text(fallback)]))
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create service and initialize NVIDIA integration
    let service = WinxService::new();

    // Initialize DashScope integration (primary)
    if let Ok(enabled) = service.initialize_dashscope().await {
        if enabled {
            info!("DashScope AI integration enabled successfully (primary)");
        } else {
            warn!("DashScope AI features will be limited without valid DASHSCOPE_API_KEY");
        }
    } else {
        warn!("Failed to initialize DashScope integration");
    }

    // Initialize NVIDIA integration (fallback 1)
    if let Ok(enabled) = service.initialize_nvidia().await {
        if enabled {
            info!("NVIDIA AI integration enabled successfully (fallback 1)");
        } else {
            warn!("NVIDIA AI features will be limited without valid NVIDIA_API_KEY");
        }
    } else {
        warn!("Failed to initialize NVIDIA integration");
    }

    // Initialize Gemini integration (fallback 2)
    if let Ok(enabled) = service.initialize_gemini().await {
        if enabled {
            info!("Gemini AI integration enabled successfully (fallback 2)");
        } else {
            warn!("Gemini AI fallback features will be limited without valid GEMINI_API_KEY");
        }
    } else {
        warn!("Failed to initialize Gemini integration");
    }

    // Create and run the server with STDIO transport
    let server = service.serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {}", e);
    })?;
    server.waiting().await?;

    Ok(())
}
