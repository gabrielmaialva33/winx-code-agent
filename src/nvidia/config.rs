//! Configuration for NVIDIA API integration

use crate::errors::{Result, WinxError};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::env;
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://integrate.api.nvidia.com";
const DEFAULT_MODEL: &str = "qwen/qwen3-235b-a22b";
const ERR_API_KEY_MISSING: &str =
    "NVIDIA API key not found. Set NVIDIA_API_KEY or NVAPI_KEY environment variable";
const ERR_API_KEY_EMPTY: &str = "API key cannot be empty";
const ERR_BASE_URL_EMPTY: &str = "Base URL cannot be empty";
const ERR_TIMEOUT_ZERO: &str = "Timeout must be greater than 0";
const ERR_RATE_LIMIT_ZERO: &str = "Rate limit must be greater than 0";

lazy_static::lazy_static! {
    /// Cached default chat completions URL for NVIDIA
    static ref DEFAULT_NVIDIA_CHAT_COMPLETIONS_URL: &'static str = "https://integrate.api.nvidia.com/v1/chat/completions";
}

/// Configuration for NVIDIA API client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaConfig {
    /// NVIDIA API key
    pub api_key: String,
    /// Base URL for NVIDIA API (default: https://integrate.api.nvidia.com)
    pub base_url: String,
    /// Default model for general tasks
    pub default_model: String,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
    /// Maximum number of retries for failed requests
    pub max_retries: u32,
    /// Rate limiting: max requests per minute
    pub rate_limit_rpm: u32,
}

impl Default for NvidiaConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_model: DEFAULT_MODEL.to_string(),
            timeout_seconds: 30,
            max_retries: 3,
            rate_limit_rpm: 60,
        }
    }
}

impl NvidiaConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("NVIDIA_API_KEY")
            .or_else(|_| env::var("NVAPI_KEY"))
            .map_err(|_| WinxError::ConfigurationError {
                message: Arc::new(ERR_API_KEY_MISSING.to_string()),
            })?;

        let mut config = Self {
            api_key,
            ..Default::default()
        };

        // Override defaults with environment variables if present
        if let Ok(base_url) = env::var("NVIDIA_BASE_URL") {
            config.base_url = base_url;
        }

        if let Ok(model) = env::var("NVIDIA_DEFAULT_MODEL") {
            config.default_model = model;
        }

        if let Ok(timeout) = env::var("NVIDIA_TIMEOUT_SECONDS") {
            config.timeout_seconds = timeout.parse().unwrap_or(config.timeout_seconds);
        }

        if let Ok(retries) = env::var("NVIDIA_MAX_RETRIES") {
            config.max_retries = retries.parse().unwrap_or(config.max_retries);
        }

        if let Ok(rpm) = env::var("NVIDIA_RATE_LIMIT_RPM") {
            config.rate_limit_rpm = rpm.parse().unwrap_or(config.rate_limit_rpm);
        }

        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        if self.api_key.is_empty() {
            return Err(WinxError::ConfigurationError {
                message: Arc::new(ERR_API_KEY_EMPTY.to_string()),
            });
        }

        if self.base_url.is_empty() {
            return Err(WinxError::ConfigurationError {
                message: Arc::new(ERR_BASE_URL_EMPTY.to_string()),
            });
        }

        if self.timeout_seconds == 0 {
            return Err(WinxError::ConfigurationError {
                message: Arc::new(ERR_TIMEOUT_ZERO.to_string()),
            });
        }

        if self.rate_limit_rpm == 0 {
            return Err(WinxError::ConfigurationError {
                message: Arc::new(ERR_RATE_LIMIT_ZERO.to_string()),
            });
        }

        Ok(())
    }

    /// Get the chat completions endpoint URL
    pub fn chat_completions_url(&self) -> Cow<'_, str> {
        if self.base_url == "https://integrate.api.nvidia.com" {
            Cow::Borrowed(&DEFAULT_NVIDIA_CHAT_COMPLETIONS_URL)
        } else {
            Cow::Owned(format!(
                "{}/v1/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
        }
    }
}
