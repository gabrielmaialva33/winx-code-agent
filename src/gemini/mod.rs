//! Google Gemini AI integration module
//!
//! This module provides integration with Google Gemini models as a fallback
//! for NVIDIA AI when the primary service is unavailable.

pub mod client;
pub mod config;
pub mod models;

pub use client::GeminiClient;
pub use config::GeminiConfig;
pub use models::*;