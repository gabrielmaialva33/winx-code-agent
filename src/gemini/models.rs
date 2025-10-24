//! Google Gemini API models and data structures

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

const DETAIL_BASIC: &str = "Provide a brief, high-level explanation of what this code does.";
const DETAIL_EXPERT: &str = "Provide a comprehensive, expert-level analysis including architecture, patterns, potential issues, and optimization opportunities.";
const DETAIL_DEFAULT: &str = "Provide a detailed explanation of this code including its purpose, how it works, and key concepts.";

lazy_static::lazy_static! {
    /// Cached JSON structure template for code analysis responses
    static ref ANALYSIS_JSON_STRUCTURE: &'static str = r#"Return a JSON response with the following structure:
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
```"#;

    /// Cached generic analysis template
    static ref GENERIC_ANALYSIS_TEMPLATE: &'static str = r#"Return a JSON response with the following structure:
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
```"#;

    /// Cached code generation template
    static ref CODE_GENERATION_TEMPLATE: &'static str = "Generate {} code based on this description: {}\n\nContext: {}\n\nProvide clean, well-commented code with best practices.";

    /// Cached simple code generation template
    static ref SIMPLE_CODE_GENERATION_TEMPLATE: &'static str = "Generate {} code based on this description: {}\n\nProvide clean, well-commented code with best practices.";

    /// Cached context code generation template
    static ref CONTEXT_CODE_GENERATION_TEMPLATE: &'static str = "Generate code based on this description: {}\n\nContext: {}\n\nProvide clean, well-commented code with best practices.";

    /// Cached basic code generation template
    static ref BASIC_CODE_GENERATION_TEMPLATE: &'static str = "Generate code based on this description: {}\n\nProvide clean, well-commented code with best practices.";

    /// Cached explanation template with language
    static ref EXPLANATION_TEMPLATE_WITH_LANG: &'static str = "{}\n\n{} code to explain:\n```{}\n{}\n```";

    /// Cached basic explanation template
    static ref BASIC_EXPLANATION_TEMPLATE: &'static str = "{}\n\nCode to explain:\n```\n{}\n```";
}

/// Gemini content part
#[derive(Debug, Serialize, Deserialize)]
pub struct ContentPart {
    pub text: String,
}

/// Gemini content
#[derive(Debug, Serialize, Deserialize)]
pub struct Content {
    pub parts: Vec<ContentPart>,
}

/// Gemini generation request
#[derive(Debug, Serialize)]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_settings: Option<Vec<SafetySetting>>,
}

/// Generation configuration
#[derive(Debug, Serialize)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

/// Safety setting
#[derive(Debug, Serialize)]
pub struct SafetySetting {
    pub category: String,
    pub threshold: String,
}

/// Gemini candidate
#[derive(Debug, Deserialize)]
pub struct Candidate {
    pub content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_ratings: Option<Vec<SafetyRating>>,
}

/// Safety rating
#[derive(Debug, Deserialize)]
pub struct SafetyRating {
    pub category: String,
    pub probability: String,
}

/// Gemini response
#[derive(Debug, Deserialize)]
pub struct GenerateContentResponse {
    pub candidates: Vec<Candidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_feedback: Option<PromptFeedback>,
}

/// Prompt feedback
#[derive(Debug, Deserialize)]
pub struct PromptFeedback {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_ratings: Option<Vec<SafetyRating>>,
}

/// Code generation request for Gemini
#[derive(Debug, Serialize)]
pub struct GeminiCodeGenerationRequest {
    pub prompt: String,
    pub language: Option<String>,
    pub context: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// Code analysis request for Gemini
#[derive(Debug, Serialize)]
pub struct GeminiCodeAnalysisRequest {
    pub code: String,
    pub language: Option<String>,
    pub include_suggestions: Option<bool>,
    pub include_complexity: Option<bool>,
}

impl GenerateContentRequest {
    /// Create a new request with a simple text prompt
    pub fn new_text(prompt: &str) -> Self {
        Self {
            contents: vec![Content {
                parts: vec![ContentPart {
                    text: prompt.to_string(),
                }],
            }],
            generation_config: None,
            safety_settings: None,
        }
    }

    /// Create a request for code analysis
    pub fn new_code_analysis(code: &str, language: Option<&str>) -> Self {
        let analysis_prompt = if let Some(lang) = language {
            format!(*ANALYSIS_JSON_STRUCTURE, lang, lang, code)
        } else {
            format!(*GENERIC_ANALYSIS_TEMPLATE, code)
        };

        let mut request = Self::new_text(&analysis_prompt);
        request.generation_config = Some(GenerationConfig {
            temperature: Some(0.1),
            top_p: Some(0.8),
            max_output_tokens: Some(2048),
            candidate_count: Some(1),
            ..Default::default()
        });
        request
    }

    /// Create a request for code generation
    pub fn new_code_generation(
        prompt: &str,
        language: Option<&str>,
        context: Option<&str>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        let generation_prompt = match (language, context) {
            (Some(lang), Some(ctx)) => {
                format!(*CODE_GENERATION_TEMPLATE, lang, prompt, ctx)
            }
            (Some(lang), None) => {
                format!(*SIMPLE_CODE_GENERATION_TEMPLATE, lang, prompt)
            }
            (None, Some(ctx)) => {
                format!(*CONTEXT_CODE_GENERATION_TEMPLATE, prompt, ctx)
            }
            (None, None) => {
                format!(*BASIC_CODE_GENERATION_TEMPLATE, prompt)
            }
        };

        let mut request = Self::new_text(&generation_prompt);
        request.generation_config = Some(GenerationConfig {
            temperature: temperature.or(Some(0.7)),
            top_p: Some(0.9),
            max_output_tokens: max_tokens.or(Some(1000)),
            candidate_count: Some(1),
            ..Default::default()
        });
        request
    }

    /// Create a request for code explanation
    pub fn new_code_explanation(code: &str, language: Option<&str>, detail_level: &str) -> Self {
        let detail_instruction = match detail_level {
            "basic" => DETAIL_BASIC,
            "expert" => DETAIL_EXPERT,
            _ => DETAIL_DEFAULT,
        };

        let explanation_prompt = if let Some(lang) = language {
            format!(
                *EXPLANATION_TEMPLATE_WITH_LANG,
                detail_instruction, lang, lang, code
            )
        } else {
            format!(*BASIC_EXPLANATION_TEMPLATE, detail_instruction, code)
        };

        let mut request = Self::new_text(&explanation_prompt);
        request.generation_config = Some(GenerationConfig {
            temperature: Some(0.3),
            top_p: Some(0.8),
            max_output_tokens: Some(1500),
            candidate_count: Some(1),
            ..Default::default()
        });
        request
    }
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: None,
            max_output_tokens: Some(1000),
            candidate_count: Some(1),
            stop_sequences: None,
        }
    }
}

impl GenerateContentResponse {
    /// Get the text content from the first candidate
    pub fn get_text(&self) -> Option<String> {
        self.candidates
            .first()?
            .content
            .parts
            .first()?
            .text
            .clone()
            .into()
    }

    /// Check if the response was blocked by safety filters
    pub fn is_blocked(&self) -> bool {
        self.prompt_feedback
            .as_ref()
            .and_then(|pf| pf.block_reason.as_ref())
            .is_some()
            || self.candidates.is_empty()
    }
}
