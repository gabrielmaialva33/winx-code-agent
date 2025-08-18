//! Data models for NVIDIA API integration

use serde::{Deserialize, Serialize};

/// Supported NVIDIA models for different tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NvidiaModel {
    /// Qwen3 235B A22B - Latest generation LLM with thinking mode and MoE architecture
    #[serde(rename = "qwen/qwen3-235b-a22b")]
    Qwen3_235B,
    /// Meta Llama 3.1 70B Instruct - Good for general coding tasks
    #[serde(rename = "meta/llama-3.1-70b-instruct")]
    Llama31_70B,
    /// NVIDIA Nemotron 4 340B Instruct - Best for complex reasoning
    #[serde(rename = "nvidia/nemotron-4-340b-instruct")]
    Nemotron340B,
    /// Microsoft Phi-3 Medium - Fast for smaller tasks
    #[serde(rename = "microsoft/phi-3-medium-128k-instruct")]
    Phi3Medium,
    /// Google CodeGemma - Specialized for code
    #[serde(rename = "google/codegemma-7b")]
    CodeGemma7B,
    /// Mistral Codestral - Code completion specialist
    #[serde(rename = "mistralai/codestral-22b-instruct-v0.1")]
    Codestral22B,
}

impl Default for NvidiaModel {
    fn default() -> Self {
        Self::Qwen3_235B
    }
}

impl NvidiaModel {
    /// Get the model string for API calls
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Qwen3_235B => "qwen/qwen3-235b-a22b",
            Self::Llama31_70B => "meta/llama-3.1-70b-instruct",
            Self::Nemotron340B => "nvidia/nemotron-4-340b-instruct",
            Self::Phi3Medium => "microsoft/phi-3-medium-128k-instruct",
            Self::CodeGemma7B => "google/codegemma-7b",
            Self::Codestral22B => "mistralai/codestral-22b-instruct-v0.1",
        }
    }

    /// Get recommended model for specific task types
    pub fn for_task(task: TaskType) -> Self {
        match task {
            TaskType::CodeGeneration | TaskType::CodeCompletion => Self::Qwen3_235B,
            TaskType::CodeAnalysis | TaskType::BugDetection => Self::Qwen3_235B,
            TaskType::CodeExplanation | TaskType::Documentation => Self::Qwen3_235B,
            TaskType::ComplexReasoning | TaskType::Refactoring => Self::Qwen3_235B,
            TaskType::FastResponse => Self::Phi3Medium,
        }
    }
}

/// Types of AI tasks for model selection
#[derive(Debug, Clone)]
pub enum TaskType {
    CodeGeneration,
    CodeCompletion,
    CodeAnalysis,
    BugDetection,
    CodeExplanation,
    Documentation,
    ComplexReasoning,
    Refactoring,
    FastResponse,
}

/// Chat completion request to NVIDIA API
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

/// Chat message for conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Reasoning content for models that support thinking mode (like Qwen3)
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            reasoning_content: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            reasoning_content: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            reasoning_content: None,
        }
    }

    /// Get the effective content, combining content and reasoning_content if available
    pub fn effective_content(&self) -> String {
        match (&self.content, &self.reasoning_content) {
            (content, Some(reasoning)) if content.is_empty() => reasoning.clone(),
            (content, Some(reasoning)) => format!("{}\n\nReasoning: {}", content, reasoning),
            (content, None) => content.clone(),
        }
    }
}

/// Response from NVIDIA chat completion API
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    #[serde(default)]
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub model: String,
}

/// Individual choice in completion response
#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage information
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Code analysis result
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeAnalysisResult {
    pub summary: String,
    pub issues: Vec<CodeIssue>,
    pub suggestions: Vec<String>,
    pub complexity_score: Option<u32>,
}

/// Individual code issue found during analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeIssue {
    pub severity: IssueSeverity,
    pub category: IssueCategory,
    pub message: String,
    pub line: Option<u32>,
    pub suggestion: Option<String>,
}

/// Severity levels for code issues
#[derive(Debug, Serialize, Deserialize)]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueSeverity::Info => write!(f, "Info"),
            IssueSeverity::Warning => write!(f, "Warning"),
            IssueSeverity::Error => write!(f, "Error"),
            IssueSeverity::Critical => write!(f, "Critical"),
        }
    }
}

/// Categories of code issues
#[derive(Debug, Serialize, Deserialize)]
pub enum IssueCategory {
    Bug,
    Performance,
    Security,
    Style,
    Maintainability,
    Documentation,
}

impl std::fmt::Display for IssueCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueCategory::Bug => write!(f, "Bug"),
            IssueCategory::Performance => write!(f, "Performance"),
            IssueCategory::Security => write!(f, "Security"),
            IssueCategory::Style => write!(f, "Style"),
            IssueCategory::Maintainability => write!(f, "Maintainability"),
            IssueCategory::Documentation => write!(f, "Documentation"),
        }
    }
}

/// Code generation request
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeGenerationRequest {
    pub prompt: String,
    pub language: Option<String>,
    pub context: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// Code generation result
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeGenerationResult {
    pub code: String,
    pub language: Option<String>,
    pub explanation: Option<String>,
    pub tests: Option<String>,
}

/// Error response from NVIDIA API
#[derive(Debug, Deserialize)]
pub struct ApiError {
    pub error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    pub r#type: String,
    pub code: Option<String>,
}
