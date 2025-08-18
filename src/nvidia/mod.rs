//! NVIDIA API integration module for Winx
//!
//! This module provides integration with NVIDIA's NIM (NVIDIA Inference Microservices) API,
//! enabling advanced AI capabilities for code analysis, generation, and assistance.

pub mod client;
pub mod config;
pub mod models;
pub mod tools;

#[cfg(test)]
pub mod tests;

pub use client::NvidiaClient;
pub use config::NvidiaConfig;
pub use models::*;

use crate::errors::{Result, WinxError};

/// Initialize NVIDIA integration with configuration
pub async fn initialize(config: NvidiaConfig) -> Result<NvidiaClient> {
    if config.api_key.is_empty() {
        return Err(WinxError::ConfigurationError(
            "NVIDIA API key is required".to_string(),
        ));
    }

    let client = NvidiaClient::new(config).await?;

    // Test connectivity
    client.validate_connection().await?;

    Ok(client)
}

/// Get default NVIDIA configuration from environment
pub fn default_config() -> Result<NvidiaConfig> {
    NvidiaConfig::from_env()
}
