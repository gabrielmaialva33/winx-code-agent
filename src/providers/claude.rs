//! Claude (Anthropic) Provider
//!
//! Integração com a API da Anthropic para modelos Claude.

use std::env;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    ChatOptions, ChatResponse, ClaudeConfig, EventStream, FunctionDef, Message, MessageContent,
    ModelInfo, Provider, ProviderError, Role, StreamEvent, ToolCall, Usage,
};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ClaudeProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl ClaudeProvider {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            default_model: "claude-sonnet-4-20250514".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let api_key = env::var("ANTHROPIC_API_KEY")
            .map_err(|_| ProviderError::ConfigError("ANTHROPIC_API_KEY not set".to_string()))?;
        Ok(Self::new(&api_key))
    }

    pub fn from_config(config: &ClaudeConfig) -> Option<Self> {
        let api_key = config
            .api_key
            .clone()
            .or_else(|| env::var("ANTHROPIC_API_KEY").ok())?;

        let mut provider = Self::new(&api_key);
        if let Some(ref model) = config.default_model {
            provider.default_model = model.clone();
        }
        Some(provider)
    }

    fn convert_messages(&self, messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
        let mut system = None;
        let mut result = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if let MessageContent::Text(text) = &msg.content {
                        system = Some(text.clone());
                    }
                }
                Role::User | Role::Assistant => {
                    result.push(self.convert_message(msg));
                }
            }
        }

        (system, result)
    }

    fn convert_message(&self, msg: &Message) -> AnthropicMessage {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "user", // Não deve acontecer
        };

        let content = match &msg.content {
            MessageContent::Text(text) => vec![AnthropicContent::Text { text: text.clone() }],
            MessageContent::Parts(parts) => {
                parts
                    .iter()
                    .map(|p| match p {
                        super::ContentPart::Text { text } => {
                            AnthropicContent::Text { text: text.clone() }
                        }
                        super::ContentPart::Image { image_url } => AnthropicContent::Image {
                            source: ImageSource {
                                r#type: "base64".to_string(),
                                media_type: "image/png".to_string(),
                                data: image_url.url.clone(),
                            },
                        },
                        super::ContentPart::ToolUse { id, name, input } => {
                            AnthropicContent::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }
                        }
                    })
                    .collect()
            }
            MessageContent::ToolResult {
                tool_use_id,
                content,
            } => {
                vec![AnthropicContent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                }]
            }
        };

        AnthropicMessage {
            role: role.to_string(),
            content,
        }
    }

    fn convert_tools(&self, tools: &[FunctionDef]) -> Vec<AnthropicTool> {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect()
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo::new("claude-sonnet-4-20250514", "Claude Sonnet 4 - Balanced")
                .with_context(200000)
                .with_vision(),
            ModelInfo::new("claude-opus-4-20250514", "Claude Opus 4 - Most capable")
                .with_context(200000)
                .with_vision(),
            ModelInfo::new("claude-3-5-haiku-20241022", "Claude 3.5 Haiku - Fast")
                .with_context(200000),
        ]
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

        let (system, converted_messages) = self.convert_messages(messages);

        let mut body = json!({
            "model": model,
            "messages": converted_messages,
            "max_tokens": options.max_tokens.unwrap_or(4096),
            "stream": true,
        });

        if let Some(ref sys) = system.or(options.system.clone()) {
            body["system"] = json!(sys);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(ref tools) = options.tools {
            body["tools"] = json!(self.convert_tools(tools));
        }

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
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

                        // Process complete SSE events
                        while let Some(pos) = buffer.find("\n\n") {
                            let event_str = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            if let Some(event) = parse_sse_event(&event_str) {
                                yield event;
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

        let (system, converted_messages) = self.convert_messages(messages);

        let mut body = json!({
            "model": model,
            "messages": converted_messages,
            "max_tokens": options.max_tokens.unwrap_or(4096),
        });

        if let Some(ref sys) = system.or(options.system.clone()) {
            body["system"] = json!(sys);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(ref tools) = options.tools {
            body["tools"] = json!(self.convert_tools(tools));
        }

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
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

        let api_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in api_response.content {
            match block {
                AnthropicContent::Text { text } => {
                    content.push_str(&text);
                }
                AnthropicContent::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                _ => {}
            }
        }

        Ok(ChatResponse {
            content,
            tool_calls,
            usage: Some(Usage {
                prompt_tokens: api_response.usage.input_tokens,
                completion_tokens: api_response.usage.output_tokens,
                total_tokens: api_response.usage.input_tokens + api_response.usage.output_tokens,
            }),
            finish_reason: Some(api_response.stop_reason.unwrap_or_default()),
        })
    }
}

// Tipos internos para API da Anthropic

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image")]
    Image { source: ImageSource },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct ImageSource {
    r#type: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    r#type: String,
    #[serde(default)]
    delta: Option<AnthropicDelta>,
    #[serde(default)]
    content_block: Option<AnthropicContent>,
    #[serde(default)]
    index: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

fn parse_sse_event(event_str: &str) -> Option<StreamEvent> {
    let mut data = None;

    for line in event_str.lines() {
        if let Some(value) = line.strip_prefix("data: ") {
            data = Some(value);
        }
    }

    let data = data?;

    let event: AnthropicStreamEvent = serde_json::from_str(data).ok()?;

    match event.r#type.as_str() {
        "content_block_delta" => {
            if let Some(delta) = event.delta {
                if let Some(text) = delta.text {
                    return Some(StreamEvent::Text(text));
                }
                if let Some(json) = delta.partial_json {
                    if let Some(idx) = event.index {
                        return Some(StreamEvent::ToolCallDelta {
                            id: idx.to_string(),
                            arguments: json,
                        });
                    }
                }
            }
        }
        "content_block_start" => {
            if let Some(block) = event.content_block {
                if let AnthropicContent::ToolUse { id, name, .. } = block {
                    return Some(StreamEvent::ToolCallStart { id, name });
                }
            }
        }
        "content_block_stop" => {
            if let Some(idx) = event.index {
                return Some(StreamEvent::ToolCallEnd {
                    id: idx.to_string(),
                });
            }
        }
        "message_stop" => {
            return Some(StreamEvent::Done);
        }
        "error" => {
            return Some(StreamEvent::Error(data.to_string()));
        }
        _ => {}
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_models() {
        let provider = ClaudeProvider::new("test-key");
        let models = provider.models();

        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.id.contains("sonnet")));
    }

    #[test]
    fn test_message_conversion() {
        let provider = ClaudeProvider::new("test-key");

        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi!"),
        ];

        let (system, converted) = provider.convert_messages(&messages);

        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "user");
        assert_eq!(converted[1].role, "assistant");
    }
}
