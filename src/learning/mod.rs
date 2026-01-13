//! Learning module - Aprende padrões de comunicação do usuário.
//!
//! Este módulo lê as sessões do Claude Code e extrai:
//! - Padrões de comunicação (vocabulário, estilo)
//! - Pedidos repetidos (candidatos a automação)
//! - Padrões de pensamento (atalhos mentais)
//!
//! Foco em COMO o usuário pensa e se comunica, não em código.

pub mod communication;
pub mod embedding_engine;
pub mod embeddings;
pub mod repetitions;
pub mod session_parser;
pub mod store;
pub mod thinking;

use std::path::PathBuf;

use directories::BaseDirs;

use crate::errors::WinxError;

pub use communication::CommunicationLearner;
pub use embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine, EmbeddingStats};
pub use repetitions::RepetitionDetector;
pub use session_parser::{SessionMessage, SessionParser};
pub use store::LearningStore;
pub use thinking::ThinkingPatterns;

/// Diretório padrão para armazenar aprendizado
pub fn default_learning_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".winx")
        .join("learning")
}

/// Diretório das sessões do Claude Code
pub fn claude_sessions_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// Sistema de aprendizado completo
pub struct LearningSystem {
    /// Parser de sessões
    pub parser: SessionParser,
    /// Aprendizado de comunicação
    pub communication: CommunicationLearner,
    /// Detector de repetições
    pub repetitions: RepetitionDetector,
    /// Padrões de pensamento
    pub thinking: ThinkingPatterns,
    /// Storage persistente
    pub store: LearningStore,
}

impl LearningSystem {
    /// Cria novo sistema de aprendizado
    pub fn new() -> Result<Self, WinxError> {
        let sessions_dir = claude_sessions_dir();
        let learning_dir = default_learning_dir();

        Ok(Self {
            parser: SessionParser::new(sessions_dir),
            communication: CommunicationLearner::new(),
            repetitions: RepetitionDetector::new(),
            thinking: ThinkingPatterns::new(),
            store: LearningStore::new(learning_dir)?,
        })
    }

    /// Processa todas as sessões e extrai aprendizado
    pub async fn process_all_sessions(&mut self) -> Result<LearningReport, WinxError> {
        // 1. Parse todas as sessões
        let messages = self.parser.parse_all_sessions().await?;

        // 2. Extrai apenas mensagens do usuário
        let user_messages: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "user")
            .collect();

        // 3. Analisa comunicação
        for msg in &user_messages {
            self.communication.analyze(&msg.content);
        }

        // 4. Detecta repetições
        for msg in &user_messages {
            self.repetitions.add_message(&msg.content, &msg.session_id);
        }

        // 5. Analisa padrões de pensamento
        self.thinking.analyze_sequences(&messages);

        // 6. Gera relatório
        let report = LearningReport {
            total_sessions: self.parser.session_count(),
            total_messages: messages.len(),
            user_messages: user_messages.len(),
            vocabulary: self.communication.get_vocabulary(),
            frequent_requests: self.repetitions.get_frequent_requests(2),
            automation_candidates: self.repetitions.get_automation_candidates(),
            thinking_patterns: self.thinking.get_patterns(),
        };

        // 7. Persiste aprendizado
        self.store.save_report(&report)?;

        Ok(report)
    }
}

impl Default for LearningSystem {
    fn default() -> Self {
        Self::new().expect("Failed to create LearningSystem")
    }
}

/// Relatório de aprendizado
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearningReport {
    /// Total de sessões processadas
    pub total_sessions: usize,
    /// Total de mensagens
    pub total_messages: usize,
    /// Mensagens do usuário
    pub user_messages: usize,
    /// Vocabulário aprendido
    pub vocabulary: Vec<(String, usize)>,
    /// Pedidos frequentes
    pub frequent_requests: Vec<FrequentRequest>,
    /// Candidatos a automação
    pub automation_candidates: Vec<AutomationCandidate>,
    /// Padrões de pensamento
    pub thinking_patterns: Vec<ThinkingPattern>,
}

/// Pedido frequente detectado
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrequentRequest {
    /// Texto do pedido (normalizado)
    pub text: String,
    /// Quantas vezes apareceu
    pub count: usize,
    /// Em quais sessões
    pub sessions: Vec<String>,
}

/// Candidato a automação
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationCandidate {
    /// Descrição do padrão
    pub pattern: String,
    /// Sugestão de comando/skill
    pub suggested_command: String,
    /// Frequência
    pub frequency: usize,
    /// Exemplos de uso
    pub examples: Vec<String>,
}

/// Padrão de pensamento detectado
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThinkingPattern {
    /// Nome do padrão
    pub name: String,
    /// Descrição
    pub description: String,
    /// Frequência
    pub frequency: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_dirs() {
        let learning_dir = default_learning_dir();
        assert!(learning_dir.to_string_lossy().contains(".winx/learning"));

        let sessions_dir = claude_sessions_dir();
        assert!(sessions_dir.to_string_lossy().contains(".claude/projects"));
    }
}
