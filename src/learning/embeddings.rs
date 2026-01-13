//! Conversation Embeddings - Busca semântica em conversas.
//!
//! Usa embeddings reais (jina-embeddings-v2-base-code) para encontrar
//! conversas similares. Roda local na RTX 4090.
//!
//! "Já conversamos sobre isso antes?" → busca semântica real!

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine};
use super::SessionMessage;

/// Sistema de embeddings de conversas com ML real
pub struct ConversationEmbeddings {
    /// Índice de conversas (por tópico normalizado)
    index: HashMap<String, Vec<ConversationEntry>>,
    /// Engine de embeddings (Candle/HTTP/Jaccard)
    engine: Arc<RwLock<Option<EmbeddingEngine>>>,
    /// Embeddings computados para cada sessão
    session_embeddings: HashMap<String, Vec<f32>>,
    /// Configuração
    config: EmbeddingConfig,
}

/// Entrada no índice de conversas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// ID da sessão
    pub session_id: String,
    /// Resumo da conversa
    pub summary: String,
    /// Timestamp
    pub timestamp: String,
    /// Projeto associado
    pub project: Option<String>,
    /// Score de relevância
    pub relevance: f32,
}

/// Resultado de busca
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Entradas encontradas
    pub entries: Vec<ConversationEntry>,
    /// Query original
    pub query: String,
    /// Método usado (text_similarity ou embedding)
    pub method: String,
}

impl Default for ConversationEmbeddings {
    fn default() -> Self {
        Self {
            index: HashMap::new(),
            engine: Arc::new(RwLock::new(None)),
            session_embeddings: HashMap::new(),
            config: EmbeddingConfig::default(),
        }
    }
}

impl ConversationEmbeddings {
    /// Cria novo sistema de embeddings
    pub fn new() -> Self {
        Self::default()
    }

    /// Cria com configuração customizada
    pub fn with_config(config: EmbeddingConfig) -> Self {
        Self {
            index: HashMap::new(),
            engine: Arc::new(RwLock::new(None)),
            session_embeddings: HashMap::new(),
            config,
        }
    }

    /// Inicializa o engine de embeddings (async)
    pub async fn initialize(&mut self) {
        let engine = EmbeddingEngine::new(self.config.clone()).await;
        let mut lock = self.engine.write().await;
        *lock = Some(engine);
    }

    /// Retorna o backend ativo
    pub async fn backend(&self) -> EmbeddingBackend {
        let lock = self.engine.read().await;
        lock.as_ref()
            .map(|e| e.backend())
            .unwrap_or(EmbeddingBackend::Jaccard)
    }

    /// Indexa mensagens de sessões
    pub fn index_messages(&mut self, messages: &[SessionMessage]) {
        // Agrupa por sessão
        let mut sessions: HashMap<String, Vec<&SessionMessage>> = HashMap::new();
        for msg in messages {
            sessions.entry(msg.session_id.clone())
                .or_default()
                .push(msg);
        }

        // Cria entrada para cada sessão
        for (session_id, session_messages) in sessions {
            // Extrai tópicos da conversa
            let topics = extract_topics(&session_messages);

            for topic in topics {
                let entry = ConversationEntry {
                    session_id: session_id.clone(),
                    summary: create_summary(&session_messages),
                    timestamp: session_messages.first()
                        .map(|m| m.timestamp.clone())
                        .unwrap_or_default(),
                    project: session_messages.first()
                        .and_then(|m| m.project.clone()),
                    relevance: 1.0,
                };

                self.index.entry(topic)
                    .or_default()
                    .push(entry);
            }
        }
    }

    /// Busca conversas similares (usa embeddings se disponível)
    pub async fn search(&self, query: &str) -> SearchResult {
        // Tenta usar embeddings reais primeiro
        let engine_guard = self.engine.read().await;
        if let Some(ref engine) = *engine_guard {
            if engine.backend() != EmbeddingBackend::Jaccard {
                // Busca semântica real com embeddings
                return self.search_with_embeddings(engine, query).await;
            }
        }
        drop(engine_guard);

        // Fallback para busca por keywords
        self.search_keywords(query)
    }

