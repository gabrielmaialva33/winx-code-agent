//! Google Gemini HTTP client implementation

use crate::errors::{Result, WinxError};
use crate::gemini::{GeminiConfig, GenerateContentRequest, GenerateContentResponse};
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
    fn new(rpm: u32) -> Self {
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

/// Google Gemini HTTP client
pub struct GeminiClient {
    config: GeminiConfig,
    client: reqwest::Client,
    rate_limit: Arc<Mutex<RateLimit>>,
}

impl GeminiClient {
    /// Create a new Gemini client
    pub fn new(config: GeminiConfig) -> Result<Self> {
        config.validate()?;
        
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .user_agent("Winx-Code-Agent/1.0")
            .build()
            .map_err(|e| WinxError::NetworkError(format!("Failed to create HTTP client: {}", e)))?;

        let rate_limit = Arc::new(Mutex::new(RateLimit::new(config.rate_limit_rpm)));

        info!("Gemini client initialized with model: {}", config.model);
        
        Ok(Self {
            config,
            client,
            rate_limit,
        })
    }

    /// Make a request to the Gemini API with retries
    pub async fn generate_content(&self, request: &GenerateContentRequest) -> Result<GenerateContentResponse> {
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

            debug!("Gemini API attempt {} of {}", attempt, self.config.max_retries);
            
            match self.make_request(request).await {
                Ok(response) => {
                    info!("Gemini API request successful on attempt {}", attempt);
                    return Ok(response);
                }
                Err(e) => {
                    warn!("Gemini API attempt {} failed: {}", attempt, e);
                    last_error = Some(e);
                    
                    if attempt < self.config.max_retries {
                        let delay = Duration::from_millis(1000 * attempt as u64);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| {
            WinxError::NetworkError("All Gemini API attempts failed".to_string())
        }))
    }

    /// Make a single request to the Gemini API
    async fn make_request(&self, request: &GenerateContentRequest) -> Result<GenerateContentResponse> {
        let endpoint = self.config.model.endpoint();
        let url = self.config.endpoint_url(&endpoint);
        
        debug!("Making Gemini API request to: {}", url);
        
        let response = self.client
            .post(&url)
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
            
            error!("Gemini API error {}: {}", status, error_text);
            return Err(WinxError::NetworkError(format!(
                "Gemini API error {}: {}", status, error_text
            )));
        }

        let gemini_response: GenerateContentResponse = response
            .json()
            .await
            .map_err(|e| WinxError::SerializationError(format!("Failed to parse response: {}", e)))?;

        if gemini_response.is_blocked() {
            warn!("Gemini response was blocked by safety filters");
            return Err(WinxError::ExternalApiError(
                "Response blocked by Gemini safety filters".to_string()
            ));
        }

        Ok(gemini_response)
    }

    /// Analyze code using Gemini
    pub async fn analyze_code(&self, code: &str, language: Option<&str>) -> Result<String> {
        let request = GenerateContentRequest::new_code_analysis(code, language);
        
        debug!("Analyzing code with Gemini model: {}", self.config.model);
        
        let response = self.generate_content(&request).await?;
        
        response
            .get_text()
            .ok_or_else(|| WinxError::ExternalApiError("Empty response from Gemini".to_string()))
    }

    /// Generate code using Gemini
    pub async fn generate_code(
        &self,
        prompt: &str,
        language: Option<&str>,
        context: Option<&str>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<String> {
        let request = GenerateContentRequest::new_code_generation(
            prompt, language, context, max_tokens, temperature
        );
        
        debug!("Generating code with Gemini model: {}", self.config.model);
        
        let response = self.generate_content(&request).await?;
        
        response
            .get_text()
            .ok_or_else(|| WinxError::ExternalApiError("Empty response from Gemini".to_string()))
    }

    /// Explain code using Gemini
    pub async fn explain_code(
        &self,
        code: &str,
        language: Option<&str>,
        detail_level: &str,
    ) -> Result<String> {
        let request = GenerateContentRequest::new_code_explanation(code, language, detail_level);
        
        debug!("Explaining code with Gemini model: {}", self.config.model);
        
        let response = self.generate_content(&request).await?;
        
        response
            .get_text()
            .ok_or_else(|| WinxError::ExternalApiError("Empty response from Gemini".to_string()))
    }

    /// Test the connection to Gemini API
    pub async fn test_connection(&self) -> Result<()> {
        let test_request = GenerateContentRequest::new_text("Hello, can you respond with 'OK'?");
        
        debug!("Testing Gemini API connection");
        
        let response = self.generate_content(&test_request).await?;
        
        if response.get_text().is_some() {
            info!("Gemini API connection test successful");
            Ok(())
        } else {
            Err(WinxError::ExternalApiError("Gemini connection test failed".to_string()))
        }
    }

    /// Get the current model being used
    pub fn get_model(&self) -> &str {
        self.config.model.model_name()
    }

    /// Get configuration
    pub fn get_config(&self) -> &GeminiConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::GeminiModel;

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
    fn test_gemini_config_validation() {
        let mut config = GeminiConfig::default();
        config.api_key = "AIzaValidKey".to_string();
        
        assert!(config.validate().is_ok());
        
        // Test invalid API key
        config.api_key = "InvalidKey".to_string();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn test_client_creation() {
        let mut config = GeminiConfig::default();
        config.api_key = "AIzaTestKey".to_string();
        
        let client = GeminiClient::new(config);
        assert!(client.is_ok());
    }
}