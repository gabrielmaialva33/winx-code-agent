//! DashScope AI integration module
//!
//! This module provides integration with Alibaba Cloud's DashScope platform,
//! specifically using Qwen3-Coder-Plus model as the primary AI provider.

pub mod client;
pub mod config;
pub mod models;

pub use client::DashScopeClient;
pub use config::DashScopeConfig;
pub use models::*;
