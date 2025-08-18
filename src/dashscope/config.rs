//! DashScope configuration

use crate::errors::{Result, WinxError};
use std::env;

/// Available DashScope models
#[derive(Debug, Clone)]
pub enum DashScopeModel {
    /// Qwen3 Coder Plus - Optimized for code generation and analysis
    Qwen3CoderPlus,
    /// Qwen3 32B - General purpose model
    Qwen3_32B,
    /// Qwen3 72B - Larger general purpose model
    Qwen3_72B,
}

impl DashScopeModel {
    /// Get the model name for API requests
    pub fn model_name(&self) -> &'static str {
        match self {
            DashScopeModel::Qwen3CoderPlus => "qwen3-coder-plus",
            DashScopeModel::Qwen3_32B => "qwen3-32b",
            DashScopeModel::Qwen3_72B => "qwen3-72b",
        }
    }
}

impl std::fmt::Display for DashScopeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.model_name())
    }
}

/// DashScope configuration
#[derive(Debug, Clone)]
pub struct DashScopeConfig {
    /// API key for DashScope
    pub api_key: String,
    /// Primary model to use
    pub model: DashScopeModel,
    /// Base URL for DashScope API
    pub base_url: String,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
    /// Maximum number of retries
    pub max_retries: u32,
    /// Rate limit in requests per minute
    pub rate_limit_rpm: u32,
    /// Temperature for generation (0.0-2.0)
    pub temperature: f32,
    /// Top-p for generation (0.0-1.0)
    pub top_p: f32,
    /// Enable streaming responses
    pub stream: bool,
}

impl Default for DashScopeConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: DashScopeModel::Qwen3CoderPlus,
            base_url: "https://dashscope-intl.aliyuncs.com".to_string(),
            timeout_seconds: 30,
            max_retries: 3,
            rate_limit_rpm: 60,
            temperature: 1.0,
            top_p: 0.8,
            stream: true,
        }
    }
}

impl DashScopeConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("DASHSCOPE_API_KEY")
            .map_err(|_| WinxError::ConfigurationError("DASHSCOPE_API_KEY not set".to_string()))?;

        let model = env::var("DASHSCOPE_MODEL").unwrap_or_else(|_| "qwen3-coder-plus".to_string());
        let model = Self::parse_model(&model)?;

        let base_url = env::var("DASHSCOPE_BASE_URL")
            .unwrap_or_else(|_| "https://dashscope-intl.aliyuncs.com".to_string());

        let timeout_seconds = env::var("DASHSCOPE_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .map_err(|_| {
                WinxError::ConfigurationError("Invalid DASHSCOPE_TIMEOUT_SECONDS".to_string())
            })?;

        let max_retries = env::var("DASHSCOPE_MAX_RETRIES")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .map_err(|_| {
                WinxError::ConfigurationError("Invalid DASHSCOPE_MAX_RETRIES".to_string())
            })?;

        let rate_limit_rpm = env::var("DASHSCOPE_RATE_LIMIT_RPM")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .map_err(|_| {
                WinxError::ConfigurationError("Invalid DASHSCOPE_RATE_LIMIT_RPM".to_string())
            })?;

        let temperature = env::var("DASHSCOPE_TEMPERATURE")
            .unwrap_or_else(|_| "1.0".to_string())
            .parse()
            .map_err(|_| {
                WinxError::ConfigurationError("Invalid DASHSCOPE_TEMPERATURE".to_string())
            })?;

        let top_p = env::var("DASHSCOPE_TOP_P")
            .unwrap_or_else(|_| "0.8".to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError("Invalid DASHSCOPE_TOP_P".to_string()))?;

        let stream = env::var("DASHSCOPE_STREAM")
            .unwrap_or_else(|_| "true".to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError("Invalid DASHSCOPE_STREAM".to_string()))?;

        Ok(Self {
            api_key,
            model,
            base_url,
            timeout_seconds,
            max_retries,
            rate_limit_rpm,
            temperature,
            top_p,
            stream,
        })
    }

    /// Parse model string to DashScopeModel enum
    fn parse_model(model_str: &str) -> Result<DashScopeModel> {
        match model_str {
            "qwen3-coder-plus" => Ok(DashScopeModel::Qwen3CoderPlus),
            "qwen3-32b" => Ok(DashScopeModel::Qwen3_32B),
            "qwen3-72b" => Ok(DashScopeModel::Qwen3_72B),
            _ => Err(WinxError::ConfigurationError(format!(
                "Unknown DashScope model: {}. Supported models: qwen3-coder-plus, qwen3-32b, qwen3-72b",
                model_str
            ))),
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        if self.api_key.is_empty() {
            return Err(WinxError::ConfigurationError(
                "DashScope API key cannot be empty".to_string(),
            ));
        }

        if !self.api_key.starts_with("sk-") {
            return Err(WinxError::ConfigurationError(
                "Invalid DashScope API key format (should start with 'sk-')".to_string(),
            ));
        }

        if self.timeout_seconds == 0 || self.timeout_seconds > 300 {
            return Err(WinxError::ConfigurationError(
                "Timeout must be between 1 and 300 seconds".to_string(),
            ));
        }

        if self.max_retries > 10 {
            return Err(WinxError::ConfigurationError(
                "Max retries cannot exceed 10".to_string(),
            ));
        }

        if self.rate_limit_rpm == 0 || self.rate_limit_rpm > 1000 {
            return Err(WinxError::ConfigurationError(
                "Rate limit must be between 1 and 1000 RPM".to_string(),
            ));
        }

        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(WinxError::ConfigurationError(
                "Temperature must be between 0.0 and 2.0".to_string(),
            ));
        }

        if !(0.0..=1.0).contains(&self.top_p) {
            return Err(WinxError::ConfigurationError(
                "Top-p must be between 0.0 and 1.0".to_string(),
            ));
        }

        Ok(())
    }

    /// Get the chat completions endpoint URL
    pub fn chat_completions_url(&self) -> String {
        format!("{}/compatible-mode/v1/chat/completions", self.base_url)
    }
}
