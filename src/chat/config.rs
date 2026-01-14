//! Chat Configuration
//!
//! Gerencia configuração persistente do chat.

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::errors::WinxError;
use crate::providers::ProvidersConfig;

/// Configuração do chat
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatConfig {
    /// Provider padrão
    pub default_provider: Option<String>,

    /// Modelo padrão
    pub default_model: Option<String>,

    /// Temperatura (0.0 - 2.0)
    pub temperature: Option<f32>,

    /// Max tokens
    pub max_tokens: Option<usize>,

    /// System prompt padrão
    pub system_prompt: Option<String>,

    /// Configuração dos providers
    #[serde(default)]
    pub providers: ProvidersConfig,

    /// Diretório para histórico
    pub history_dir: Option<PathBuf>,

    /// Tema de cores (light/dark)
    pub theme: Option<String>,

    /// Syntax highlighting habilitado
    #[serde(default = "default_true")]
    pub syntax_highlighting: bool,

    /// Stream output
    #[serde(default = "default_true")]
    pub stream: bool,
}

fn default_true() -> bool {
    true
}

/// Default system prompt for Winx
const DEFAULT_SYSTEM_PROMPT: &str = r"You are Winx, a high-performance AI assistant running locally on the user's computer.

Key facts:
- You are running as `winx-code-agent`, a Rust-based CLI chat application
- You are NOT a cloud-only service - you run locally via the user's terminal
- You communicate with LLM providers (NVIDIA NIM, OpenAI, Ollama) to process requests
- The user can see what model is being used in their prompt

Be helpful, concise, and direct. Prefer code examples when relevant.
The user may speak Portuguese (Brazilian) or English - respond in the same language they use.";

impl ChatConfig {
    /// Cria config com valores default sensíveis
    pub fn sensible_defaults() -> Self {
        Self {
            temperature: Some(0.7),
            max_tokens: Some(4096),
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.to_string()),
            syntax_highlighting: true,
            stream: true,
            ..Default::default()
        }
    }

    /// Diretório de configuração
    pub fn config_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "winx", "winx-chat")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    /// Diretório de dados (histórico, etc)
    pub fn data_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "winx", "winx-chat")
            .map(|dirs| dirs.data_dir().to_path_buf())
    }

    /// Caminho do arquivo de config
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|dir| dir.join("config.toml"))
    }

    /// Diretório de histórico
    pub fn history_path(&self) -> PathBuf {
        self.history_dir
            .clone()
            .or_else(|| Self::data_dir().map(|d| d.join("history")))
            .unwrap_or_else(|| PathBuf::from("~/.winx/chat/history"))
    }
}

/// Carrega configuração do arquivo
pub fn load_config() -> Result<ChatConfig, WinxError> {
    let path = ChatConfig::config_path()
        .ok_or_else(|| WinxError::ConfigurationError("Could not determine config path".to_string()))?;

    if !path.exists() {
        // Retorna config com defaults
        return Ok(ChatConfig::sensible_defaults());
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| WinxError::ConfigurationError(format!("Failed to read config: {e}")))?;

    // Tenta TOML primeiro, depois JSON
    if path.extension().is_some_and(|e| e == "toml") {
        toml::from_str(&content)
            .map_err(|e| WinxError::ConfigurationError(format!("Invalid TOML config: {e}")))
    } else {
        serde_json::from_str(&content)
            .map_err(|e| WinxError::ConfigurationError(format!("Invalid JSON config: {e}")))
    }
}

/// Salva configuração no arquivo
pub fn save_config(config: &ChatConfig) -> Result<(), WinxError> {
    let path = ChatConfig::config_path()
        .ok_or_else(|| WinxError::ConfigurationError("Could not determine config path".to_string()))?;

    // Cria diretório se não existe
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| WinxError::ConfigurationError(format!("Failed to create config dir: {e}")))?;
    }

    let content = if path.extension().is_some_and(|e| e == "toml") {
        toml::to_string_pretty(config)
            .map_err(|e| WinxError::ConfigurationError(format!("Failed to serialize config: {e}")))?
    } else {
        serde_json::to_string_pretty(config)
            .map_err(|e| WinxError::ConfigurationError(format!("Failed to serialize config: {e}")))?
    };

    fs::write(&path, content)
        .map_err(|e| WinxError::ConfigurationError(format!("Failed to write config: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ChatConfig::sensible_defaults();
        assert!(config.default_provider.is_none());
        assert!(config.syntax_highlighting);
        assert!(config.stream);
    }

    #[test]
    fn test_sensible_defaults() {
        let config = ChatConfig::sensible_defaults();
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.max_tokens, Some(4096));
    }
}
