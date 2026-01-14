//! `OpenAI` Provider
//!
//! Suporta `OpenAI` API e compatíveis (Azure, Together, etc.)

use std::env;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    ChatOptions, ChatResponse, EventStream, FunctionDef, Message, MessageContent, ModelInfo,
    OpenAIConfig, Provider, ProviderError, Role, StreamEvent, ToolCall, Usage,
};

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    is_nvidia_compat: bool,
}

impl OpenAIProvider {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            base_url: OPENAI_API_URL.to_string(),
            default_model: "gpt-4o".to_string(),
            is_nvidia_compat: false,
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.to_string();
        self.is_nvidia_compat = url.contains("nvidia.com");
        self
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::ConfigError("OPENAI_API_KEY not set".to_string()))?;

        let mut provider = Self::new(&api_key);

        if let Ok(base_url) = env::var("OPENAI_BASE_URL") {
            // Detect NVIDIA endpoint and adjust
            if base_url.contains("nvidia.com") {
                provider.is_nvidia_compat = true;
                provider.default_model = "qwen/qwen3-next-80b-a3b-instruct".to_string();
            }
            // Ensure URL has chat/completions endpoint
            if base_url.ends_with("/v1") {
                provider.base_url = format!("{base_url}/chat/completions");
            } else if !base_url.contains("/chat/completions") {
                provider.base_url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            } else {
                provider.base_url = base_url;
            }
        }

        Ok(provider)
    }

    pub fn from_config(config: &OpenAIConfig) -> Option<Self> {
        let api_key = config
            .api_key
            .clone()
            .or_else(|| env::var("OPENAI_API_KEY").ok())?;

        let mut provider = Self::new(&api_key);

        if let Some(ref url) = config.base_url {
            provider.is_nvidia_compat = url.contains("nvidia.com");
            // Ensure URL has chat/completions endpoint
            if url.ends_with("/v1") {
                provider.base_url = format!("{url}/chat/completions");
            } else if !url.contains("/chat/completions") {
                provider.base_url = format!("{}/chat/completions", url.trim_end_matches('/'));
            } else {
                provider.base_url = url.clone();
            }
        }

        if let Some(ref model) = config.default_model {
            provider.default_model = model.clone();
        } else if provider.is_nvidia_compat {
            provider.default_model = "qwen/qwen3-next-80b-a3b-instruct".to_string();
        }

        Some(provider)
    }

    fn convert_messages(&self, messages: &[Message]) -> Vec<OpenAIMessage> {
        messages.iter().map(|m| self.convert_message(m)).collect()
    }

    fn convert_message(&self, msg: &Message) -> OpenAIMessage {
        let role = msg.role.as_str().to_string();

        let content = match &msg.content {
            MessageContent::Text(text) => OpenAIContent::Text(text.clone()),
            MessageContent::Parts(parts) => {
                let converted: Vec<OpenAIContentPart> = parts
                    .iter()
                    .filter_map(|p| match p {
                        super::ContentPart::Text { text } => {
                            Some(OpenAIContentPart::Text { text: text.clone() })
                        }
                        super::ContentPart::Image { image_url } => {
                            Some(OpenAIContentPart::ImageUrl {
                                image_url: OpenAIImageUrl {
                                    url: image_url.url.clone(),
                                },
                            })
                        }
                        _ => None,
                    })
                    .collect();
                OpenAIContent::Parts(converted)
            }
            MessageContent::ToolResult {
                tool_use_id,
                content,
            } => {
                return OpenAIMessage {
                    role: "tool".to_string(),
                    content: OpenAIContent::Text(content.clone()),
                    tool_call_id: Some(tool_use_id.clone()),
                    tool_calls: None,
                };
            }
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
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        if self.is_nvidia_compat {
            "openai-nvidia"
        } else {
            "openai"
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        if self.is_nvidia_compat {
            // NVIDIA API models via OpenAI-compatible endpoint (2025 latest)
            vec![
                ModelInfo::new("qwen/qwen3-next-80b-a3b-instruct", "Qwen3-Next 80B - Agentic coding")
                    .with_context(256000),
                ModelInfo::new("qwen/qwen3-coder-480b-a35b-instruct", "Qwen3 Coder 480B - Best for code")
                    .with_context(256000),
                ModelInfo::new("qwen/qwen3-235b-a22b-fp8", "Qwen3 235B - Deep reasoning")
                    .with_context(128000),
                ModelInfo::new("qwen/qwq-32b", "QwQ 32B - Advanced reasoning")
                    .with_context(128000),
                ModelInfo::new("qwen/qwen3-next-80b-a3b-instruct", "Llama 3.3 70B - General")
                    .with_context(128000),
            ]
        } else {
            vec![
                ModelInfo::new("gpt-4o", "GPT-4o - Latest multimodal")
                    .with_context(128000)
                    .with_vision(),
                ModelInfo::new("gpt-4o-mini", "GPT-4o Mini - Fast and cheap")
                    .with_context(128000)
                    .with_vision(),
                ModelInfo::new("gpt-4-turbo", "GPT-4 Turbo").with_context(128000),
                ModelInfo::new("o1", "O1 - Deep reasoning").with_context(128000),
                ModelInfo::new("o1-mini", "O1 Mini - Faster reasoning").with_context(128000),
            ]
        }
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports_vision(&self) -> bool {
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

        // Build messages array with optional system prompt first
        let mut all_messages = Vec::new();
        if let Some(ref system) = options.system {
            all_messages.push(OpenAIMessage {
                role: "system".to_string(),
                content: OpenAIContent::Text(system.clone()),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        all_messages.extend(self.convert_messages(messages));

        let mut body = json!({
            "model": model,
            "messages": all_messages,
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
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            return Err(match status.as_u16() {
                401 => ProviderError::InvalidApiKey,
                429 => ProviderError::RateLimited,
                _ => ProviderError::ApiError(format!("{status}: {text}")),
            });
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

                                if let Some(event) = parse_openai_stream(data) {
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

        // Build messages array with optional system prompt first
        let mut all_messages = Vec::new();
        if let Some(ref system) = options.system {
            all_messages.push(OpenAIMessage {
                role: "system".to_string(),
                content: OpenAIContent::Text(system.clone()),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        all_messages.extend(self.convert_messages(messages));

        let mut body = json!({
            "model": model,
            "messages": all_messages,
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
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            return Err(match status.as_u16() {
                401 => ProviderError::InvalidApiKey,
                429 => ProviderError::RateLimited,
                _ => ProviderError::ApiError(format!("{status}: {text}")),
            });
        }

        let api_response: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let choice = api_response
            .choices
            .first()
            .ok_or_else(|| ProviderError::ParseError("No choices in response".to_string()))?;

        let content = match &choice.message.content {
            Some(OpenAIContent::Text(t)) => t.clone(),
            _ => String::new(),
        };

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

// Tipos internos para OpenAI API

#[derive(Debug, Serialize)]
struct OpenAIMessage {
    role: String,
    content: OpenAIContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum OpenAIContent {
    Text(String),
    Parts(Vec<OpenAIContentPart>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum OpenAIContentPart {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAIImageUrl },
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIImageUrl {
    url: String,
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
    content: Option<OpenAIContent>,
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
struct OpenAIStreamChunk {
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIStreamDelta,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<OpenAIStreamFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn parse_openai_stream(data: &str) -> Option<StreamEvent> {
    let chunk: OpenAIStreamChunk = serde_json::from_str(data).ok()?;
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
    fn test_openai_models() {
        let provider = OpenAIProvider::new("test-key");
        let models = provider.models();

        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.id.contains("gpt")));
    }

    #[test]
    fn test_tool_conversion() {
        let provider = OpenAIProvider::new("test-key");

        let tools = vec![FunctionDef {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: json!({"type": "object"}),
        }];

        let converted = provider.convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].function.name, "get_weather");
    }
}
