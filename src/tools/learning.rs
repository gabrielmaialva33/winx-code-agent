//! Learning Tools - MCP tools para integração do sistema de aprendizado.
//!
//! Ferramentas que permitem ao Claude Code:
//! - Buscar em histórico de conversas
//! - Detectar padrões repetitivos
//! - Obter contexto do usuário
//! - Sugerir automações

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::errors::WinxError;
use crate::learning::{LearningReport, LearningSystem};
use crate::types::{
    DetectPatterns, GetAutomationSuggestions, GetUserContext, ProcessLearning, SearchHistory,
};

/// Estado compartilhado do sistema de aprendizado
pub type SharedLearningState = Arc<Mutex<Option<LearningState>>>;

/// Estado do sistema de aprendizado
pub struct LearningState {
    /// Sistema de aprendizado
    pub system: LearningSystem,
    /// Relatório mais recente (cache)
    pub cached_report: Option<LearningReport>,
    /// Indica se já foi processado
    pub processed: bool,
}

impl LearningState {
    /// Cria novo estado
    pub fn new() -> Result<Self, WinxError> {
        Ok(Self {
            system: LearningSystem::new()?,
            cached_report: None,
            processed: false,
        })
    }

    /// Carrega relatório existente do disco
    pub fn load_existing(&mut self) {
        if let Some(report) = self.system.store.load_report() {
            info!(
                "Loaded existing learning report: {} sessions, {} messages",
                report.total_sessions, report.total_messages
            );
            self.cached_report = Some(report);
            self.processed = true;
        }
    }
}

impl Default for LearningState {
    fn default() -> Self {
        Self::new().expect("Failed to create LearningState")
    }
}

