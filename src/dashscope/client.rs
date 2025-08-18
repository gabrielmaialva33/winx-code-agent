//! DashScope HTTP client implementation

use crate::dashscope::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, DashScopeConfig,
};
use crate::errors::{Result, WinxError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Rate limiting information
#[derive(Debug)]
struct RateLimit {
    requests_count: u32,
    window_start: Instant,
    window_duration: Duration,
}

impl RateLimit {
    fn new(_rpm: u32) -> Self {
        Self {
            requests_count: 0,
            window_start: Instant::now(),
            window_duration: Duration::from_secs(60),
        }
    }

    fn can_make_request(&mut self, max_rpm: u32) -> bool {
        let now = Instant::now();

        // Reset window if it has passed
        if now.duration_since(self.window_start) >= self.window_duration {
            self.requests_count = 0;
            self.window_start = now;
        }

        self.requests_count < max_rpm
    }

    fn record_request(&mut self) {
        self.requests_count += 1;
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
            .map_err(|e| WinxError::NetworkError(format!("Failed to create HTTP client: {}", e)))?;

        let rate_limit = Arc::new(Mutex::new(RateLimit::new(config.rate_limit_rpm)));

        info!("DashScope client initialized with model: {}", config.model);

        Ok(Self {
            config,
            client,
            rate_limit,
        })
    }

    /// Make a chat completion request
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let mut last_error = None;

        for attempt in 1..=self.config.max_retries {
            // Check rate limit
            {
                let mut rate_limit = self.rate_limit.lock().await;
                if !rate_limit.can_make_request(self.config.rate_limit_rpm) {
                    warn!("Rate limit exceeded, waiting...");
                    drop(rate_limit);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
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
                        let delay = Duration::from_millis(1000 * attempt as u64);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            WinxError::NetworkError("All DashScope API attempts failed".to_string())
        }))
    }

    /// Make a single request to the DashScope API
    async fn make_request(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let url = self.config.chat_completions_url();

        debug!("Making DashScope API request to: {}", url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| WinxError::NetworkError(format!("Request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            error!("DashScope API error {}: {}", status, error_text);
            return Err(WinxError::NetworkError(format!(
                "DashScope API error {}: {}",
                status, error_text
            )));
        }

        let completion_response: ChatCompletionResponse = response.json().await.map_err(|e| {
            WinxError::SerializationError(format!("Failed to parse response: {}", e))
        })?;

        if !completion_response.is_success() {
            warn!("DashScope response has no choices");
            return Err(WinxError::ApiError(
                "Empty response from DashScope".to_string(),
            ));
        }

        Ok(completion_response)
    }

    /// Analyze code using DashScope
    pub async fn analyze_code(&self, code: &str, language: Option<&str>) -> Result<String> {
        let request = ChatCompletionRequest::new_code_analysis(
            self.config.model.model_name().to_string(),
            code,
            language,
        );

        debug!("Analyzing code with DashScope model: {}", self.config.model);

        let response = self.chat_completion(&request).await?;

        response
            .get_content()
            .map(|s| s.to_string())
            .ok_or_else(|| WinxError::ApiError("Empty response from DashScope".to_string()))
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
            self.config.model.model_name().to_string(),
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
            .ok_or_else(|| WinxError::ApiError("Empty response from DashScope".to_string()))
    }

    /// Explain code using DashScope
    pub async fn explain_code(
        &self,
        code: &str,
        language: Option<&str>,
        detail_level: &str,
    ) -> Result<String> {
        let request = ChatCompletionRequest::new_code_explanation(
            self.config.model.model_name().to_string(),
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
            .ok_or_else(|| WinxError::ApiError("Empty response from DashScope".to_string()))
    }

    /// Test the connection to DashScope API
    pub async fn test_connection(&self) -> Result<()> {
        let messages = vec![ChatMessage::user(
            "Hello, can you respond with 'OK'?".to_string(),
        )];
        let test_request =
            ChatCompletionRequest::new(self.config.model.model_name().to_string(), messages);

        debug!("Testing DashScope API connection");

        let response = self.chat_completion(&test_request).await?;

        if response.is_success() {
            info!("DashScope API connection test successful");
            Ok(())
        } else {
            Err(WinxError::ApiError(
                "DashScope connection test failed".to_string(),
            ))
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
    use crate::dashscope::config::DashScopeModel;

    #[test]
    fn test_rate_limit() {
        let mut rate_limit = RateLimit::new(5);

        // Should allow first 5 requests
        for _ in 0..5 {
            assert!(rate_limit.can_make_request(5));
            rate_limit.record_request();
        }

        // Should deny 6th request
        assert!(!rate_limit.can_make_request(5));
    }

    #[test]
    fn test_dashscope_config_validation() {
        let mut config = DashScopeConfig::default();
        config.api_key = "sk-validkey123".to_string();

        assert!(config.validate().is_ok());

        // Test invalid API key
        config.api_key = "InvalidKey".to_string();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn test_client_creation() {
        let mut config = DashScopeConfig::default();
        config.api_key = "sk-testkey123".to_string();

        let client = DashScopeClient::new(config);
        assert!(client.is_ok());
    }
}
