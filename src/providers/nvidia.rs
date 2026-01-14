//! NVIDIA NIM Provider
//!
//! Usa API OpenAI-compatible da NVIDIA com nosso pool de keys rotativo.

use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use super::{
    ChatOptions, ChatResponse, EventStream, FunctionDef, Message, MessageContent, ModelInfo,
    NvidiaConfig, Provider, ProviderError, Role, StreamEvent, ToolCall, Usage,
};

const NVIDIA_API_URL: &str = "https://integrate.api.nvidia.com/v1/chat/completions";

/// Pool rotativo de API keys NVIDIA
pub struct NvidiaKeyPool {
    keys: Vec<String>,
    current: AtomicUsize,
    usage: Arc<RwLock<Vec<KeyUsage>>>,
}

#[derive(Debug, Default, Clone)]
struct KeyUsage {
    requests: usize,
    last_used: u64,
    rate_limited: bool,
}

impl NvidiaKeyPool {
    pub fn new(keys: Vec<String>) -> Self {
        let usage = vec![KeyUsage::default(); keys.len()];
        Self {
            keys,
            current: AtomicUsize::new(0),
            usage: Arc::new(RwLock::new(usage)),
        }
    }

    /// Obtém próxima key disponível (round-robin, pula rate-limited)
    pub async fn get_key(&self) -> Option<String> {
        if self.keys.is_empty() {
            return None;
        }

        let usage = self.usage.read().await;
        let start = self.current.fetch_add(1, Ordering::Relaxed) % self.keys.len();

        // Tenta encontrar uma key não rate-limited
        for i in 0..self.keys.len() {
            let idx = (start + i) % self.keys.len();
            if !usage[idx].rate_limited {
                return Some(self.keys[idx].clone());
            }
        }

        // Se todas estão rate-limited, usa a primeira
        Some(self.keys[start].clone())
    }

    /// Marca key como rate-limited
    pub async fn mark_rate_limited(&self, key: &str) {
        let mut usage = self.usage.write().await;
        if let Some(idx) = self.keys.iter().position(|k| k == key) {
            usage[idx].rate_limited = true;
        }
    }

    /// Reset rate limits (chamar periodicamente)
    pub async fn reset_rate_limits(&self) {
        let mut usage = self.usage.write().await;
        for u in usage.iter_mut() {
            u.rate_limited = false;
        }
    }
}

pub struct NvidiaProvider {
    client: Client,
    pool: NvidiaKeyPool,
    default_model: String,
}

