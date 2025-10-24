//! DashScope configuration

use crate::errors::{Result, WinxError};
use lazy_static::lazy_static;
use std::borrow::Cow;
use std::env;
use std::sync::Arc;

const DEFAULT_MODEL: &str = "qwen3-coder-plus";
const DEFAULT_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com";
const DEFAULT_TIMEOUT: &str = "30";
const DEFAULT_MAX_RETRIES: &str = "3";
const DEFAULT_RATE_LIMIT: &str = "60";
const DEFAULT_TEMPERATURE: &str = "1.0";
const DEFAULT_TOP_P: &str = "0.8";
const DEFAULT_STREAM: &str = "true";

const ERR_API_KEY_NOT_SET: &str = "DASHSCOPE_API_KEY not set";
const ERR_INVALID_TIMEOUT: &str = "Invalid DASHSCOPE_TIMEOUT_SECONDS";
const ERR_INVALID_MAX_RETRIES: &str = "Invalid DASHSCOPE_MAX_RETRIES";
const ERR_INVALID_RATE_LIMIT: &str = "Invalid DASHSCOPE_RATE_LIMIT_RPM";
const ERR_INVALID_TEMPERATURE: &str = "Invalid DASHSCOPE_TEMPERATURE";
const ERR_INVALID_TOP_P: &str = "Invalid DASHSCOPE_TOP_P";
const ERR_INVALID_STREAM: &str = "Invalid DASHSCOPE_STREAM";
const ERR_UNKNOWN_MODEL: &str =
    "Unknown DashScope model: {}. Supported models: qwen3-coder-plus, qwen3-32b, qwen3-72b";
const ERR_API_KEY_EMPTY: &str = "DashScope API key cannot be empty";
const ERR_INVALID_API_KEY_FORMAT: &str =
    "Invalid DashScope API key format (should start with 'sk-')";
const ERR_TIMEOUT_RANGE: &str = "Timeout must be between 1 and 300 seconds";
const ERR_MAX_RETRIES_EXCEED: &str = "Max retries cannot exceed 10";
const ERR_RATE_LIMIT_RANGE: &str = "Rate limit must be between 1 and 1000 RPM";
const ERR_TEMPERATURE_RANGE: &str = "Temperature must be between 0.0 and 2.0";
const ERR_TOP_P_RANGE: &str = "Top-p must be between 0.0 and 1.0";

lazy_static::lazy_static! {
    /// Cached default chat completions URL for DashScope
    static ref DEFAULT_DASHSCOPE_CHAT_COMPLETIONS_URL: &'static str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions";
}

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
    /// Cached authorization header
    pub(crate) authorization_header: Arc<str>,
    /// Cached chat completions URL
    pub(crate) chat_completions_url_cached: Arc<str>,
}

impl Default for DashScopeConfig {
    fn default() -> Self {
        let api_key = String::new();
        let authorization_header = Arc::from(format!("Bearer {}", api_key));
        let chat_completions_url_cached = Arc::from(*DEFAULT_DASHSCOPE_CHAT_COMPLETIONS_URL);

        Self {
            api_key,
            model: DashScopeModel::Qwen3CoderPlus,
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout_seconds: 30,
            max_retries: 3,
            rate_limit_rpm: 60,
            temperature: 1.0,
            top_p: 0.8,
            stream: true,
            authorization_header,
            chat_completions_url_cached,
        }
    }
}

impl DashScopeConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("DASHSCOPE_API_KEY")
            .map_err(|_| WinxError::ConfigurationError(ERR_API_KEY_NOT_SET.to_string()))?;

        let model = env::var("DASHSCOPE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let model = Self::parse_model(&model)?;

        let base_url =
            env::var("DASHSCOPE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

        let timeout_seconds = env::var("DASHSCOPE_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| DEFAULT_TIMEOUT.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_TIMEOUT.to_string()))?;

        let max_retries = env::var("DASHSCOPE_MAX_RETRIES")
            .unwrap_or_else(|_| DEFAULT_MAX_RETRIES.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_MAX_RETRIES.to_string()))?;

        let rate_limit_rpm = env::var("DASHSCOPE_RATE_LIMIT_RPM")
            .unwrap_or_else(|_| DEFAULT_RATE_LIMIT.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_RATE_LIMIT.to_string()))?;

        let temperature = env::var("DASHSCOPE_TEMPERATURE")
            .unwrap_or_else(|_| DEFAULT_TEMPERATURE.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_TEMPERATURE.to_string()))?;

        let top_p = env::var("DASHSCOPE_TOP_P")
            .unwrap_or_else(|_| DEFAULT_TOP_P.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_TOP_P.to_string()))?;

        let stream = env::var("DASHSCOPE_STREAM")
            .unwrap_or_else(|_| DEFAULT_STREAM.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_STREAM.to_string()))?;

        let authorization_header = Arc::from(format!("Bearer {}", api_key));
        let chat_completions_url_cached = if base_url == "https://dashscope-intl.aliyuncs.com" {
            Arc::from(*DEFAULT_DASHSCOPE_CHAT_COMPLETIONS_URL)
        } else {
            Arc::from(format!("{}/compatible-mode/v1/chat/completions", base_url))
        };

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
            authorization_header,
            chat_completions_url_cached,
        })
    }

    /// Parse model string to DashScopeModel enum
    fn parse_model(model_str: &str) -> Result<DashScopeModel> {
        match model_str {
            "qwen3-coder-plus" => Ok(DashScopeModel::Qwen3CoderPlus),
            "qwen3-32b" => Ok(DashScopeModel::Qwen3_32B),
            "qwen3-72b" => Ok(DashScopeModel::Qwen3_72B),
            _ => Err(WinxError::ConfigurationError(format!(
                ERR_UNKNOWN_MODEL,
                model_str
            ))),
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        if self.api_key.is_empty() {
            return Err(WinxError::ConfigurationError(ERR_API_KEY_EMPTY.to_string()));
        }

        if !self.api_key.starts_with("sk-") {
            return Err(WinxError::ConfigurationError(
                ERR_INVALID_API_KEY_FORMAT.to_string(),
            ));
        }

        if self.timeout_seconds == 0 || self.timeout_seconds > 300 {
            return Err(WinxError::ConfigurationError(ERR_TIMEOUT_RANGE.to_string()));
        }

        if self.max_retries > 10 {
            return Err(WinxError::ConfigurationError(
                ERR_MAX_RETRIES_EXCEED.to_string(),
            ));
        }

        if self.rate_limit_rpm == 0 || self.rate_limit_rpm > 1000 {
            return Err(WinxError::ConfigurationError(
                ERR_RATE_LIMIT_RANGE.to_string(),
            ));
        }

        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(WinxError::ConfigurationError(
                ERR_TEMPERATURE_RANGE.to_string(),
            ));
        }

        if !(0.0..=1.0).contains(&self.top_p) {
            return Err(WinxError::ConfigurationError(ERR_TOP_P_RANGE.to_string()));
        }

        Ok(())
    }

    /// Get the chat completions endpoint URL
    pub fn chat_completions_url(&self) -> &str {
        &self.chat_completions_url_cached
    }
}
