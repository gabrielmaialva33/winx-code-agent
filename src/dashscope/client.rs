//! DashScope HTTP client implementation

use crate::dashscope::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, DashScopeConfig,
};
use crate::errors::{Result, WinxError};
use backoff::ExponentialBackoff;
use backoff::backoff::Backoff;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const UNKNOWN_ERROR: &str = "Unknown error";
const TEST_MESSAGE: &str = "Hello, can you respond with 'OK'?";
const ALL_ATTEMPTS_FAILED: &str = "All DashScope API attempts failed";
const REQUEST_FAILED: &str = "Request failed: {}";
const API_ERROR: &str = "DashScope API error {}: {}";
const PARSE_FAILED: &str = "Failed to parse response: {}";
const EMPTY_RESPONSE: &str = "Empty response from DashScope";
const CONNECTION_TEST_FAILED: &str = "DashScope connection test failed";

/// Rate limiting information
#[derive(Debug)]
struct RateLimit {
    timestamps: VecDeque<Instant>,
    rpm: u32,
}

impl RateLimit {
    fn new(rpm: u32) -> Self {
        Self {
            timestamps: VecDeque::new(),
            rpm,
        }
    }

    fn can_make_request(&mut self) -> Option<Duration> {
        let now = Instant::now();

        // Remove timestamps older than 60 seconds
        while let Some(&front) = self.timestamps.front() {
            if now.duration_since(front) > Duration::from_secs(60) {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }

        if self.timestamps.len() < self.rpm as usize {
            Some(Duration::from_secs(0))
        } else {
            // Calculate wait time until the oldest request expires
            let oldest = self
                .timestamps
                .front()
                .expect("Timestamps should not be empty when len >= rpm");
            let elapsed = now.duration_since(*oldest);
            let remaining = Duration::from_secs(60).saturating_sub(elapsed);
            Some(remaining)
        }
    }

    fn record_request(&mut self) {
        self.timestamps.push_back(Instant::now());
    }
}

/// DashScope HTTP client
#[derive(Clone)]
pub struct DashScopeClient {
    config: DashScopeConfig,
    client: reqwest::Client,
    rate_limit: Arc<Mutex<RateLimit>>,
}

impl DashScopeClient {
    /// Create a new DashScope client
    pub fn new(config: DashScopeConfig) -> Result<Self> {
        config.validate()?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .user_agent("Winx-Code-Agent/1.0")
            .build()
            .map_err(|e| WinxError::NetworkError {
                message: Arc::new(format!("Failed to create HTTP client: {}", e)),
            })?;

        let rate_limit = Arc::new(Mutex::new(RateLimit::new(config.rate_limit_rpm)));

        info!("DashScope client initialized with model: {}", config.model);

        Ok(Self {
            config,
            client,
            rate_limit,
        })
    }

    /// Make a chat completion request
    pub async fn chat_completion<'a>(
        &self,
        request: &ChatCompletionRequest<'a>,
    ) -> Result<ChatCompletionResponse> {
        let mut backoff = ExponentialBackoff::default();
        let mut last_error = None;

        for attempt in 1..=self.config.max_retries {
            // Check rate limit
            let wait_time = {
                let mut rate_limit = self.rate_limit.lock().await;
                rate_limit.can_make_request()
            };

            if let Some(wait) = wait_time
                && !wait.is_zero() {
                    warn!("Rate limit exceeded, waiting for {:?}", wait);
                    tokio::time::sleep(wait).await;
                    continue;
                }

            // Record the request
            {
                let mut rate_limit = self.rate_limit.lock().await;
                rate_limit.record_request();
            }

            debug!(
                "DashScope API attempt {} of {}",
                attempt, self.config.max_retries
            );

            match self.make_request(request).await {
                Ok(response) => {
                    info!("DashScope API request successful on attempt {}", attempt);
                    return Ok(response);
                }
                Err(e) => {
                    warn!("DashScope API attempt {} failed: {}", attempt, e);
                    last_error = Some(e);

                    if attempt < self.config.max_retries {
                        let delay = backoff.next_backoff().unwrap_or(Duration::from_secs(1));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| WinxError::NetworkError {
            message: Arc::new(ALL_ATTEMPTS_FAILED.to_string()),
        }))
    }

    /// Make a single request to the DashScope API
    async fn make_request<'a>(
        &self,
        request: &ChatCompletionRequest<'a>,
    ) -> Result<ChatCompletionResponse> {
        let url = self.config.chat_completions_url();

        debug!("Making DashScope API request to: {}", url);

        let response = self
            .client
            .post(url)
            .header("Authorization", &*self.config.authorization_header)
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| WinxError::NetworkError {
                message: Arc::new(format!("Failed to send request: {}", e)),
            })?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| UNKNOWN_ERROR.to_string());