    /// Busca semântica com embeddings reais
    async fn search_with_embeddings(&self, engine: &EmbeddingEngine, query: &str) -> SearchResult {
        let query_emb = engine.embed(query).await;
        let mut results: Vec<ConversationEntry> = Vec::new();

        // Compara com embeddings de cada sessão
        for (session_id, session_emb) in &self.session_embeddings {
            let similarity = EmbeddingEngine::cosine_similarity(&query_emb, session_emb);

            if similarity > 0.5 {
                // Encontra a entrada correspondente
                for entries in self.index.values() {
                    for entry in entries {
                        if &entry.session_id == session_id {
                            let mut entry = entry.clone();
                            entry.relevance = similarity;
                            if !results.iter().any(|e| e.session_id == entry.session_id) {
                                results.push(entry);
                            }
                        }
                    }
                }
            }
        }

        // Ordena por relevância
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(10);

        SearchResult {
            entries: results,
            query: query.to_string(),
            method: "embedding".to_string(),
        }
    }

    /// Busca por keywords (fallback)
    fn search_keywords(&self, query: &str) -> SearchResult {
        let normalized = normalize_query(query);
        let keywords = extract_keywords(&normalized);

        let mut results: Vec<ConversationEntry> = Vec::new();

        // Busca por keywords
        for keyword in &keywords {
            if let Some(entries) = self.index.get(keyword) {
                for entry in entries {
                    // Evita duplicatas
                    if !results.iter().any(|e| e.session_id == entry.session_id) {
                        let mut entry = entry.clone();
                        entry.relevance = calculate_relevance(&normalized, &entry.summary);
                        results.push(entry);
                    }
                }
            }
        }

        // Busca por similaridade de texto (fallback)
        if results.is_empty() {
            for (topic, entries) in &self.index {
                let sim = text_similarity(&normalized, topic);
                if sim > 0.3 {
                    for entry in entries {
                        if !results.iter().any(|e| e.session_id == entry.session_id) {
                            let mut entry = entry.clone();
                            entry.relevance = sim;
                            results.push(entry);
                        }
                    }
                }
            }
        }

        // Ordena por relevância
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap());
        results.truncate(10);

        SearchResult {
            entries: results,
            query: query.to_string(),
            method: "text_similarity".to_string(),
        }
    }

    /// Verifica se existe conversa sobre um tópico
    pub async fn has_conversation_about(&self, topic: &str) -> bool {
        let result = self.search(topic).await;
        !result.entries.is_empty()
    }

    /// Computa embeddings para mensagens indexadas
    pub async fn compute_embeddings(&mut self, messages: &[SessionMessage]) {
        let engine_guard = self.engine.read().await;
        let Some(ref engine) = *engine_guard else {
            return;
        };

        // Agrupa por sessão
        let mut sessions: HashMap<String, Vec<&SessionMessage>> = HashMap::new();
        for msg in messages.iter().filter(|m| m.role == "user") {
            sessions.entry(msg.session_id.clone())
                .or_default()
                .push(msg);
        }

        // Computa embedding para cada sessão (média dos embeddings das mensagens)
        for (session_id, msgs) in sessions {
            if self.session_embeddings.contains_key(&session_id) {
                continue; // Já computado
            }

            // Concatena mensagens da sessão
            let combined: String = msgs.iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            // Limita tamanho
            let text = if combined.len() > 8000 {
                &combined[..8000]
            } else {
                &combined
            };

            let embedding = engine.embed(text).await;
            self.session_embeddings.insert(session_id, embedding);
        }
    }

    /// Retorna estatísticas
    pub async fn stats(&self) -> EmbeddingStats {
        let total_topics = self.index.len();
        let total_entries: usize = self.index.values().map(|v| v.len()).sum();

        let engine_guard = self.engine.read().await;
        let using_embeddings = engine_guard
            .as_ref()
            .map(|e| e.backend() != EmbeddingBackend::Jaccard)
            .unwrap_or(false);

        let backend_name = engine_guard
            .as_ref()
            .map(|e| format!("{:?}", e.backend()))
            .unwrap_or_else(|| "None".to_string());

        EmbeddingStats {
            indexed_topics: total_topics,
            indexed_conversations: total_entries,
            using_embeddings,
            backend: backend_name,
            session_embeddings: self.session_embeddings.len(),
        }
    }
}

