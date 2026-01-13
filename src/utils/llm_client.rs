//! LLM Client for NVIDIA NIM integration.
//!
//! This module provides a client for interacting with NVIDIA NIM API
//! using Qwen3-Next-80B model for semantic code matching.

use std::env;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

/// Default NVIDIA NIM endpoint
const DEFAULT_NVIDIA_ENDPOINT: &str = "https://integrate.api.nvidia.com/v1/chat/completions";

/// Default model for semantic matching
const DEFAULT_MODEL: &str = "qwen/qwen3-next-80b-a3b-instruct";

/// Default timeout for API calls (30 seconds)
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum tokens for responses
const DEFAULT_MAX_TOKENS: u32 = 2048;

/// LLM Client configuration
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// API endpoint URL
    pub endpoint: String,
    /// Model name
    pub model: String,
    /// API key (from environment or explicit)
    pub api_key: Option<String>,
    /// Request timeout
    pub timeout: Duration,
    /// Temperature for responses (0.0 = deterministic)
    pub temperature: f32,
    /// Maximum tokens to generate
    pub max_tokens: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_NVIDIA_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: env::var("NVIDIA_API_KEY").ok(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            temperature: 0.1, // Low temperature for deterministic code matching
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// Message role in chat completion
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Chat completion request
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// Chat completion response
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    #[allow(dead_code)]
    usage: Option<Usage>,
}

/// Choice in response
#[derive(Debug, Deserialize)]
struct Choice {
    message: MessageResponse,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

/// Message in response
#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: String,
}

/// Token usage information
#[derive(Debug, Deserialize)]
struct Usage {
    #[allow(dead_code)]
    prompt_tokens: u32,
    #[allow(dead_code)]
    completion_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
}

/// LLM Client for NVIDIA NIM
#[derive(Debug, Clone)]
pub struct LlmClient {
    config: LlmConfig,
    client: Client,
}

/// Result type for semantic matching
#[derive(Debug, Clone)]
pub struct SemanticMatchResult {
    /// The matched/corrected search block
    pub corrected_search: String,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f32,
    /// Whether the match was successful
    pub success: bool,
    /// Explanation of the match
    pub explanation: String,
}

impl LlmClient {
    /// Create a new LLM client with default configuration
    pub fn new() -> Option<Self> {
        let config = LlmConfig::default();

        // Check if API key is available
        if config.api_key.is_none() {
            warn!("NVIDIA_API_KEY not found in environment, LLM features disabled");
            return None;
        }

        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .ok()?;

        Some(Self { config, client })
    }

    /// Create a new LLM client with custom configuration
    pub fn with_config(config: LlmConfig) -> Option<Self> {
        if config.api_key.is_none() {
            warn!("API key not provided, LLM features disabled");
            return None;
        }

        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .ok()?;

        Some(Self { config, client })
    }

    /// Send a chat completion request
    async fn chat_completion(&self, messages: Vec<Message>) -> Result<String, LlmError> {
        let api_key = self.config.api_key.as_ref().ok_or(LlmError::NoApiKey)?;

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            stream: Some(false),
        };

        debug!("Sending request to NVIDIA NIM: {:?}", self.config.endpoint);

        let response = self
            .client
            .post(&self.config.endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| LlmError::RequestFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            error!("NVIDIA NIM API error: {} - {}", status, error_text);
            return Err(LlmError::ApiError {
                status: status.as_u16(),
                message: error_text,
            });
        }

