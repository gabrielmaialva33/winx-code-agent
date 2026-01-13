//! Chat Engine
//!
//! Sistema de chat interativo com suporte a múltiplos providers LLM,
//! streaming, histórico e integração com sistema de aprendizado.

mod config;
mod history;
mod session;
mod streaming;

pub use config::{ChatConfig, load_config, save_config};
pub use history::{parse_chat_markdown, serialize_chat_markdown, ChatHistory};
pub use session::{ChatSession, SessionMeta};
pub use streaming::{print_stream, StreamPrinter};

use std::sync::Arc;

use crate::errors::WinxError;
use crate::providers::{
    ChatOptions, ChatResponse, EventStream, Message, Provider, ProviderRegistry,
};

/// Estado global do chat
pub struct ChatEngine {
    /// Registry de providers
    pub registry: ProviderRegistry,
    /// Configuração
    pub config: ChatConfig,
    /// Sessão atual
    pub session: Option<ChatSession>,
}

impl ChatEngine {
    /// Cria novo engine com configuração
    pub fn new(config: ChatConfig) -> Self {
        let registry = crate::providers::create_registry(&config.providers);

        Self {
            registry,
            config,
            session: None,
        }
    }

    /// Cria engine a partir de config file
    pub fn from_config_file() -> Result<Self, WinxError> {
        let config = load_config()?;
        Ok(Self::new(config))
    }

    /// Inicia nova sessão
    pub fn new_session(&mut self) -> Result<&mut ChatSession, WinxError> {
        let provider = self.registry.default().ok_or_else(|| {
            WinxError::ConfigurationError("No provider available".to_string())
        })?;

        let model = self.config.default_model.clone()
            .unwrap_or_else(|| provider.default_model().to_string());

        let mut session = ChatSession::new(provider, model);

        // Apply config settings to session
        if let Some(ref system_prompt) = self.config.system_prompt {
            session.set_system_prompt(system_prompt);
        }
        if let Some(temp) = self.config.temperature {
            session.set_temperature(temp);
        }
        if let Some(max_tokens) = self.config.max_tokens {
            session.set_max_tokens(max_tokens);
        }

        self.session = Some(session);
        self.session.as_mut().ok_or_else(|| {
            WinxError::ConfigurationError("Failed to create session".to_string())
        })
    }

    /// Obtém sessão atual ou cria nova
    pub fn current_session(&mut self) -> Result<&mut ChatSession, WinxError> {
        if self.session.is_none() {
            self.new_session()?;
        }
        self.session.as_mut().ok_or_else(|| {
            WinxError::ConfigurationError("No session available".to_string())
        })
    }

    /// Envia mensagem one-shot (sem sessão persistente)
    pub async fn one_shot(&self, message: &str) -> Result<ChatResponse, WinxError> {
        let provider = self.registry.default().ok_or_else(|| {
            WinxError::ConfigurationError("No provider available".to_string())
        })?;

        let model = self.config.default_model.clone()
            .unwrap_or_else(|| provider.default_model().to_string());

        let messages = vec![Message::user(message)];

        let options = ChatOptions {
            model: Some(model),
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            system: self.config.system_prompt.clone(),
            ..Default::default()
        };

        provider.chat(&messages, &options).await.map_err(|e| {
            WinxError::ApiError(e.to_string())
        })
    }

    /// Envia mensagem one-shot com streaming
    pub async fn one_shot_stream(&self, message: &str) -> Result<EventStream, WinxError> {
        let provider = self.registry.default().ok_or_else(|| {
            WinxError::ConfigurationError("No provider available".to_string())
        })?;

        let model = self.config.default_model.clone()
            .unwrap_or_else(|| provider.default_model().to_string());

        let messages = vec![Message::user(message)];

        let options = ChatOptions {
            model: Some(model),
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            system: self.config.system_prompt.clone(),
            stream: true,
            ..Default::default()
        };

        provider.chat_stream(&messages, &options).await.map_err(|e| {
            WinxError::ApiError(e.to_string())
        })
    }

    /// Lista providers disponíveis
    pub fn list_providers(&self) -> Vec<&str> {
        self.registry.list()
    }

    /// Lista modelos disponíveis
    pub fn list_models(&self) -> Vec<(String, String)> {
        self.registry.all_models()
            .into_iter()
            .map(|(id, info)| (id, info.description))
            .collect()
    }

    /// Troca provider ativo
    pub fn set_provider(&mut self, name: &str) -> Result<(), WinxError> {
        if self.registry.get(name).is_none() {
            return Err(WinxError::ConfigurationError(format!("Provider '{}' not found", name)));
        }
        self.config.default_provider = Some(name.to_string());
        Ok(())
    }

    /// Troca modelo ativo
    pub fn set_model(&mut self, model: &str) {
        self.config.default_model = Some(model.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_creation() {
        let config = ChatConfig::default();
        let engine = ChatEngine::new(config);

        // Deve ter pelo menos Ollama disponível
        assert!(!engine.list_providers().is_empty());
    }
}
