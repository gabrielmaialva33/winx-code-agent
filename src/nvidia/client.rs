//! NVIDIA API client implementation

use crate::errors::{Result, WinxError};
use crate::nvidia::{config::NvidiaConfig, models::*};
use reqwest::{header, Client, Response};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Rate limiting state
#[derive(Debug)]
struct RateLimiter {
    last_request: Instant,
    requests_this_minute: u32,
    minute_start: Instant,
}

impl RateLimiter {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            last_request: now,
            requests_this_minute: 0,
            minute_start: now,
        }
    }

    async fn check_rate_limit(&mut self, max_rpm: u32) -> Result<()> {
        let now = Instant::now();

        // Reset counter if a minute has passed
        if now.duration_since(self.minute_start).as_secs() >= 60 {
            self.requests_this_minute = 0;
            self.minute_start = now;
        }

        // Check if we've exceeded the rate limit
        if self.requests_this_minute >= max_rpm {
            let wait_time = 60 - now.duration_since(self.minute_start).as_secs();
            warn!("Rate limit exceeded, waiting {} seconds", wait_time);
            tokio::time::sleep(Duration::from_secs(wait_time)).await;
            self.requests_this_minute = 0;
            self.minute_start = Instant::now();
        }

        self.requests_this_minute += 1;
        self.last_request = now;
        Ok(())
    }
}

/// NVIDIA API client
#[derive(Debug, Clone)]
pub struct NvidiaClient {
    client: Client,
    config: NvidiaConfig,
    rate_limiter: Arc<Mutex<RateLimiter>>,
}