        let completion: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| LlmError::ParseError(e.to_string()))?;

        completion
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or(LlmError::EmptyResponse)
    }

    /// Perform semantic code block matching
    ///
    /// When fuzzy matching fails, this uses the LLM to understand
    /// the intent and find the correct code block to match.
    pub async fn semantic_match(
        &self,
        search_block: &str,
        file_content: &str,
        file_path: &str,
    ) -> Result<SemanticMatchResult, LlmError> {
        let system_prompt = r#"You are a precise code matching assistant. Your task is to find the exact location of a search block in a file, even when there are minor differences (whitespace, indentation, formatting).

RULES:
1. Return ONLY the exact matching block from the file content, preserving its exact formatting
2. If no match exists, return "NO_MATCH"
3. Include a confidence score (0.0-1.0) based on semantic similarity
4. Be precise - small differences matter in code

OUTPUT FORMAT (JSON):
{
  "matched_block": "exact content from file or NO_MATCH",
  "confidence": 0.95,
  "explanation": "brief explanation"
}"#;

        let user_prompt = format!(
            r#"Find this search block in the file:

FILE: {file_path}
---FILE CONTENT START---
{file_content}
---FILE CONTENT END---

---SEARCH BLOCK START---
{search_block}
---SEARCH BLOCK END---

Find the best matching section in the file content. Return JSON with the exact matched block."#
        );

        let messages = vec![
            Message {
                role: Role::System,
                content: system_prompt.to_string(),
            },
            Message {
                role: Role::User,
                content: user_prompt,
            },
        ];

        let response = self.chat_completion(messages).await?;

        // Parse the JSON response
        Self::parse_semantic_response(&response, search_block)
    }

    /// Parse the LLM response for semantic matching
    fn parse_semantic_response(
        response: &str,
        original_search: &str,
    ) -> Result<SemanticMatchResult, LlmError> {
        // Try to extract JSON from the response
        let json_str = if response.contains("```json") {
            response
                .split("```json")
                .nth(1)
                .and_then(|s| s.split("```").next())
                .unwrap_or(response)
        } else if response.contains("```") {
            response
                .split("```")
                .nth(1)
                .unwrap_or(response)
        } else {
            response
        };

        #[derive(Deserialize)]
        struct MatchResponse {
            matched_block: String,
            confidence: f32,
            explanation: String,
        }

        match serde_json::from_str::<MatchResponse>(json_str.trim()) {
            Ok(parsed) => {
                let success = parsed.matched_block != "NO_MATCH" && parsed.confidence > 0.7;
                Ok(SemanticMatchResult {
                    corrected_search: if success {
                        parsed.matched_block
                    } else {
                        original_search.to_string()
                    },
                    confidence: parsed.confidence,
                    success,
                    explanation: parsed.explanation,
                })
            }
            Err(e) => {
                debug!("Failed to parse LLM response as JSON: {}", e);
                // Try to extract the matched block directly from the response
                if response.contains("NO_MATCH") {
                    Ok(SemanticMatchResult {
                        corrected_search: original_search.to_string(),
                        confidence: 0.0,
                        success: false,
                        explanation: "No match found".to_string(),
                    })
                } else {
                    Err(LlmError::ParseError(format!(
                        "Failed to parse semantic match response: {e}"
                    )))
                }
            }
        }
    }

    /// Analyze code intent for better error recovery
    pub async fn analyze_code_intent(
        &self,
        search_block: &str,
        replace_block: &str,
        file_path: &str,
    ) -> Result<CodeIntentAnalysis, LlmError> {
        let system_prompt = r#"You are a code analysis assistant. Analyze the intent of a search/replace operation.

OUTPUT FORMAT (JSON):
{
  "intent": "brief description of what the change does",
  "risk_level": "low|medium|high",
  "suggestions": ["list of suggestions if any issues detected"],
  "is_safe": true/false
}"#;

        let user_prompt = format!(
            r#"Analyze this code change intent:

FILE: {file_path}

SEARCH (to be replaced):
```
{search_block}
```

REPLACE (new content):
```
{replace_block}
```

Analyze the intent and safety of this change."#
        );

        let messages = vec![
            Message {
                role: Role::System,
                content: system_prompt.to_string(),
            },
            Message {
                role: Role::User,
                content: user_prompt,
            },
        ];

        let response = self.chat_completion(messages).await?;
        Self::parse_intent_response(&response)
    }

    /// Parse intent analysis response
    fn parse_intent_response(response: &str) -> Result<CodeIntentAnalysis, LlmError> {
        let json_str = if response.contains("```json") {
            response
                .split("```json")
                .nth(1)
                .and_then(|s| s.split("```").next())
                .unwrap_or(response)
        } else if response.contains("```") {
            response
                .split("```")
                .nth(1)
                .unwrap_or(response)
        } else {
            response
        };

        #[derive(Deserialize)]
        struct IntentResponse {
            intent: String,
            risk_level: String,
            suggestions: Vec<String>,
            is_safe: bool,
        }

        match serde_json::from_str::<IntentResponse>(json_str.trim()) {
            Ok(parsed) => Ok(CodeIntentAnalysis {
                intent: parsed.intent,
                risk_level: match parsed.risk_level.to_lowercase().as_str() {
                    "low" => RiskLevel::Low,
                    "medium" => RiskLevel::Medium,
                    "high" => RiskLevel::High,
                    _ => RiskLevel::Medium,
                },
                suggestions: parsed.suggestions,
                is_safe: parsed.is_safe,
            }),
            Err(e) => Err(LlmError::ParseError(format!(
                "Failed to parse intent response: {e}"
            ))),
        }
    }

    /// Check if LLM features are available
    pub fn is_available(&self) -> bool {
        self.config.api_key.is_some()
    }

    /// Get the current model name
    pub fn model(&self) -> &str {
        &self.config.model
    }
}

/// Code intent analysis result
#[derive(Debug, Clone)]
pub struct CodeIntentAnalysis {
    /// Description of the change intent
    pub intent: String,
    /// Risk level of the change
    pub risk_level: RiskLevel,
    /// Suggestions for improvement
    pub suggestions: Vec<String>,
    /// Whether the change is considered safe
    pub is_safe: bool,
}

/// Risk level for code changes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// LLM-specific errors
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("NVIDIA API key not configured")]
    NoApiKey,

    #[error("Request failed: {0}")]
    RequestFailed(String),

    #[error("API error (status {status}): {message}")]
    ApiError { status: u16, message: String },

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("Empty response from API")]
    EmptyResponse,

    #[error("Timeout waiting for response")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LlmConfig::default();
        assert_eq!(config.endpoint, DEFAULT_NVIDIA_ENDPOINT);
        assert_eq!(config.model, DEFAULT_MODEL);
        assert!(config.temperature < 0.5); // Should be low for code matching
    }

    #[test]
    fn test_parse_semantic_response_success() {
        let response = r#"```json
{
  "matched_block": "fn test() {\n    println!(\"hello\");\n}",
  "confidence": 0.95,
  "explanation": "Exact match found"
}
```"#;

        let result = LlmClient::parse_semantic_response(response, "original").unwrap();
        assert!(result.success);
        assert!(result.confidence > 0.9);
    }

    #[test]
    fn test_parse_semantic_response_no_match() {
        let response = r#"{
  "matched_block": "NO_MATCH",
  "confidence": 0.0,
  "explanation": "No similar block found"
}"#;

        let result = LlmClient::parse_semantic_response(response, "original").unwrap();
        assert!(!result.success);
        assert_eq!(result.corrected_search, "original");
    }

    #[test]
    fn test_parse_intent_response() {
        let response = r#"```json
{
  "intent": "Add error handling",
  "risk_level": "low",
  "suggestions": [],
  "is_safe": true
}
```"#;

        let result = LlmClient::parse_intent_response(response).unwrap();
        assert_eq!(result.risk_level, RiskLevel::Low);
        assert!(result.is_safe);
    }
}
