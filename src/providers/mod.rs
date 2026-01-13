//! LLM Provider Abstraction Layer
//!
//! Suporta múltiplos providers de LLM com interface unificada:
//! - Claude (Anthropic)
//! - OpenAI
//! - NVIDIA NIM (nosso pool)
//! - Gemini
//! - Ollama (local)

mod claude;
mod nvidia;
mod ollama;
mod openai;

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use claude::ClaudeProvider;
pub use nvidia::NvidiaProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;

/// Evento de streaming
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Texto parcial
    Text(String),
    /// Tool call iniciado
    ToolCallStart { id: String, name: String },
    /// Argumentos parciais da tool
    ToolCallDelta { id: String, arguments: String },
    /// Tool call completo
    ToolCallEnd { id: String },
    /// Fim do stream
    Done,
    /// Erro
    Error(String),
}

/// Tipo do stream de eventos
pub type EventStream = Pin<Box<dyn Stream<Item = StreamEvent> + Send>>;

/// Informações sobre um modelo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context_length: usize,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

impl ModelInfo {
    pub fn new(id: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            description: description.to_string(),
            context_length: 8192,
            supports_tools: true,
            supports_vision: false,
        }
    }

    pub fn with_context(mut self, length: usize) -> Self {
        self.context_length = length;
        self
    }

    pub fn with_vision(mut self) -> Self {
        self.supports_vision = true;
        self
    }
}

/// Mensagem de chat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    pub fn user(content: &str) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(content.to_string()),
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.to_string()),
        }
    }

    pub fn system(content: &str) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(content.to_string()),
        }
    }

    pub fn tool_result(tool_use_id: &str, content: &str) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
            },
        }
    }

    /// Retorna conteúdo como texto (simplificado)
    pub fn text(&self) -> &str {
        match &self.content {
            MessageContent::Text(t) => t,
            MessageContent::Parts(parts) => {
                parts.iter().find_map(|p| {
                    if let ContentPart::Text { text } = p {
                        Some(text.as_str())
                    } else {
                        None
                    }
                }).unwrap_or("")
            }
            MessageContent::ToolResult { content, .. } => content,
        }
    }
}

/// Role da mensagem
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// Conteúdo da mensagem
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// Parte do conteúdo (multimodal)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image_url")]
    Image { image_url: ImageUrl },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

/// Definição de função/tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Opções de chat
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub tools: Option<Vec<FunctionDef>>,
    pub system: Option<String>,
    pub stream: bool,
}

/// Resposta completa
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

/// Tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Uso de tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

/// Trait principal para providers
#[async_trait]
pub trait Provider: Send + Sync {
    /// Nome do provider
    fn name(&self) -> &str;

    /// Modelos disponíveis
    fn models(&self) -> Vec<ModelInfo>;

    /// Modelo padrão
    fn default_model(&self) -> &str;

    /// Chat completion com streaming
    async fn chat_stream(
        &self,
        messages: &[Message],
        options: &ChatOptions,
    ) -> Result<EventStream, ProviderError>;

    /// Chat completion blocking
    async fn chat(
        &self,
        messages: &[Message],
        options: &ChatOptions,
    ) -> Result<ChatResponse, ProviderError>;

    /// Suporta tools/function calling?
    fn supports_tools(&self) -> bool {
        true
    }

    /// Suporta visão?
    fn supports_vision(&self) -> bool {
        false
    }
}

/// Erros do provider
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("API error: {0}")]
    ApiError(String),

    #[error("Rate limit exceeded")]
    RateLimited,

    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// Configuração global de providers
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,

    pub claude: Option<ClaudeConfig>,
    pub openai: Option<OpenAIConfig>,
    pub nvidia: Option<NvidiaConfig>,
    pub ollama: Option<OllamaConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeConfig {
    pub api_key: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaConfig {
    pub api_keys: Vec<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    pub base_url: Option<String>,
    pub default_model: Option<String>,
}

/// Registry de providers
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn Provider>>,
    default_provider: Option<String>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            default_provider: None,
        }
    }

    /// Registra um provider
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.push(provider);
    }

    /// Define provider padrão
    pub fn set_default(&mut self, name: &str) {
        self.default_provider = Some(name.to_string());
    }

    /// Obtém provider por nome
    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.iter().find(|p| p.name() == name).cloned()
    }

    /// Obtém provider padrão
    pub fn default(&self) -> Option<Arc<dyn Provider>> {
        if let Some(ref name) = self.default_provider {
            self.get(name)
        } else {
            self.providers.first().cloned()
        }
    }

    /// Lista todos os providers
    pub fn list(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Lista todos os modelos de todos os providers
    pub fn all_models(&self) -> Vec<(String, ModelInfo)> {
        self.providers
            .iter()
            .flat_map(|p| {
                p.models()
                    .into_iter()
                    .map(|m| (format!("{}:{}", p.name(), m.id), m))
            })
            .collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Cria registry com providers configurados
pub fn create_registry(config: &ProvidersConfig) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();

    // Claude
    if let Some(ref claude_config) = config.claude {
        if let Some(provider) = ClaudeProvider::from_config(claude_config) {
            registry.register(Arc::new(provider));
        }
    } else if let Ok(provider) = ClaudeProvider::from_env() {
        registry.register(Arc::new(provider));
    }

    // OpenAI
    if let Some(ref openai_config) = config.openai {
        if let Some(provider) = OpenAIProvider::from_config(openai_config) {
            registry.register(Arc::new(provider));
        }
    } else if let Ok(provider) = OpenAIProvider::from_env() {
        registry.register(Arc::new(provider));
    }

    // NVIDIA
    if let Some(ref nvidia_config) = config.nvidia {
        if let Some(provider) = NvidiaProvider::from_config(nvidia_config) {
            registry.register(Arc::new(provider));
        }
    } else if let Ok(provider) = NvidiaProvider::from_env() {
        registry.register(Arc::new(provider));
    }

    // Ollama (local, sempre disponível)
    let ollama_config = config.ollama.clone().unwrap_or_default();
    registry.register(Arc::new(OllamaProvider::new(&ollama_config)));

    // Define default
    if let Some(ref default) = config.default_provider {
        registry.set_default(default);
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.text(), "Hello");

        let msg = Message::assistant("Hi there");
        assert_eq!(msg.role, Role::Assistant);
    }

    #[test]
    fn test_model_info() {
        let model = ModelInfo::new("gpt-4", "Latest GPT-4")
            .with_context(128000)
            .with_vision();

        assert_eq!(model.id, "gpt-4");
        assert_eq!(model.context_length, 128000);
        assert!(model.supports_vision);
    }

    #[test]
    fn test_registry() {
        let registry = ProviderRegistry::new();
        assert!(registry.list().is_empty());
    }
}
