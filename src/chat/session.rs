//! Chat Session
//!
//! Gerencia uma sessão de conversa com histórico de mensagens.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::errors::WinxError;
use crate::providers::{
    ChatOptions, ChatResponse, EventStream, FunctionDef, Message, Provider, ToolCall,
};

/// Metadados da sessão
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// ID único da sessão
    pub id: String,

    /// Título da sessão
    pub title: Option<String>,

    /// Modelo usado
    pub model: String,

    /// Provider usado
    pub provider: String,

    /// Data de criação
    pub created_at: DateTime<Utc>,

    /// Data de última atualização
    pub updated_at: DateTime<Utc>,

    /// Tags
    #[serde(default)]
    pub tags: Vec<String>,
}

impl SessionMeta {
    pub fn new(provider: &str, model: &str) -> Self {
        let now = Utc::now();
        Self {
            id: generate_session_id(),
            title: None,
            model: model.to_string(),
            provider: provider.to_string(),
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
        }
    }
}

/// Sessão de chat
pub struct ChatSession {
    /// Metadados
    pub meta: SessionMeta,

    /// Provider ativo
    provider: Arc<dyn Provider>,

    /// Histórico de mensagens
    messages: Vec<Message>,

    /// System prompt
    system_prompt: Option<String>,

    /// Tools disponíveis (MCP ou custom)
    tools: Vec<FunctionDef>,

    /// Temperatura
    temperature: Option<f32>,

    /// Max tokens
    max_tokens: Option<usize>,

    /// Tool calls pendentes
    pending_tool_calls: Vec<ToolCall>,
}

impl ChatSession {
    /// Cria nova sessão
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
        let meta = SessionMeta::new(provider.name(), &model);

        Self {
            meta,
            provider,
            messages: Vec::new(),
            system_prompt: None,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            pending_tool_calls: Vec::new(),
        }
    }

    /// Define system prompt
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt = Some(prompt.to_string());
    }

    /// Adiciona tools
    pub fn add_tools(&mut self, tools: Vec<FunctionDef>) {
        self.tools.extend(tools);
    }

    /// Define temperatura
    pub fn set_temperature(&mut self, temp: f32) {
        self.temperature = Some(temp);
    }

    /// Define max tokens
    pub fn set_max_tokens(&mut self, max: usize) {
        self.max_tokens = Some(max);
    }

    /// Troca provider
    pub fn set_provider(&mut self, provider: Arc<dyn Provider>) {
        self.meta.provider = provider.name().to_string();
        self.provider = provider;
    }

    /// Troca modelo
    pub fn set_model(&mut self, model: &str) {
        self.meta.model = model.to_string();
    }

    /// Retorna nome do provider
    pub fn provider_name(&self) -> &str {
        &self.meta.provider
    }

    /// Retorna nome do modelo
    pub fn model_name(&self) -> &str {
        &self.meta.model
    }

    /// Adiciona mensagem do usuário
    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message::user(content));
        self.meta.updated_at = Utc::now();
    }

    /// Adiciona mensagem do assistente
    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message::assistant(content));
        self.meta.updated_at = Utc::now();
    }

    /// Adiciona resultado de tool
    pub fn add_tool_result(&mut self, tool_id: &str, result: &str) {
        self.messages.push(Message::tool_result(tool_id, result));
        self.meta.updated_at = Utc::now();
    }

    /// Obtém histórico de mensagens
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Limpa histórico
    pub fn clear(&mut self) {
        self.messages.clear();
        self.pending_tool_calls.clear();
    }

    /// Número de mensagens
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Monta opções de chat
    fn build_options(&self, stream: bool) -> ChatOptions {
        ChatOptions {
            model: Some(self.meta.model.clone()),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            tools: if self.tools.is_empty() { None } else { Some(self.tools.clone()) },
            system: self.system_prompt.clone(),
            stream,
        }
    }

    /// Envia mensagem e obtém resposta
    pub async fn send(&mut self, content: &str) -> Result<ChatResponse, WinxError> {
        self.add_user_message(content);

        let options = self.build_options(false);

        let response = self.provider.chat(&self.messages, &options).await
            .map_err(|e| WinxError::ApiError(e.to_string()))?;

        // Adiciona resposta ao histórico
        if !response.content.is_empty() {
            self.add_assistant_message(&response.content);
        }

        // Guarda tool calls pendentes
        if !response.tool_calls.is_empty() {
            self.pending_tool_calls = response.tool_calls.clone();
        }

        Ok(response)
    }

    /// Envia mensagem com streaming
    pub async fn send_stream(&mut self, content: &str) -> Result<EventStream, WinxError> {
        self.add_user_message(content);

        let options = self.build_options(true);

        self.provider.chat_stream(&self.messages, &options).await
            .map_err(|e| WinxError::ApiError(e.to_string()))
    }

    /// Verifica se há tool calls pendentes
    pub fn has_pending_tool_calls(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    /// Obtém tool calls pendentes
    pub fn pending_tool_calls(&self) -> &[ToolCall] {
        &self.pending_tool_calls
    }

    /// Processa resultado de tool call
    pub async fn process_tool_result(
        &mut self,
        tool_id: &str,
        result: &str,
    ) -> Result<ChatResponse, WinxError> {
        // Remove da lista de pendentes
        self.pending_tool_calls.retain(|tc| tc.id != tool_id);

        // Adiciona resultado
        self.add_tool_result(tool_id, result);

        // Se não há mais pendentes, continua a conversa
        if self.pending_tool_calls.is_empty() {
            let options = self.build_options(false);

            let response = self.provider.chat(&self.messages, &options).await
                .map_err(|e| WinxError::ApiError(e.to_string()))?;

            if !response.content.is_empty() {
                self.add_assistant_message(&response.content);
            }

            if !response.tool_calls.is_empty() {
                self.pending_tool_calls = response.tool_calls.clone();
            }

            Ok(response)
        } else {
            // Ainda há tool calls pendentes
            Ok(ChatResponse {
                content: String::new(),
                tool_calls: self.pending_tool_calls.clone(),
                usage: None,
                finish_reason: None,
            })
        }
    }

    /// Exporta sessão como markdown
    pub fn to_markdown(&self) -> String {
        super::history::serialize_chat_markdown(self)
    }
}

/// Gera ID único para sessão
fn generate_session_id() -> String {
    use rand::Rng;

    let mut rng = rand::rng();
    let random: u64 = rng.random();
    format!("sess_{:016x}", random)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::OllamaConfig;

    #[test]
    fn test_session_creation() {
        let config = OllamaConfig::default();
        let provider = Arc::new(crate::providers::OllamaProvider::new(&config));
        let session = ChatSession::new(provider, "llama3.2".to_string());

        assert!(session.meta.id.starts_with("sess_"));
        assert_eq!(session.meta.model, "llama3.2");
        assert!(session.messages.is_empty());
    }

    #[test]
    fn test_message_history() {
        let config = OllamaConfig::default();
        let provider = Arc::new(crate::providers::OllamaProvider::new(&config));
        let mut session = ChatSession::new(provider, "llama3.2".to_string());

        session.add_user_message("Hello");
        session.add_assistant_message("Hi there!");

        assert_eq!(session.message_count(), 2);
        assert_eq!(session.messages()[0].text(), "Hello");
    }

    #[test]
    fn test_session_id_generation() {
        let id1 = generate_session_id();
        let id2 = generate_session_id();

        assert_ne!(id1, id2);
        assert!(id1.starts_with("sess_"));
    }
}
