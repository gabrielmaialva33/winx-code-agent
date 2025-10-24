//! DashScope API models compatible with OpenAI format

use serde::{Deserialize, Serialize};

const DETAIL_BASIC: &str = "Provide a brief, high-level explanation of what this code does.";
const DETAIL_EXPERT: &str = "Provide a comprehensive, expert-level analysis including architecture, patterns, potential issues, and optimization opportunities.";
const DETAIL_DEFAULT: &str = "Provide a detailed explanation of this code including its purpose, how it works, and key concepts.";

/// Chat message role
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Chat message
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

impl ChatMessage {
    /// Create a system message
    pub fn system(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
        }
    }

    /// Create a user message
    pub fn user(content: String) -> Self {
        Self {
            role: MessageRole::User,
            content,
        }
    }

    /// Create an assistant message
    pub fn assistant(content: String) -> Self {
        Self {
            role: MessageRole::Assistant,
            content,
        }
    }
}

/// Chat completion request
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Choice in chat completion response
#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Usage statistics
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Chat completion response
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Streaming delta message
#[derive(Debug, Deserialize)]
pub struct DeltaMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<MessageRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Streaming choice
#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: DeltaMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Streaming response chunk
#[derive(Debug, Deserialize)]
pub struct StreamResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
}

/// Code generation request for DashScope
#[derive(Debug, Serialize)]
pub struct DashScopeCodeGenerationRequest {
    pub prompt: String,
    pub language: Option<String>,
    pub context: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// Code analysis request for DashScope  
#[derive(Debug, Serialize)]
pub struct DashScopeCodeAnalysisRequest {
    pub code: String,
    pub language: Option<String>,
    pub include_suggestions: Option<bool>,
    pub include_complexity: Option<bool>,
}

impl<'a> ChatCompletionRequest<'a> {
    /// Create a new chat completion request
    pub fn new(model: &'a str, messages: Vec<ChatMessage>) -> Self {
        Self {
            model,
            messages,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: None,
            stop: None,
        }
    }

    /// Create a request for code analysis
    pub fn new_code_analysis(model: &'a str, code: &str, language: Option<&str>) -> Self {
        let analysis_prompt = if let Some(lang) = language {
            format!(
                "Return a JSON response with the following structure:
{{
  \"summary\": \"Brief description of what the code does and main issues\",
  \"issues\": [
    {{
      \"severity\": \"Error|Warning|Info|Critical\",
      \"category\": \"Bug|Security|Performance|Style\",
      \"message\": \"Description of the issue\",
      \"line\": 10,
      \"suggestion\": \"How to fix this issue\"
    }}
  ],
  \"suggestions\": [\"General improvement suggestions\"],
  \"complexity_score\": 75
}}

Code to analyze:
```{} {}
```
{}",
                lang, lang, code
            )
        } else {
            format!(
                "Return a JSON response with the following structure:
{{
  \"summary\": \"Brief description of what the code does and main issues\",
  \"issues\": [
    {{
      \"severity\": \"Error|Warning|Info|Critical\",
      \"category\": \"Bug|Security|Performance|Style\",
      \"message\": \"Description of the issue\",
      \"line\": 10,
      \"suggestion\": \"How to fix this issue\"
    }}
  ],
  \"suggestions\": [\"General improvement suggestions\"],
  \"complexity_score\": 75
}}

Code to analyze:
```
{}
```",
                code
            )
        };

        let messages = vec![ChatMessage::user(analysis_prompt)];
        let mut request = Self::new(model, messages);
        request.temperature = Some(0.1);
        request.top_p = Some(0.8);
        request.max_tokens = Some(2048);
        request
    }

    /// Create a request for code generation
    pub fn new_code_generation(
        model: &'a str,
        prompt: &str,
        language: Option<&str>,
        context: Option<&str>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        let generation_prompt = match (language, context) {
            (Some(lang), Some(ctx)) => {
                format!(
                    "Generate {} code based on this description: {}\n\nContext: {}\n\nProvide clean, well-commented code with best practices.",
                    lang, prompt, ctx
                )
            }
            (Some(lang), None) => {
                format!(
                    "Generate {} code based on this description: {}\n\nProvide clean, well-commented code with best practices.",
                    lang, prompt
                )
            }
            (None, Some(ctx)) => {
                format!(
                    "Generate code based on this description: {}\n\nContext: {}\n\nProvide clean, well-commented code with best practices.",
                    prompt, ctx
                )
            }
            (None, None) => {
                format!(
                    "Generate code based on this description: {}\n\nProvide clean, well-commented code with best practices.",
                    prompt
                )
            }
        };

        let messages = vec![ChatMessage::user(generation_prompt)];
        let mut request = Self::new(model, messages);
        request.temperature = temperature.or(Some(0.7));
        request.top_p = Some(0.9);
        request.max_tokens = max_tokens.or(Some(1000));
        request
    }

    /// Create a request for code explanation
    pub fn new_code_explanation(
        model: &'a str,
        code: &str,
        language: Option<&str>,
        detail_level: &str,
    ) -> Self {
        let detail_instruction = match detail_level {
            "basic" => DETAIL_BASIC,
            "expert" => DETAIL_EXPERT,
            _ => DETAIL_DEFAULT,
        };

        let explanation_prompt = if let Some(lang) = language {
            format!(
                "{}\n\n{} code to explain:\n```{}\n{}\n```",
                detail_instruction, lang, lang, code
            )
        } else {
            format!(
                "{}\n\nCode to explain:\n```\n{}\n```",
                detail_instruction, code
            )
        };

        let messages = vec![ChatMessage::user(explanation_prompt)];
        let mut request = Self::new(model, messages);
        request.temperature = Some(0.3);
        request.top_p = Some(0.8);
        request.max_tokens = Some(1500);
        request
    }
}

impl ChatCompletionResponse {
    /// Get the content from the first choice
    pub fn get_content(&self) -> Option<&str> {
        self.choices
            .first()
            .map(|choice| choice.message.content.as_str())
    }

    /// Check if the response was successful
    pub fn is_success(&self) -> bool {
        !self.choices.is_empty()
    }
}