impl NvidiaClient {
    /// Create a new NVIDIA client
    pub async fn new(config: NvidiaConfig) -> Result<Self> {
        config.validate()?;

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", config.api_key)).map_err(|e| {
                WinxError::ConfigurationError(format!("Invalid API key format: {}", e))
            })?,
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&format!(
                "winx-code-agent/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .unwrap(),
        );

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .default_headers(headers)
            .build()
            .map_err(|e| WinxError::NetworkError(format!("Failed to create HTTP client: {}", e)))?;

        info!(
            "NVIDIA client initialized with base URL: {}",
            config.base_url
        );

        Ok(Self {
            client,
            config,
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
        })
    }

    /// Validate connection to NVIDIA API
    pub async fn validate_connection(&self) -> Result<()> {
        debug!("Validating NVIDIA API connection");

        let request = ChatCompletionRequest {
            model: self.config.default_model.clone(),
            messages: vec![ChatMessage::user("Test connection - respond with 'OK'")],
            max_tokens: Some(10),
            temperature: Some(0.1),
            top_p: None,
            stream: Some(false),
        };

        match self.chat_completion(&request).await {
            Ok(_) => {
                info!("NVIDIA API connection validated successfully");
                Ok(())
            }
            Err(e) => {
                error!("Failed to validate NVIDIA API connection: {}", e);
                Err(e)
            }
        }
    }

    /// Make a chat completion request
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        self.rate_limiter
            .lock()
            .await
            .check_rate_limit(self.config.rate_limit_rpm)
            .await?;

        debug!("Making chat completion request to model: {}", request.model);

        for attempt in 1..=self.config.max_retries {
            match self.make_request(request).await {
                Ok(response) => {
                    debug!("Chat completion successful on attempt {}", attempt);
                    return Ok(response);
                }
                Err(e) => {
                    warn!("Chat completion attempt {} failed: {}", attempt, e);
                    if attempt < self.config.max_retries {
                        let delay = Duration::from_millis(1000 * attempt as u64);
                        debug!("Retrying in {:?}", delay);
                        tokio::time::sleep(delay).await;
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        Err(WinxError::NetworkError(
            "All retry attempts failed".to_string(),
        ))
    }

    /// Make the actual HTTP request
    async fn make_request(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let url = self.config.chat_completions_url();

        let response = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| WinxError::NetworkError(format!("Request failed: {}", e)))?;

        self.handle_response(response).await
    }

    /// Handle HTTP response and convert to ChatCompletionResponse
    async fn handle_response(&self, response: Response) -> Result<ChatCompletionResponse> {
        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| WinxError::NetworkError(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            // Try to parse as API error
            if let Ok(api_error) = serde_json::from_str::<ApiError>(&response_text) {
                return Err(WinxError::ApiError(format!(
                    "NVIDIA API error ({}): {}",
                    status, api_error.error.message
                )));
            }

            return Err(WinxError::ApiError(format!(
                "HTTP error {}: {}",
                status, response_text
            )));
        }

        serde_json::from_str::<ChatCompletionResponse>(&response_text).map_err(|e| {
            WinxError::ParseError(format!(
                "Failed to parse response: {}. Response body: {}",
                e, response_text
            ))
        })
    }

    /// High-level method for code analysis
    pub async fn analyze_code(
        &self,
        code: &str,
        language: Option<&str>,
    ) -> Result<CodeAnalysisResult> {
        let language_context = language
            .map(|l| format!(" (written in {})", l))
            .unwrap_or_default();

        let system_prompt = "You are an expert code analyzer. Analyze the provided code and return your analysis in JSON format with the following structure:
{
  \"summary\": \"Brief summary of what the code does\",
  \"issues\": [
    {
      \"severity\": \"Error|Warning|Info|Critical\",
      \"category\": \"Bug|Performance|Security|Style|Maintainability|Documentation\",
      \"message\": \"Description of the issue\",
      \"line\": 10,
      \"suggestion\": \"How to fix it\"
    }
  ],
  \"suggestions\": [\"General improvement suggestions\"],
  \"complexity_score\": 75
}";

        let user_prompt = format!(
            "Analyze this code{}:\n\n```\n{}\n```",
            language_context, code
        );

        let request = ChatCompletionRequest {
            model: NvidiaModel::for_task(TaskType::CodeAnalysis)
                .as_str()
                .to_string(),
            messages: vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user(user_prompt),
            ],
            max_tokens: Some(2048),
            temperature: Some(0.1),
            top_p: None,
            stream: Some(false),
        };

        let response = self.chat_completion(&request).await?;

        if let Some(choice) = response.choices.first() {
            let effective_content = choice.message.effective_content();
            
            // Try to parse as JSON, fallback to plain text summary
            if let Ok(analysis) = serde_json::from_str::<CodeAnalysisResult>(&effective_content) {
                Ok(analysis)
            } else {
                // Fallback: create a simple analysis from the response
                Ok(CodeAnalysisResult {
                    summary: effective_content,
                    issues: vec![],
                    suggestions: vec![],
                    complexity_score: None,
                })
            }
        } else {
            Err(WinxError::ApiError(
                "Empty response from NVIDIA API".to_string(),
            ))
        }
    }

    /// High-level method for code generation
    pub async fn generate_code(
        &self,
        request: &CodeGenerationRequest,
    ) -> Result<CodeGenerationResult> {
        let language_context = request
            .language
            .as_ref()
            .map(|l| format!(" in {}", l))
            .unwrap_or_default();

        let context_info = request
            .context
            .as_ref()
            .map(|c| format!("\n\nContext: {}", c))
            .unwrap_or_default();

        let system_prompt = format!(
            "You are an expert software developer. Generate high-quality code{} based on the user's request. \
            Provide clean, well-documented, and efficient code. Always include explanatory comments for complex logic.{}",
            language_context, context_info
        );

        let chat_request = ChatCompletionRequest {
            model: NvidiaModel::for_task(TaskType::CodeGeneration)
                .as_str()
                .to_string(),
            messages: vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user(&request.prompt),
            ],
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            top_p: None,
            stream: Some(false),
        };

        let response = self.chat_completion(&chat_request).await?;

        if let Some(choice) = response.choices.first() {
            Ok(CodeGenerationResult {
                code: choice.message.effective_content(),
                language: request.language.clone(),
                explanation: None,
                tests: None,
            })
        } else {
            Err(WinxError::ApiError(
                "Empty response from NVIDIA API".to_string(),
            ))
        }
    }

    /// Get model recommendations for specific tasks
    pub fn recommend_model(&self, task_type: TaskType) -> NvidiaModel {
        NvidiaModel::for_task(task_type)
    }
}
