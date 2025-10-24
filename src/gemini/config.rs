//! Google Gemini configuration

use crate::errors::{Result, WinxError};
use std::env;

const DEFAULT_MODEL: &str = "gemini-2.5-pro";
const DEFAULT_FALLBACK_MODEL: &str = "gemini-2.5-flash";
const DEFAULT_TIMEOUT: &str = "30";
const DEFAULT_MAX_RETRIES: &str = "3";
const DEFAULT_RATE_LIMIT: &str = "60";
const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

const ERR_API_KEY_NOT_SET: &str = "GEMINI_API_KEY not set";
const ERR_INVALID_TIMEOUT: &str = "Invalid GEMINI_TIMEOUT_SECONDS";
const ERR_INVALID_MAX_RETRIES: &str = "Invalid GEMINI_MAX_RETRIES";
const ERR_INVALID_RATE_LIMIT: &str = "Invalid GEMINI_RATE_LIMIT_RPM";
const ERR_UNKNOWN_MODEL: &str =
    "Unknown Gemini model: {}. Supported models: gemini-2.5-pro, gemini-2.5-flash";
const ERR_API_KEY_EMPTY: &str = "Gemini API key cannot be empty";
const ERR_INVALID_API_KEY_FORMAT: &str = "Invalid Gemini API key format (should start with 'AIza')";
const ERR_TIMEOUT_RANGE: &str = "Timeout must be between 1 and 300 seconds";
const ERR_MAX_RETRIES_EXCEED: &str = "Max retries cannot exceed 10";
const ERR_RATE_LIMIT_RANGE: &str = "Rate limit must be between 1 and 1000 RPM";

lazy_static::lazy_static! {
    /// Cached endpoint for Gemini 2.5 Pro
    static ref GEMINI_25_PRO_ENDPOINT: &str = "models/gemini-2.5-pro:generateContent";
    /// Cached endpoint for Gemini 2.5 Flash
    static ref GEMINI_25_FLASH_ENDPOINT: &str = "models/gemini-2.5-flash:generateContent";
}

/// Available Gemini models
#[derive(Debug, Clone)]
pub enum GeminiModel {
    /// Gemini 2.5 Pro - Most capable model
    Gemini25Pro,
    /// Gemini 2.5 Flash - Faster and more cost-effective
    Gemini25Flash,
}

impl GeminiModel {
    /// Get the model name for API requests
    pub fn model_name(&self) -> &'static str {
        match self {
            GeminiModel::Gemini25Pro => "gemini-2.5-pro",
            GeminiModel::Gemini25Flash => "gemini-2.5-flash",
        }
    }

    /// Get the full endpoint path for the model
    pub fn endpoint(&self) -> &str {
        match self {
            GeminiModel::Gemini25Pro => *GEMINI_25_PRO_ENDPOINT,
            GeminiModel::Gemini25Flash => *GEMINI_25_FLASH_ENDPOINT,
        }
    }
}

impl std::fmt::Display for GeminiModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.model_name())
    }
}

/// Google Gemini configuration
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    /// API key for Google AI
    pub api_key: String,
    /// Primary model to use
    pub model: GeminiModel,
    /// Fallback model for when primary fails
    pub fallback_model: GeminiModel,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
    /// Maximum number of retries
    pub max_retries: u32,
    /// Rate limit in requests per minute
    pub rate_limit_rpm: u32,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: GeminiModel::Gemini25Pro,
            fallback_model: GeminiModel::Gemini25Flash,
            timeout_seconds: 30,
            max_retries: 3,
            rate_limit_rpm: 60,
        }
    }
}

impl GeminiConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("GEMINI_API_KEY")
            .map_err(|_| WinxError::ConfigurationError(ERR_API_KEY_NOT_SET.to_string()))?;

        let model = env::var("GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let model = Self::parse_model(&model)?;

        let fallback_model = env::var("GEMINI_FALLBACK_MODEL")
            .unwrap_or_else(|_| DEFAULT_FALLBACK_MODEL.to_string());
        let fallback_model = Self::parse_model(&fallback_model)?;

        let timeout_seconds = env::var("GEMINI_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| DEFAULT_TIMEOUT.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_TIMEOUT.to_string()))?;

        let max_retries = env::var("GEMINI_MAX_RETRIES")
            .unwrap_or_else(|_| DEFAULT_MAX_RETRIES.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_MAX_RETRIES.to_string()))?;

        let rate_limit_rpm = env::var("GEMINI_RATE_LIMIT_RPM")
            .unwrap_or_else(|_| DEFAULT_RATE_LIMIT.to_string())
            .parse()
            .map_err(|_| WinxError::ConfigurationError(ERR_INVALID_RATE_LIMIT.to_string()))?;

        Ok(Self {
            api_key,
            model,
            fallback_model,
            timeout_seconds,
            max_retries,
            rate_limit_rpm,
        })
    }

    /// Parse model string to GeminiModel enum
    fn parse_model(model_str: &str) -> Result<GeminiModel> {
        match model_str {
            "gemini-2.5-pro" => Ok(GeminiModel::Gemini25Pro),
            "gemini-2.5-flash" => Ok(GeminiModel::Gemini25Flash),
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

        if !self.api_key.starts_with("AIza") {
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

        Ok(())
    }

    /// Get the base URL for Gemini API
    pub fn base_url(&self) -> &'static str {
        BASE_URL
    }

    /// Get the full URL for an endpoint
    pub fn endpoint_url(&self, endpoint: &str) -> String {
        format!("{}/{}?key={}", self.base_url(), endpoint, self.api_key)
    }
}
