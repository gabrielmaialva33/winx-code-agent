//! Thinking Patterns - Analisa padrões de pensamento.
//!
//! Detecta:
//! - Sequências típicas de investigação
//! - Atalhos mentais que o usuário usa
//! - O que ele tende a esquecer

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{SessionMessage, ThinkingPattern};

/// Analisador de padrões de pensamento
#[derive(Debug, Default)]
pub struct ThinkingPatterns {
    /// Sequências de ações detectadas
    action_sequences: HashMap<String, SequenceData>,
    /// Contextos esquecidos (repetidos após longo tempo)
    forgotten_contexts: Vec<ForgottenContext>,
    /// Atalhos mentais
    shortcuts: HashMap<String, usize>,
}

/// Dados de uma sequência de ações
#[derive(Debug, Clone, Default)]
struct SequenceData {
    /// Sequência de ações
    actions: Vec<String>,
    /// Frequência
    count: usize,
}

/// Contexto que o usuário esquece
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgottenContext {
    /// O que foi esquecido
    pub context: String,
    /// Quantas vezes foi re-perguntado
    pub times_asked: usize,
}

impl ThinkingPatterns {
    /// Cria novo analisador
    pub fn new() -> Self {
        Self::default()
    }

    /// Analisa sequências de mensagens
    pub fn analyze_sequences(&mut self, messages: &[SessionMessage]) {
        // Agrupa mensagens por sessão
        let mut sessions: HashMap<String, Vec<&SessionMessage>> = HashMap::new();
        for msg in messages {
            sessions.entry(msg.session_id.clone())
                .or_default()
                .push(msg);
        }

        // Analisa cada sessão
        for (_session_id, session_messages) in sessions {
            self.analyze_session_sequence(&session_messages);
        }

        // Detecta padrões de esquecimento
        self.detect_forgotten_patterns(messages);
    }

    /// Analisa sequência de uma sessão
    fn analyze_session_sequence(&mut self, messages: &[&SessionMessage]) {
        // Extrai ações do usuário
        let user_actions: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| extract_action(&m.content))
            .collect();

        // Detecta sequências de 2-3 ações
        for window_size in 2..=3 {
            for window in user_actions.windows(window_size) {
                let key = window.join(" -> ");
                self.action_sequences
                    .entry(key)
                    .or_default()
                    .count += 1;
            }
        }

        // Detecta atalhos mentais (comandos curtos)
        for msg in messages {
            if msg.role == "user" {
                let shortcuts = detect_shortcuts(&msg.content);
                for shortcut in shortcuts {
                    *self.shortcuts.entry(shortcut).or_insert(0) += 1;
                }
            }
        }
    }

    /// Detecta padrões de esquecimento
    fn detect_forgotten_patterns(&mut self, messages: &[SessionMessage]) {
        // Agrupa perguntas similares que aparecem em sessões diferentes
        let mut question_counts: HashMap<String, Vec<String>> = HashMap::new();

        for msg in messages.iter().filter(|m| m.role == "user") {
            if is_question(&msg.content) {
                let normalized = normalize_question(&msg.content);
                question_counts
                    .entry(normalized)
                    .or_default()
                    .push(msg.session_id.clone());
            }
        }

        // Perguntas em múltiplas sessões = contexto esquecido
        for (question, sessions) in question_counts {
            let unique_sessions: std::collections::HashSet<_> = sessions.iter().collect();
            if unique_sessions.len() >= 2 {
                self.forgotten_contexts.push(ForgottenContext {
                    context: question,
                    times_asked: unique_sessions.len(),
                });
            }
        }
    }

    /// Retorna padrões detectados
    pub fn get_patterns(&self) -> Vec<ThinkingPattern> {
        let mut patterns = Vec::new();

        // Sequências frequentes
        let mut sequences: Vec<_> = self.action_sequences.iter().collect();
        sequences.sort_by(|a, b| b.1.count.cmp(&a.1.count));

        for (seq, data) in sequences.iter().take(10) {
            if data.count >= 2 {
                patterns.push(ThinkingPattern {
                    name: format!("Sequence: {}", seq),
                    description: format!("Aparece {} vezes", data.count),
                    frequency: data.count,
                });
            }
        }

        // Atalhos mais usados
        let mut shortcuts: Vec<_> = self.shortcuts.iter().collect();
        shortcuts.sort_by(|a, b| b.1.cmp(a.1));

        for (shortcut, count) in shortcuts.iter().take(10) {
            if **count >= 3 {
                patterns.push(ThinkingPattern {
                    name: format!("Shortcut: {}", shortcut),
                    description: "Atalho mental frequente".to_string(),
                    frequency: **count,
                });
            }
        }

        // Contextos esquecidos
        for ctx in &self.forgotten_contexts {
            if ctx.times_asked >= 2 {
                patterns.push(ThinkingPattern {
                    name: "Forgotten context".to_string(),
                    description: ctx.context.clone(),
                    frequency: ctx.times_asked,
                });
            }
        }

        patterns.sort_by(|a, b| b.frequency.cmp(&a.frequency));
        patterns
    }

    /// Retorna contextos esquecidos
    pub fn get_forgotten_contexts(&self) -> Vec<ForgottenContext> {
        let mut contexts = self.forgotten_contexts.clone();
        contexts.sort_by(|a, b| b.times_asked.cmp(&a.times_asked));
        contexts
    }
}

