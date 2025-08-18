//! Winx MCP Server implementation using rmcp 0.5.0
//! Enhanced server with NVIDIA AI integration

use rmcp::{model::*, transport::stdio, ServerHandler, ServiceExt, tool, ErrorData as McpError, schemars};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::nvidia::{NvidiaClient, NvidiaConfig};
use crate::nvidia::tools::code_analysis::{analyze_code_with_ai, AnalyzeCodeParams};
use crate::state::BashState;

/// Winx service with shared bash state and NVIDIA AI integration
#[derive(Clone)]
#[tool(tool_box)]
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
            Ok(config) => {
                match crate::nvidia::initialize(config).await {
                    Ok(client) => {
                        *self.nvidia_client.lock().await = Some(client);
                        info!("NVIDIA AI integration initialized successfully");
                        Ok(true)
                    }
                    Err(e) => {
                        warn!("Failed to initialize NVIDIA integration: {}", e);
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                info!("NVIDIA integration not available: {}", e);
                Ok(false)
            }
        }
    }

    /// Get NVIDIA client if available
    pub async fn get_nvidia_client(&self) -> Option<NvidiaClient> {
        self.nvidia_client.lock().await.clone()
    }

    /// AI-powered code analysis tool using NVIDIA models
    #[tool(description = "Analyze code for bugs, security issues, performance problems, and style violations using NVIDIA AI")]
    async fn analyze_code(
        &self,
        #[tool(aggr)]
        params: AnalyzeCodeParams,
    ) -> String {
        match self.get_nvidia_client().await {
            Some(nvidia_client) => {
                match analyze_code_with_ai(&nvidia_client, params).await {
                    Ok(result) => {
                        serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|e| format!("Error serializing result: {}", e))
                    }
                    Err(e) => {
                        format!("AI code analysis failed: {}", e)
                    }
                }
            }
            None => {
                "NVIDIA AI integration not available. Please set NVIDIA_API_KEY environment variable.".to_string()
            }
        }
    }

    /// AI-powered code generation tool using NVIDIA models
    #[tool(description = "Generate code from natural language descriptions using NVIDIA AI")]
    async fn generate_code(
        &self,
        #[tool(param)]
        #[schemars(description = "Natural language description of the code to generate")]
        prompt: String,
        #[tool(param)]
        #[schemars(description = "Programming language (e.g., Rust, Python, JavaScript)")]
        language: Option<String>,
        #[tool(param)]
        #[schemars(description = "Additional context or constraints")]
        context: Option<String>,
        #[tool(param)]
        #[schemars(description = "Maximum tokens to generate")]
        max_tokens: Option<u32>,
        #[tool(param)]
        #[schemars(description = "Temperature for creativity (0.0-1.0)")]
        temperature: Option<f32>,
    ) -> String {
        match self.get_nvidia_client().await {
            Some(nvidia_client) => {
                let request = crate::nvidia::models::CodeGenerationRequest {
                    prompt,
                    language,
                    context,
                    max_tokens,
                    temperature,
                };

                match nvidia_client.generate_code(&request).await {
                    Ok(result) => {
                        serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|e| format!("Error serializing result: {}", e))
                    }
                    Err(e) => {
                        format!("AI code generation failed: {}", e)
                    }
                }
            }
            None => {
                "NVIDIA AI integration not available. Please set NVIDIA_API_KEY environment variable.".to_string()
            }
        }
    }

    /// AI-powered code explanation tool using NVIDIA models
    #[tool(description = "Get detailed explanations of code using NVIDIA AI")]
    async fn explain_code(
        &self,
        #[tool(param)]
        #[schemars(description = "Path to the file to explain")]
        file_path: Option<String>,
        #[tool(param)]
        #[schemars(description = "Code content to explain")]
        code: Option<String>,
        #[tool(param)]
        #[schemars(description = "Programming language")]
        language: Option<String>,
        #[tool(param)]
        #[schemars(description = "Level of detail (brief, detailed, comprehensive)")]
        detail_level: Option<String>,
    ) -> String {
        match self.get_nvidia_client().await {
            Some(nvidia_client) => {
                // Get code content
                let code_content = match (&file_path, &code) {
                    (Some(path), _) => {
                        match tokio::fs::read_to_string(path).await {
                            Ok(content) => content,
                            Err(e) => return format!("Failed to read file {}: {}", path, e),
                        }
                    }
                    (None, Some(code)) => code.clone(),
                    (None, None) => return "Either file_path or code must be provided".to_string(),
                };

                let detail = detail_level.as_deref().unwrap_or("detailed");
                let language_context = language.as_ref()
                    .map(|l| format!(" (written in {})", l))
                    .unwrap_or_default();

                let system_prompt = format!(
                    "You are an expert code explainer. Provide a {} explanation of the code{}, including its purpose, how it works, and any important details.",
                    detail, language_context
                );

                let user_prompt = format!("Explain this code:\n\n```\n{}\n```", code_content);

                let request = crate::nvidia::models::ChatCompletionRequest {
                    model: crate::nvidia::models::NvidiaModel::for_task(
                        crate::nvidia::models::TaskType::CodeExplanation
                    ).as_str().to_string(),
                    messages: vec![
                        crate::nvidia::models::ChatMessage::system(system_prompt),
                        crate::nvidia::models::ChatMessage::user(user_prompt),
                    ],
                    max_tokens: Some(2048),
                    temperature: Some(0.1),
                    top_p: None,
                    stream: Some(false),
                };

                match nvidia_client.chat_completion(&request).await {
                    Ok(response) => {
                        if let Some(choice) = response.choices.first() {
                            choice.message.content.clone()
                        } else {
                            "Empty response from NVIDIA API".to_string()
                        }
                    }
                    Err(e) => {
                        format!("AI code explanation failed: {}", e)
                    }
                }
            }
            None => {
                "NVIDIA AI integration not available. Please set NVIDIA_API_KEY environment variable.".to_string()
            }
        }
    }
}

// ServerHandler implementation with NVIDIA tools
#[tool(tool_box)]
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
                .build(),
            instructions: Some(
                "Winx is a high-performance Rust implementation of WCGW for code agents with NVIDIA AI integration. \
                Provides shell execution, file management, and AI-powered code analysis capabilities.".into(),
            ),
        }
    }
}

/// Create and start the Winx MCP server
pub async fn start_winx_server() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Winx MCP Server using rmcp 0.5.0");

    // Create service and initialize NVIDIA integration
    let service = WinxService::new();
    
    // Attempt to initialize NVIDIA integration (non-blocking)
    if let Err(e) = service.initialize_nvidia().await {
        warn!("Could not initialize NVIDIA integration: {}", e);
    }

    // Create and run the server with STDIO transport
    let server = service.serve(stdio()).await.inspect_err(|e| {
        eprintln!("Error starting server: {}", e);
    })?;
    server.waiting().await?;

    Ok(())
}