/// Estatísticas de embeddings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingStats {
    pub indexed_topics: usize,
    pub indexed_conversations: usize,
    pub using_embeddings: bool,
    pub backend: String,
    pub session_embeddings: usize,
}

/// Extrai tópicos de uma sessão
fn extract_topics(messages: &[&SessionMessage]) -> Vec<String> {
    let mut topics = Vec::new();

    for msg in messages.iter().filter(|m| m.role == "user") {
        let keywords = extract_keywords(&msg.content.to_lowercase());
        for kw in keywords {
            if !topics.contains(&kw) && kw.len() > 3 {
                topics.push(kw);
            }
        }
    }

    topics.truncate(10); // Limite de tópicos por sessão
    topics
}

/// Cria resumo de uma sessão
fn create_summary(messages: &[&SessionMessage]) -> String {
    // Pega primeira mensagem do usuário
    messages
        .iter()
        .filter(|m| m.role == "user")
        .next()
        .map(|m| truncate(&m.content, 200))
        .unwrap_or_default()
}

/// Normaliza query para busca
fn normalize_query(query: &str) -> String {
    query
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extrai keywords de texto
fn extract_keywords(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| w.len() > 3)
        .filter(|w| !is_stopword(w))
        .map(|w| w.to_string())
        .collect()
}

/// Calcula relevância entre query e texto
fn calculate_relevance(query: &str, text: &str) -> f32 {
    text_similarity(query, &text.to_lowercase())
}

/// Similaridade de texto (Jaccard)
fn text_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<_> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<_> = b.split_whitespace().collect();

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

/// Trunca string
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Verifica se é stopword
fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "que", "para", "com", "nao", "uma", "por", "mais", "como", "mas",
        "foi", "ser", "tem", "seu", "sua", "ele", "ela", "isso", "esta",
        "the", "and", "for", "are", "but", "not", "you", "all", "can",
    ];
    STOPWORDS.contains(&word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_similarity() {
        let sim = text_similarity("deploy viva producao", "fazer deploy do viva");
        assert!(sim > 0.2); // Devem ter alguma similaridade
    }

    #[test]
    fn test_extract_keywords() {
        let keywords = extract_keywords("como fazer deploy do viva em producao");
        assert!(keywords.contains(&"deploy".to_string()));
        assert!(keywords.contains(&"viva".to_string()));
        assert!(keywords.contains(&"producao".to_string()));
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let mut embeddings = ConversationEmbeddings::new();

        let messages = vec![
            SessionMessage {
                session_id: "session1".to_string(),
                role: "user".to_string(),
                content: "como fazer deploy do viva em producao".to_string(),
                timestamp: "2024-01-01".to_string(),
                cwd: None,
                project: Some("viva".to_string()),
            },
        ];

        embeddings.index_messages(&messages);

        // Sem initialize(), usa fallback keywords
        let result = embeddings.search("deploy viva").await;
        assert!(!result.entries.is_empty());
        assert_eq!(result.method, "text_similarity");
    }

    #[tokio::test]
    async fn test_backend_detection() {
        let embeddings = ConversationEmbeddings::new();

        // Sem initialize, backend é Jaccard
        let backend = embeddings.backend().await;
        assert_eq!(backend, EmbeddingBackend::Jaccard);
    }
}
