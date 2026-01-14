//! Ollama Provider
//!
//! Modelos locais via Ollama (OpenAI-compatible API).

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    ChatOptions, ChatResponse, EventStream, FunctionDef, Message, MessageContent, ModelInfo,
    OllamaConfig, Provider, ProviderError, StreamEvent, ToolCall, Usage,
};

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434/v1/chat/completions";

/// Provider para Ollama (modelos locais)
pub struct OllamaProvider {
    client: Client,
    base_url: String,
    default_model: String,
}

impl OllamaProvider {
    /// Cria provider com URL padrão
    pub fn new(config: &OllamaConfig) -> Self {
        Self {
            client: Client::new(),
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string()),
            default_model: config
                .default_model
                .clone()
                .unwrap_or_else(|| "llama3.2".to_string()),
        }
    }

    /// Converte mensagens para formato `OpenAI`
    fn convert_messages(&self, messages: &[Message]) -> Vec<OllamaMessage> {
        messages.iter().map(|m| self.convert_message(m)).collect()
    }

    fn convert_message(&self, msg: &Message) -> OllamaMessage {
        let content = match &msg.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let super::ContentPart::Text { text } = p {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            MessageContent::ToolResult { content, .. } => content.clone(),
        };

        OllamaMessage {
            role: msg.role.as_str().to_string(),
            content,
        }
    }

    fn convert_tools(&self, tools: &[FunctionDef]) -> Vec<OllamaTool> {
        tools
            .iter()
            .map(|t| OllamaTool {
                r#type: "function".to_string(),
                function: OllamaFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    /// Verifica se Ollama está disponível
    pub async fn is_available(&self) -> bool {
        let health_url = self.base_url.replace("/v1/chat/completions", "/api/tags");

        self.client.get(&health_url).send().await.is_ok()
    }

    /// Lista modelos instalados no Ollama
    pub async fn list_installed_models(&self) -> Result<Vec<String>, ProviderError> {
        let tags_url = self.base_url.replace("/v1/chat/completions", "/api/tags");

        let response = self
            .client
            .get(&tags_url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::ApiError("Failed to list models".to_string()));
        }

        let data: OllamaTagsResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(data.models.into_iter().map(|m| m.name).collect())
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            default_model: None,
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Modelos comuns do Ollama (lista estática)
        vec![
            ModelInfo::new("llama3.2", "Llama 3.2 - General purpose").with_context(131072),
            ModelInfo::new("llama3.2:1b", "Llama 3.2 1B - Fast").with_context(131072),
            ModelInfo::new("codellama", "Code Llama - Coding").with_context(16384),
            ModelInfo::new("deepseek-coder-v2", "DeepSeek Coder v2").with_context(131072),
            ModelInfo::new("qwen2.5-coder", "Qwen 2.5 Coder").with_context(131072),
            ModelInfo::new("mistral", "Mistral 7B").with_context(32768),
            ModelInfo::new("mixtral", "Mixtral 8x7B MoE").with_context(32768),
            ModelInfo::new("phi3", "Phi-3 Mini").with_context(131072),
            ModelInfo::new("gemma2", "Gemma 2").with_context(8192),
        ]
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports_vision(&self) -> bool {
        false // Ollama tem suporte limitado
    }

    fn supports_tools(&self) -> bool {
        true // Alguns modelos suportam
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        options: &ChatOptions,
    ) -> Result<EventStream, ProviderError> {
        let model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        let mut body = json!({
            "model": model,
            "messages": self.convert_messages(messages),
            "stream": true,
        });

        if let Some(max_tokens) = options.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(ref tools) = options.tools {
            body["tools"] = json!(self.convert_tools(tools));
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ProviderError::ConfigError("Ollama not running. Start with: ollama serve".to_string())
                } else {
                    ProviderError::NetworkError(e.to_string())
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            return Err(ProviderError::ApiError(format!("{status}: {text}")));
        }

        let mut stream = response.bytes_stream();

        let output_stream = stream! {
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    yield StreamEvent::Done;
                                    break;
                                }

                                if let Some(event) = parse_stream_chunk(data) {
                                    yield event;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield StreamEvent::Error(e.to_string());
                        break;
                    }
                }
            }

            yield StreamEvent::Done;
        };

        Ok(Box::pin(output_stream))
    }

    async fn chat(
        &self,
        messages: &[Message],
        options: &ChatOptions,
    ) -> Result<ChatResponse, ProviderError> {
        let model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        let mut body = json!({
            "model": model,
            "messages": self.convert_messages(messages),
            "stream": false,
        });

        if let Some(max_tokens) = options.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(ref tools) = options.tools {
            body["tools"] = json!(self.convert_tools(tools));
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ProviderError::ConfigError("Ollama not running".to_string())
                } else {
                    ProviderError::NetworkError(e.to_string())
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            return Err(ProviderError::ApiError(format!("{status}: {text}")));
        }

        let api_response: OllamaResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let choice = api_response
            .choices
            .first()
            .ok_or_else(|| ProviderError::ParseError("No choices".to_string()))?;

        let content = choice.message.content.clone().unwrap_or_default();

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls
                    .iter()
                    .map(|tc| ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: serde_json::from_str(&tc.function.arguments).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            content,
            tool_calls,
            usage: api_response.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
            finish_reason: choice.finish_reason.clone(),
        })
    }
}

// Tipos OpenAI-compatible

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    r#type: String,
    function: OllamaFunction,
}

#[derive(Debug, Serialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    choices: Vec<OllamaChoice>,
    usage: Option<OllamaUsage>,
}

#[derive(Debug, Deserialize)]
struct OllamaChoice {
    message: OllamaResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    id: String,
    function: OllamaToolFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OllamaUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

fn parse_stream_chunk(data: &str) -> Option<StreamEvent> {
    let chunk: StreamChunk = serde_json::from_str(data).ok()?;
    let choice = chunk.choices.first()?;

    if let Some(ref content) = choice.delta.content {
        if !content.is_empty() {
            return Some(StreamEvent::Text(content.clone()));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_models() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(&config);
        let models = provider.models();

        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.id.contains("llama")));
    }

    #[test]
    fn test_message_conversion() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(&config);

        let messages = vec![Message::user("Hello"), Message::assistant("Hi!")];

        let converted = provider.convert_messages(&messages);

        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "user");
        assert_eq!(converted[0].content, "Hello");
    }

    #[test]
    fn test_default_config() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(&config);

        assert_eq!(provider.default_model(), "llama3.2");
        assert!(provider.base_url.contains("localhost:11434"));
    }
}