/// Extrai ação de uma mensagem
fn extract_action(content: &str) -> String {
    let lower = content.to_lowercase();

    // Detecta tipo de ação
    if lower.contains("erro") || lower.contains("error") || lower.contains("falha") {
        return "debug".to_string();
    }
    if lower.contains("como") || lower.contains("what") || lower.contains("?") {
        return "question".to_string();
    }
    if lower.contains("faz") || lower.contains("cria") || lower.contains("adiciona") {
        return "create".to_string();
    }
    if lower.contains("roda") || lower.contains("executa") || lower.contains("run") {
        return "execute".to_string();
    }
    if lower.contains("mostra") || lower.contains("lista") || lower.contains("show") {
        return "inspect".to_string();
    }
    if lower.contains("testa") || lower.contains("test") {
        return "test".to_string();
    }

    "other".to_string()
}

/// Detecta atalhos mentais
fn detect_shortcuts(content: &str) -> Vec<String> {
    let mut shortcuts = Vec::new();
    let lower = content.to_lowercase();

    // Comandos curtos
    if lower.len() < 50 {
        // Expressões de atalho
        if lower.starts_with("faz") || lower.starts_with("roda") {
            shortcuts.push("imperative-short".to_string());
        }
        if lower.contains("de novo") || lower.contains("again") {
            shortcuts.push("repeat-request".to_string());
        }
        if lower.contains("igual") || lower.contains("como antes") {
            shortcuts.push("reference-previous".to_string());
        }
    }

    // Referências implícitas
    if lower.contains("isso") || lower.contains("aquilo") || lower.contains("isso ai") {
        shortcuts.push("implicit-reference".to_string());
    }

    shortcuts
}

/// Verifica se é pergunta
fn is_question(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("?")
        || lower.starts_with("como")
        || lower.starts_with("onde")
        || lower.starts_with("o que")
        || lower.starts_with("qual")
        || lower.starts_with("what")
        || lower.starts_with("how")
        || lower.starts_with("where")
}

/// Normaliza pergunta para comparação
fn normalize_question(content: &str) -> String {
    content
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .take(10) // Primeiras 10 palavras
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_action() {
        assert_eq!(extract_action("como faço isso?"), "question");
        assert_eq!(extract_action("roda os testes"), "execute");
        assert_eq!(extract_action("deu erro aqui"), "debug");
        assert_eq!(extract_action("cria um arquivo"), "create");
    }

    #[test]
    fn test_is_question() {
        assert!(is_question("como faço deploy?"));
        assert!(is_question("onde fica o arquivo?"));
        assert!(!is_question("roda os testes"));
    }

    #[test]
    fn test_detect_shortcuts() {
        let shortcuts = detect_shortcuts("faz de novo");
        assert!(shortcuts.contains(&"imperative-short".to_string()));
        assert!(shortcuts.contains(&"repeat-request".to_string()));
    }
}
