//! Conversation Embeddings - Semantic search in conversations.
//!
//! Uses real embeddings (jina-embeddings-v2-base-code) to find
//! similar conversations. Runs locally on RTX 4090.
//!
//! "Have we talked about this before?" → real semantic search!

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine};
use super::SessionMessage;

/// Conversation embeddings system with real ML.
pub struct ConversationEmbeddings {
    /// Conversation index (by normalized topic)
    index: HashMap<String, Vec<ConversationEntry>>,
    /// Embedding engine (Candle/HTTP/Jaccard)
    engine: Arc<RwLock<Option<EmbeddingEngine>>>,
    /// Computed embeddings for each session
    session_embeddings: HashMap<String, Vec<f32>>,
    /// Configuration
    config: EmbeddingConfig,
}

/// Conversation index entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// Session ID
    pub session_id: String,
    /// Conversation summary
    pub summary: String,
    /// Timestamp
    pub timestamp: String,
    /// Associated project
    pub project: Option<String>,
    /// Relevance score
    pub relevance: f32,
}

/// Search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Found entries
    pub entries: Vec<ConversationEntry>,
    /// Original query
    pub query: String,
    /// Method used (text_similarity or embedding)
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
    /// Creates a new embeddings system.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates with custom configuration.
    pub fn with_config(config: EmbeddingConfig) -> Self {
        Self {
            index: HashMap::new(),
            engine: Arc::new(RwLock::new(None)),
            session_embeddings: HashMap::new(),
            config,
        }
    }

    /// Initializes the embedding engine (async).
    pub async fn initialize(&mut self) {
        let engine = EmbeddingEngine::new(self.config.clone()).await;
        let mut lock = self.engine.write().await;
        *lock = Some(engine);
    }

    /// Returns the active backend.
    pub async fn backend(&self) -> EmbeddingBackend {
        let lock = self.engine.read().await;
        lock.as_ref()
            .map(|e| e.backend())
            .unwrap_or(EmbeddingBackend::Jaccard)
    }

    /// Indexes session messages.
    pub fn index_messages(&mut self, messages: &[SessionMessage]) {
        // Group by session
        let mut sessions: HashMap<String, Vec<&SessionMessage>> = HashMap::new();
        for msg in messages {
            sessions.entry(msg.session_id.clone())
                .or_default()
                .push(msg);
        }

        // Create entry for each session
        for (session_id, session_messages) in sessions {
            // Extract conversation topics
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

    /// Searches for similar conversations (uses embeddings if available).
    pub async fn search(&self, query: &str) -> SearchResult {
        // Try to use real embeddings first
        let engine_guard = self.engine.read().await;
        if let Some(ref engine) = *engine_guard {
            if engine.backend() != EmbeddingBackend::Jaccard {
                // Real semantic search with embeddings
                return self.search_with_embeddings(engine, query).await;
            }
        }
        drop(engine_guard);

        // Fallback to keyword search
        self.search_keywords(query)
    }

    /// Semantic search with real embeddings.
    async fn search_with_embeddings(&self, engine: &EmbeddingEngine, query: &str) -> SearchResult {
        let query_emb = engine.embed(query).await;
        let mut results: Vec<ConversationEntry> = Vec::new();

        // Compare with embeddings of each session
        for (session_id, session_emb) in &self.session_embeddings {
            let similarity = EmbeddingEngine::cosine_similarity(&query_emb, session_emb);

            if similarity > 0.5 {
                // Find corresponding entry
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

        // Sort by relevance
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(10);

        SearchResult {
            entries: results,
            query: query.to_string(),
            method: "embedding".to_string(),
        }
    }

    /// Keyword search (fallback).
    fn search_keywords(&self, query: &str) -> SearchResult {
        let normalized = normalize_query(query);
        let keywords = extract_keywords(&normalized);

        let mut results: Vec<ConversationEntry> = Vec::new();

        // Search by keywords
        for keyword in &keywords {
            if let Some(entries) = self.index.get(keyword) {
                for entry in entries {
                    // Avoid duplicates
                    if !results.iter().any(|e| e.session_id == entry.session_id) {
                        let mut entry = entry.clone();
                        entry.relevance = calculate_relevance(&normalized, &entry.summary);
                        results.push(entry);
                    }
                }
            }
        }

        // Search by text similarity (fallback)
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

        // Sort by relevance
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap());
        results.truncate(10);

        SearchResult {
            entries: results,
            query: query.to_string(),
            method: "text_similarity".to_string(),
        }
    }

    /// Checks if there is a conversation about a topic.
    pub async fn has_conversation_about(&self, topic: &str) -> bool {
        let result = self.search(topic).await;
        !result.entries.is_empty()
    }

    /// Computes embeddings for indexed messages.
    pub async fn compute_embeddings(&mut self, messages: &[SessionMessage]) {
        let engine_guard = self.engine.read().await;
        let Some(ref engine) = *engine_guard else {
            return;
        };

        // Group by session
        let mut sessions: HashMap<String, Vec<&SessionMessage>> = HashMap::new();
        for msg in messages.iter().filter(|m| m.role == "user") {
            sessions.entry(msg.session_id.clone())
                .or_default()
                .push(msg);
        }

        // Compute embedding for each session (average of message embeddings)
        for (session_id, msgs) in sessions {
            if self.session_embeddings.contains_key(&session_id) {
                continue; // Already computed
            }

            // Concatenate session messages
            let combined: String = msgs.iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            // Limit size
            let text = if combined.len() > 8000 {
                &combined[..8000]
            } else {
                &combined
            };

            let embedding = engine.embed(text).await;
            self.session_embeddings.insert(session_id, embedding);
        }
    }

    /// Returns statistics.
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

/// Statistics of embeddings system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingStats {
    pub indexed_topics: usize,
    pub indexed_conversations: usize,
    pub using_embeddings: bool,
    pub backend: String,
    pub session_embeddings: usize,
}

/// Extracts topics from a session.
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

    topics.truncate(10); // Topic limit per session
    topics
}

/// Creates a session summary.
fn create_summary(messages: &[&SessionMessage]) -> String {
    // Get first user message
    messages
        .iter()
        .filter(|m| m.role == "user")
        .next()
        .map(|m| truncate(&m.content, 200))
        .unwrap_or_default()
}

/// Normalizes search query.
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

/// Extracts keywords from text.
fn extract_keywords(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| w.len() > 3)
        .filter(|w| !is_stopword(w))
        .map(|w| w.to_string())
        .collect()
}

/// Calculates relevance between query and text.
fn calculate_relevance(query: &str, text: &str) -> f32 {
    text_similarity(query, &text.to_lowercase())
}

/// Text similarity (Jaccard).
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

/// Truncates string.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Checks if word is a stopword.
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
        assert!(sim > 0.2); // Should have some similarity
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

        // Without initialize(), uses keyword fallback
        let result = embeddings.search("deploy viva").await;
        assert!(!result.entries.is_empty());
        assert_eq!(result.method, "text_similarity");
    }

    #[tokio::test]
    async fn test_backend_detection() {
        let embeddings = ConversationEmbeddings::new();

        // Without initialize, backend is Jaccard
        let backend = embeddings.backend().await;
        assert_eq!(backend, EmbeddingBackend::Jaccard);
    }
}