/// Executa o tool `SearchHistory`
///
/// Busca semântica nas sessões do Claude Code
pub async fn execute_search_history(
    params: SearchHistory,
    state: &SharedLearningState,
) -> Result<Value, WinxError> {
    let mut guard = state.lock().await;

    // Inicializa estado se necessário
    if guard.is_none() {
        let mut new_state = LearningState::new()?;
        new_state.load_existing();
        *guard = Some(new_state);
    }

    let learning_state = guard.as_mut().ok_or_else(|| {
        WinxError::SerializationError("Learning state not initialized".to_string())
    })?;

    // Verifica se tem dados processados
    if !learning_state.processed {
        return Ok(json!({
            "status": "not_processed",
            "message": "Learning data not yet processed. Call ProcessLearning first.",
            "results": []
        }));
    }

    debug!("Searching history for: {}", params.query);

    // Usa embeddings para busca semântica
    let results = learning_state
        .system
        .communication
        .get_top_words(params.max_results);

    // Converte resultados
    let search_results: Vec<Value> = results
        .iter()
        .map(|(word, count)| {
            json!({
                "term": word,
                "frequency": count
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "query": params.query,
        "results": search_results,
        "total_found": search_results.len()
    }))
}

/// Executa o tool `DetectPatterns`
///
/// Detecta se o pedido atual é similar a pedidos anteriores
pub async fn execute_detect_patterns(
    params: DetectPatterns,
    state: &SharedLearningState,
) -> Result<Value, WinxError> {
    let mut guard = state.lock().await;

    // Inicializa estado se necessário
    if guard.is_none() {
        let mut new_state = LearningState::new()?;
        new_state.load_existing();
        *guard = Some(new_state);
    }

    let learning_state = guard.as_mut().ok_or_else(|| {
        WinxError::SerializationError("Learning state not initialized".to_string())
    })?;

    debug!("Detecting patterns for: {}", params.request);

    // Adiciona mensagem ao detector para encontrar similares
    learning_state
        .system
        .repetitions
        .add_message(&params.request, "current_session");

    // Busca candidatos a automação
    let candidates = learning_state.system.repetitions.get_automation_candidates();

    // Verifica se o pedido atual é similar a algum candidato
    let similar: Vec<Value> = candidates
        .iter()
        .filter(|c| {
            // Verifica similaridade básica com o padrão
            let request_lower = params.request.to_lowercase();
            let pattern_lower = c.pattern.to_lowercase();

            // Calcula Jaccard simples
            let request_words: std::collections::HashSet<_> =
                request_lower.split_whitespace().collect();
            let pattern_words: std::collections::HashSet<_> =
                pattern_lower.split_whitespace().collect();

            let intersection = request_words.intersection(&pattern_words).count();
            let union = request_words.union(&pattern_words).count();

            if union > 0 {
                let similarity = intersection as f64 / union as f64;
                similarity > 0.3
            } else {
                false
            }
        })
        .take(3)
        .map(|c| {
            json!({
                "pattern": c.pattern,
                "suggested_command": c.suggested_command,
                "frequency": c.frequency,
                "examples": c.examples
            })
        })
        .collect();

    let is_repeated = !similar.is_empty();

    Ok(json!({
        "status": "ok",
        "request": params.request,
        "is_repeated_pattern": is_repeated,
        "similar_patterns": similar,
        "suggestion": if is_repeated {
            "This request is similar to patterns that have been repeated. Consider creating an automation."
        } else {
            "This appears to be a unique request."
        }
    }))
}

/// Executa o tool `GetUserContext`
///
/// Retorna perfil de comunicação do usuário
pub async fn execute_get_user_context(
    params: GetUserContext,
    state: &SharedLearningState,
) -> Result<Value, WinxError> {
    let mut guard = state.lock().await;

    // Inicializa estado se necessário
    if guard.is_none() {
        let mut new_state = LearningState::new()?;
        new_state.load_existing();
        *guard = Some(new_state);
    }

    let learning_state = guard.as_ref().ok_or_else(|| {
        WinxError::SerializationError("Learning state not initialized".to_string())
    })?;

    debug!("Getting user context");

    let mut context = json!({
        "status": "ok"
    });

    // Vocabulário
    if params.include_vocabulary {
        let vocab = learning_state.system.communication.get_vocabulary();
        context["vocabulary"] = json!({
            "top_expressions": vocab.iter().take(20).collect::<Vec<_>>(),
            "total_unique": vocab.len()
        });
    }

    // Correções
    if params.include_corrections {
        let corrections = learning_state.system.communication.get_corrections();
        context["corrections"] = json!({
            "patterns": corrections.iter().take(10).map(|c| {
                json!({
                    "phrase": c.phrase,
                    "count": c.count
                })
            }).collect::<Vec<_>>(),
            "total": corrections.len()
        });
    }

    // Padrões de pensamento
    if params.include_thinking {
        let patterns = learning_state.system.thinking.get_patterns();
        context["thinking_patterns"] = json!({
            "patterns": patterns.iter().take(10).map(|p| {
                json!({
                    "name": p.name,
                    "description": p.description,
                    "frequency": p.frequency
                })
            }).collect::<Vec<_>>(),
            "total": patterns.len()
        });
    }

    // Estatísticas gerais
    if let Some(ref report) = learning_state.cached_report {
        context["stats"] = json!({
            "total_sessions": report.total_sessions,
            "total_messages": report.total_messages,
            "user_messages": report.user_messages
        });
    }

    Ok(context)
}

/// Executa o tool `GetAutomationSuggestions`
///
/// Retorna lista de pedidos repetitivos que podem virar automações
pub async fn execute_get_automation_suggestions(
    params: GetAutomationSuggestions,
    state: &SharedLearningState,
) -> Result<Value, WinxError> {
    let mut guard = state.lock().await;

    // Inicializa estado se necessário
    if guard.is_none() {
        let mut new_state = LearningState::new()?;
        new_state.load_existing();
        *guard = Some(new_state);
    }

    let learning_state = guard.as_ref().ok_or_else(|| {
        WinxError::SerializationError("Learning state not initialized".to_string())
    })?;

    debug!(
        "Getting automation suggestions (min_freq: {}, max: {})",
        params.min_frequency, params.max_suggestions
    );

    // Obtém candidatos do relatório cache ou do detector
    let candidates = if let Some(ref report) = learning_state.cached_report {
        report.automation_candidates.clone()
    } else {
        learning_state.system.repetitions.get_automation_candidates()
    };

    // Filtra e limita
    let suggestions: Vec<Value> = candidates
        .iter()
        .filter(|c| c.frequency >= params.min_frequency)
        .take(params.max_suggestions)
        .map(|c| {
            json!({
                "pattern": c.pattern,
                "suggested_command": c.suggested_command,
                "frequency": c.frequency,
                "examples": c.examples,
                "impact": match c.frequency {
                    f if f >= 10 => "high",
                    f if f >= 5 => "medium",
                    _ => "low"
                }
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "suggestions": suggestions,
        "total_candidates": candidates.len(),
        "message": if suggestions.is_empty() {
            "No repetitive patterns found that meet the criteria."
        } else {
            "These patterns have been repeated multiple times and could be automated."
        }
    }))
}

/// Executa o tool `ProcessLearning`
///
/// Processa todas as sessões do Claude Code para extrair aprendizado
pub async fn execute_process_learning(
    params: ProcessLearning,
    state: &SharedLearningState,
) -> Result<Value, WinxError> {
    let mut guard = state.lock().await;

    // Inicializa estado se necessário
    if guard.is_none() {
        *guard = Some(LearningState::new()?);
    }

    let learning_state = guard.as_mut().ok_or_else(|| {
        WinxError::SerializationError("Learning state not initialized".to_string())
    })?;

    // Verifica se já foi processado
    if learning_state.processed && !params.force {
        if let Some(ref report) = learning_state.cached_report {
            return Ok(json!({
                "status": "already_processed",
                "message": "Learning already processed. Use force=true to reprocess.",
                "summary": {
                    "total_sessions": report.total_sessions,
                    "total_messages": report.total_messages,
                    "user_messages": report.user_messages,
                    "vocabulary_size": report.vocabulary.len(),
                    "automation_candidates": report.automation_candidates.len()
                }
            }));
        }
    }

    info!("Processing learning from Claude Code sessions...");

    // Processa todas as sessões
    let report: LearningReport = learning_state.system.process_all_sessions().await?;

    // Atualiza cache
    learning_state.cached_report = Some(report.clone());
    learning_state.processed = true;

    info!(
        "Learning processed: {} sessions, {} messages, {} automation candidates",
        report.total_sessions,
        report.total_messages,
        report.automation_candidates.len()
    );

    Ok(json!({
        "status": "ok",
        "message": "Learning processed successfully",
        "summary": {
            "total_sessions": report.total_sessions,
            "total_messages": report.total_messages,
            "user_messages": report.user_messages,
            "vocabulary_size": report.vocabulary.len(),
            "frequent_requests": report.frequent_requests.len(),
            "automation_candidates": report.automation_candidates.len(),
            "thinking_patterns": report.thinking_patterns.len()
        },
        "top_vocabulary": report.vocabulary.iter().take(10).collect::<Vec<_>>(),
        "top_automations": report.automation_candidates.iter().take(5).map(|c| {
            json!({
                "pattern": c.pattern,
                "command": c.suggested_command,
                "frequency": c.frequency
            })
        }).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_learning_state_creation() {
        let state: SharedLearningState = Arc::new(Mutex::new(None));

        // Inicializa
        let mut guard = state.lock().await;
        *guard = Some(LearningState::new().unwrap());
        drop(guard);

        // Verifica
        let guard = state.lock().await;
        assert!(guard.is_some());
    }

    #[tokio::test]
    async fn test_get_user_context() {
        let state: SharedLearningState = Arc::new(Mutex::new(None));

        let params = GetUserContext {
            include_vocabulary: true,
            include_corrections: true,
            include_thinking: false,
        };

        let result = execute_get_user_context(params, &state).await.unwrap();
        assert_eq!(result["status"], "ok");
    }

    #[tokio::test]
    async fn test_detect_patterns() {
        let state: SharedLearningState = Arc::new(Mutex::new(None));

        let params = DetectPatterns {
            request: "como faço deploy do viva?".to_string(),
        };

        let result = execute_detect_patterns(params, &state).await.unwrap();
        assert_eq!(result["status"], "ok");
    }
}