impl NvidiaProvider {
    pub fn new(keys: Vec<String>) -> Self {
        Self {
            client: Client::new(),
            pool: NvidiaKeyPool::new(keys),
            default_model: "meta/llama-3.3-70b-instruct".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        // Tenta múltiplas keys (NVIDIA_API_KEY, NVIDIA_API_KEY_1, etc.)
        let mut keys = Vec::new();

        if let Ok(key) = env::var("NVIDIA_API_KEY") {
            keys.push(key);
        }

        for i in 1..=10 {
            if let Ok(key) = env::var(format!("NVIDIA_API_KEY_{i}")) {
                keys.push(key);
            }
        }

        if keys.is_empty() {
            return Err(ProviderError::ConfigError(
                "NVIDIA_API_KEY not set".to_string(),
            ));
        }

        Ok(Self::new(keys))
    }

    pub fn from_config(config: &NvidiaConfig) -> Option<Self> {
        let mut keys = config.api_keys.clone();

        // Também tenta env vars
        if keys.is_empty() {
            if let Ok(key) = env::var("NVIDIA_API_KEY") {
                keys.push(key);
            }
        }

        if keys.is_empty() {
            return None;
        }

        let mut provider = Self::new(keys);
        if let Some(ref model) = config.default_model {
            provider.default_model = model.clone();
        }
        Some(provider)
    }

    fn convert_messages(&self, messages: &[Message]) -> Vec<OpenAIMessage> {
        messages.iter().map(|m| self.convert_message(m)).collect()
    }

    fn convert_message(&self, msg: &Message) -> OpenAIMessage {
        let role = msg.role.as_str().to_string();

        let content = match &msg.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => {
                // NVIDIA API geralmente só suporta texto
                parts
                    .iter()
                    .filter_map(|p| {
                        if let super::ContentPart::Text { text } = p {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            MessageContent::ToolResult { content, .. } => content.clone(),
        };

        OpenAIMessage {
            role,
            content,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn convert_tools(&self, tools: &[FunctionDef]) -> Vec<OpenAITool> {
        tools
            .iter()
            .map(|t| OpenAITool {
                r#type: "function".to_string(),
                function: OpenAIFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    async fn request_with_retry(
        &self,
        body: Value,
        stream: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let max_retries = 3;

        for attempt in 0..max_retries {
            let api_key = self
                .pool
                .get_key()
                .await
                .ok_or_else(|| ProviderError::ConfigError("No API keys available".to_string()))?;

            let response = self
                .client
                .post(NVIDIA_API_URL)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if response.status().is_success() {
                return Ok(response);
            }

            let status = response.status();

            if status.as_u16() == 429 {
                // Rate limited - marca key e tenta outra
                self.pool.mark_rate_limited(&api_key).await;
                if attempt < max_retries - 1 {
                    continue;
                }
            }

            let text = response.text().await.unwrap_or_default();

            return Err(match status.as_u16() {
                401 => ProviderError::InvalidApiKey,
                429 => ProviderError::RateLimited,
                _ => ProviderError::ApiError(format!("{status}: {text}")),
            });
        }

        Err(ProviderError::RateLimited)
    }
}

#[async_trait]
impl Provider for NvidiaProvider {
    fn name(&self) -> &'static str {
        "nvidia"
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            // Agentic Coding (2025 Latest)
            ModelInfo::new(
                "qwen/qwen3-next-80b-a3b-instruct",
                "Qwen3-Next 80B - Agentic coding & browser use",
            )
            .with_context(256000),
            ModelInfo::new(
                "qwen/qwen3-coder-480b-a35b-instruct",
                "Qwen3 Coder 480B - Best for complex code",
            )
            .with_context(256000),
            ModelInfo::new(
                "mistralai/devstral-123b-instruct",
                "Devstral 123B - Debugging specialist",
            )
            .with_context(131072),
            // Reasoning
            ModelInfo::new(
                "qwen/qwen3-next-80b-a3b-thinking",
                "Qwen3-Next 80B Thinking - Hybrid reasoning",
            )
            .with_context(256000),
            ModelInfo::new(
                "qwen/qwen3-235b-a22b-fp8",
                "Qwen3 235B - Deep reasoning MoE",
            )
            .with_context(131072),
            ModelInfo::new(
                "qwen/qwq-32b",
                "QwQ 32B - Advanced reasoning & math",
            )
            .with_context(131072),
            ModelInfo::new("deepseek-ai/deepseek-r1", "DeepSeek R1 - Chain of thought")
                .with_context(65536),
            // General
            ModelInfo::new(
                "meta/llama-3.3-70b-instruct",
                "Llama 3.3 70B - General purpose",
            )
            .with_context(131072),
            // Fast
            ModelInfo::new(
                "microsoft/phi-4-mini-instruct",
                "Phi-4 Mini - Very fast",
            )
            .with_context(131072),
        ]
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports_vision(&self) -> bool {
        // Some models support vision
        true
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

        let response = self.request_with_retry(body, true).await?;
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

        let response = self.request_with_retry(body, false).await?;

        let api_response: OpenAIResponse = response
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

// Tipos OpenAI-compatible (NVIDIA usa mesmo formato)

#[derive(Debug, Serialize)]
struct OpenAIMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize)]
struct OpenAITool {
    r#type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Serialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIToolCall {
    id: String,
    function: OpenAIToolFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
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
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn parse_stream_chunk(data: &str) -> Option<StreamEvent> {
    let chunk: StreamChunk = serde_json::from_str(data).ok()?;
    let choice = chunk.choices.first()?;

    if let Some(ref content) = choice.delta.content {
        if !content.is_empty() {
            return Some(StreamEvent::Text(content.clone()));
        }
    }

    if let Some(ref tool_calls) = choice.delta.tool_calls {
        for tc in tool_calls {
            if let Some(ref id) = tc.id {
                if let Some(ref func) = tc.function {
                    if let Some(ref name) = func.name {
                        return Some(StreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                    }
                }
            }

            if let Some(ref func) = tc.function {
                if let Some(ref args) = func.arguments {
                    if !args.is_empty() {
                        return Some(StreamEvent::ToolCallDelta {
                            id: tc.index.to_string(),
                            arguments: args.clone(),
                        });
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nvidia_models() {
        let provider = NvidiaProvider::new(vec!["test-key".to_string()]);
        let models = provider.models();

        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.id.contains("llama")));
        assert!(models.iter().any(|m| m.id.contains("qwen")));
    }

    #[tokio::test]
    async fn test_key_pool() {
        let pool = NvidiaKeyPool::new(vec!["key1".to_string(), "key2".to_string()]);

        let k1 = pool.get_key().await;
        let k2 = pool.get_key().await;

        assert!(k1.is_some());
        assert!(k2.is_some());
        // Round robin deve alternar
        assert_ne!(k1, k2);
    }

    #[tokio::test]
    async fn test_rate_limit_handling() {
        let pool = NvidiaKeyPool::new(vec!["key1".to_string(), "key2".to_string()]);

        pool.mark_rate_limited("key1").await;

        // Deve pular key1 e retornar key2
        let key = pool.get_key().await.unwrap();
        assert_eq!(key, "key2");
    }
}