            error!("DashScope API error {}: {}", status, error_text);
            return Err(WinxError::NetworkError {
                message: Arc::new(format!("DashScope API error {}: {}", status, error_text)),
            });
        }

        let completion_response: ChatCompletionResponse =
            response
                .json()
                .await
                .map_err(|e| WinxError::SerializationError {
                    message: Arc::new(format!("Failed to parse response: {}", e)),
                })?;

        if !completion_response.is_success() {
            warn!("DashScope response has no choices");
            return Err(WinxError::ApiError {
                message: Arc::new(EMPTY_RESPONSE.to_string()),
            });
        }

        Ok(completion_response)
    }

    /// Analyze code using DashScope
    pub async fn analyze_code(&self, code: &str, language: Option<&str>) -> Result<String> {
        let request = ChatCompletionRequest::new_code_analysis(
            self.config.model.model_name(),
            code,
            language,
        );

        debug!("Analyzing code with DashScope model: {}", self.config.model);

        let response = self.chat_completion(&request).await?;

        response
            .get_content()
            .map(|s| s.to_string())
            .ok_or_else(|| WinxError::ApiError {
                message: Arc::new("No content in response".to_string()),
            })
    }

    /// Generate code using DashScope
    pub async fn generate_code(
        &self,
        prompt: &str,
        language: Option<&str>,
        context: Option<&str>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<String> {
        let request = ChatCompletionRequest::new_code_generation(
            self.config.model.model_name(),
            prompt,
            language,
            context,
            max_tokens,
            temperature,
        );

        debug!(
            "Generating code with DashScope model: {}",
            self.config.model
        );

        let response = self.chat_completion(&request).await?;

        response
            .get_content()
            .map(|s| s.to_string())
            .ok_or_else(|| WinxError::ApiError {
                message: Arc::new("No content in response".to_string()),
            })
    }

    /// Explain code using DashScope
    pub async fn explain_code(
        &self,
        code: &str,
        language: Option<&str>,
        detail_level: &str,
    ) -> Result<String> {
        let request = ChatCompletionRequest::new_code_explanation(
            self.config.model.model_name(),
            code,
            language,
            detail_level,
        );

        debug!(
            "Explaining code with DashScope model: {}",
            self.config.model
        );

        let response = self.chat_completion(&request).await?;

        response
            .get_content()
            .map(|s| s.to_string())
            .ok_or_else(|| WinxError::ApiError {
                message: Arc::new("No content in response".to_string()),
            })
    }

    /// Test the connection to DashScope API
    pub async fn test_connection(&self) -> Result<()> {
        let messages = vec![ChatMessage::user(TEST_MESSAGE.to_string())];
        let test_request = ChatCompletionRequest::new(self.config.model.model_name(), messages);

        debug!("Testing DashScope API connection");

        let response = self.chat_completion(&test_request).await?;

        if response.is_success() {
            info!("DashScope API connection test successful");
            Ok(())
        } else {
            Err(WinxError::ApiError {
                message: Arc::new("API connection test failed".to_string()),
            })
        }
    }

    /// Get the current model being used
    pub fn get_model(&self) -> &str {
        self.config.model.model_name()
    }

    /// Get configuration
    pub fn get_config(&self) -> &DashScopeConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit() {
        let mut rate_limit = RateLimit::new(5);

        // Should allow first
        for _ in 0..5 {
            assert_eq!(rate_limit.can_make_request(), Some(Duration::from_secs(0)));
            rate_limit.record_request();
        }

        // Should deny 6th request and return wait time
        let wait = rate_limit.can_make_request();
        assert!(wait.is_some() && wait.unwrap() > Duration::from_secs(0));
    }

    #[test]
    fn test_dashscope_config_validation() {
        let mut config = DashScopeConfig {
            api_key: "sk-validkey123".to_string(),
            ..Default::default()
        };

        assert!(config.validate().is_ok());

        // Test invalid API key
        config.api_key = "InvalidKey".to_string();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn test_client_creation() {
        let config = DashScopeConfig {
            api_key: "sk-testkey123".to_string(),
            ..Default::default()
        };

        let client = DashScopeClient::new(config);
        assert!(client.is_ok());
    }
}
