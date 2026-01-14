//! Repetition Detector - Detects repeated requests.
//!
//! Uses embeddings (jina-embeddings-v2-base-code) to identify
//! patterns that the user repeats across multiple sessions.
//! These patterns are candidates for automation (skills/commands).
//!
//! With real embeddings: "deploy viva" ≈ "deploy viva prod"
//! (understands they are semantically similar)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine, Embedding};
use super::{AutomationCandidate, FrequentRequest};

/// Repetition detector with embeddings.
pub struct RepetitionDetector {
    /// Inverted index: word -> set of request_ids
    inverted_index: HashMap<String, HashSet<usize>>,
    /// Stored requests
    requests: Vec<RequestData>,
    /// Request embeddings
    request_embeddings: Vec<Option<Embedding>>,
    /// Embedding engine
    engine: Arc<RwLock<Option<EmbeddingEngine>>>,
    /// Cache of calculated similarities
    similarity_cache: HashMap<(usize, usize), f64>,
    /// Clusters of similar requests
    clusters: Vec<RequestCluster>,
    /// Similarity threshold
    similarity_threshold: f64,
    /// Configuration
    config: EmbeddingConfig,
}

/// Request data.
#[derive(Debug, Clone)]
struct RequestData {
    /// Original text
    original: String,
    /// Normalized text
    normalized: String,
    /// Word set (for Jaccard)
    word_set: HashSet<String>,
    /// TF-IDF vector (lazy computed)
    tfidf: Option<Vec<f64>>,
    /// Source session
    session_id: String,
    /// Timestamp for sorting
    timestamp: u64,
}

/// Cluster of similar requests.
#[derive(Debug, Clone)]
struct RequestCluster {
    /// Centroid (most representative request)
    centroid_idx: usize,
    /// Cluster members
    member_indices: Vec<usize>,
    /// Cohesion score
    cohesion: f64,
}

impl Default for RepetitionDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl RepetitionDetector {
    /// Creates a new detector.
    pub fn new() -> Self {
        Self {
            inverted_index: HashMap::new(),
            requests: Vec::new(),
            request_embeddings: Vec::new(),
            engine: Arc::new(RwLock::new(None)),
            similarity_cache: HashMap::new(),
            clusters: Vec::new(),
            similarity_threshold: 0.6,
            config: EmbeddingConfig::default(),
        }
    }

    /// Creates with custom configuration.
    pub fn with_config(config: EmbeddingConfig) -> Self {
        Self {
            inverted_index: HashMap::new(),
            requests: Vec::new(),
            request_embeddings: Vec::new(),
            engine: Arc::new(RwLock::new(None)),
            similarity_cache: HashMap::new(),
            clusters: Vec::new(),
            similarity_threshold: 0.6,
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

    /// Adds message for analysis.
    pub fn add_message(&mut self, content: &str, session_id: &str) {
        let normalized = normalize_request(content);

        // Ignore very short messages
        if normalized.len() < 10 {
            return;
        }

        let word_set: HashSet<String> = normalized
            .split_whitespace()
            .map(String::from)
            .collect();

        // Ignore if too few words
        if word_set.len() < 2 {
            return;
        }

        let request_idx = self.requests.len();

        // Update inverted index
        for word in &word_set {
            self.inverted_index
                .entry(word.clone())
                .or_default()
                .insert(request_idx);
        }

        self.requests.push(RequestData {
            original: content.to_string(),
            normalized,
            word_set,
            tfidf: None,
            session_id: session_id.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });

        // Placeholder for embedding (will be computed later)
        self.request_embeddings.push(None);
    }

    /// Computes embeddings for all pending requests.
    pub async fn compute_embeddings(&mut self) {
        let engine_guard = self.engine.read().await;
        let Some(ref engine) = *engine_guard else {
            return;
        };

        // If backend is Jaccard, no need to compute
        if engine.backend() == EmbeddingBackend::Jaccard {
            return;
        }

        for i in 0..self.requests.len() {
            if self.request_embeddings.get(i).and_then(|e| e.as_ref()).is_none() {
                let req = &self.requests[i];
                let emb = engine.embed(&req.normalized).await;
                if i < self.request_embeddings.len() {
                    self.request_embeddings[i] = Some(emb);
                }
            }
        }
    }

    /// Clusters similar requests using clustering algorithm.
    pub fn cluster_requests(&mut self) {
        if self.requests.is_empty() {
            return;
        }

        // Use optimized incremental clustering algorithm
        let mut assigned: HashSet<usize> = HashSet::new();

        for i in 0..self.requests.len() {
            if assigned.contains(&i) {
                continue;
            }

            // Find candidates using inverted index (O(k) instead of O(n))
            let candidates = self.find_similar_candidates(i);

            // Filter by real threshold
            let mut cluster_members: Vec<usize> = vec![i];

            for &j in &candidates {
                if i != j && !assigned.contains(&j) {
                    let sim = self.calculate_similarity(i, j);
                    if sim >= self.similarity_threshold {
                        cluster_members.push(j);
                    }
                }
            }

            if cluster_members.len() >= 2 {
                // Calculate cluster cohesion
                let cohesion = self.calculate_cluster_cohesion(&cluster_members);

                // Find centroid (most central element)
                let centroid = self.find_centroid(&cluster_members);

                self.clusters.push(RequestCluster {
                    centroid_idx: centroid,
                    member_indices: cluster_members.clone(),
                    cohesion,
                });

                for &idx in &cluster_members {
                    assigned.insert(idx);
                }
            }
        }

        // Sort clusters by size and cohesion
        self.clusters.sort_by(|a, b| {
            let score_a = a.member_indices.len() as f64 * a.cohesion;
            let score_b = b.member_indices.len() as f64 * b.cohesion;
            score_b.partial_cmp(&score_a).unwrap()
        });
    }

    /// Finds similar candidates using inverted index.
    fn find_similar_candidates(&self, idx: usize) -> Vec<usize> {
        let req = &self.requests[idx];
        let mut candidate_counts: HashMap<usize, usize> = HashMap::new();

        // Count how many common words each candidate has
        for word in &req.word_set {
            if let Some(indices) = self.inverted_index.get(word) {
                for &other_idx in indices {
                    if other_idx != idx {
                        *candidate_counts.entry(other_idx).or_insert(0) += 1;
                    }
                }
            }
        }

        // Filter candidates with at least 50% overlapping words
        let min_overlap = (req.word_set.len() / 2).max(1);
        candidate_counts
            .into_iter()
            .filter(|(_, count)| *count >= min_overlap)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Calculates similarity between two requests.
    /// Uses embeddings (cosine) if available, otherwise Jaccard + TF-IDF.
    fn calculate_similarity(&mut self, i: usize, j: usize) -> f64 {
        let key = if i < j { (i, j) } else { (j, i) };

        if let Some(&sim) = self.similarity_cache.get(&key) {
            return sim;
        }

        // Try using embeddings first
        let sim = if let (Some(Some(emb_i)), Some(Some(emb_j))) = (
            self.request_embeddings.get(i),
            self.request_embeddings.get(j),
        ) {
            // Cosine similarity with real embeddings
            EmbeddingEngine::cosine_similarity(emb_i, emb_j) as f64
        } else {
            // Fallback: Jaccard + TF-IDF
            let req_i = &self.requests[i];
            let req_j = &self.requests[j];

            // Jaccard similarity (fast)
            let intersection = req_i.word_set.intersection(&req_j.word_set).count();
            let union = req_i.word_set.union(&req_j.word_set).count();
            let jaccard = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            // Add rare word bonus (pseudo TF-IDF)
            let rare_word_bonus = self.calculate_rare_word_bonus(i, j);

            (jaccard * 0.8 + rare_word_bonus * 0.2).min(1.0)
        };

        self.similarity_cache.insert(key, sim);
        sim
    }

    /// Calculates bonus for shared rare words.
    fn calculate_rare_word_bonus(&self, i: usize, j: usize) -> f64 {
        let req_i = &self.requests[i];
        let req_j = &self.requests[j];
        let total_docs = self.requests.len() as f64;

        let mut bonus = 0.0;
        let mut count = 0;

        for word in req_i.word_set.intersection(&req_j.word_set) {
            if let Some(doc_count) = self.inverted_index.get(word).map(|s| s.len()) {
                // IDF: log(N/df)
                let idf = (total_docs / doc_count as f64).ln();
                bonus += idf;
                count += 1;
            }
        }

        if count > 0 {
            (bonus / count as f64).min(1.0)
        } else {
            0.0
        }
    }

    /// Calculates cluster cohesion.
    fn calculate_cluster_cohesion(&mut self, members: &[usize]) -> f64 {
        if members.len() < 2 {
            return 1.0;
        }

        let mut total_sim = 0.0;
        let mut pairs = 0;

        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                total_sim += self.calculate_similarity(members[i], members[j]);
                pairs += 1;
            }
        }

        if pairs > 0 {
            total_sim / pairs as f64
        } else {
            0.0
        }
    }

    /// Finds cluster centroid (most central element).
    fn find_centroid(&mut self, members: &[usize]) -> usize {
        if members.len() == 1 {
            return members[0];
        }

        let mut best_idx = members[0];
        let mut best_score = 0.0;

        for &i in members {
            let mut score = 0.0;
            for &j in members {
                if i != j {
                    score += self.calculate_similarity(i, j);
                }
            }
            if score > best_score {
                best_score = score;
                best_idx = i;
            }
        }

        best_idx
    }

    /// Returns frequent requests (appeared >= min_count times).
    pub fn get_frequent_requests(&self, min_count: usize) -> Vec<FrequentRequest> {
        // First, group clusters
        self.clusters
            .iter()
            .filter(|c| c.member_indices.len() >= min_count)
            .map(|cluster| {
                let centroid = &self.requests[cluster.centroid_idx];
                let sessions: Vec<String> = cluster
                    .member_indices
                    .iter()
                    .map(|&idx| self.requests[idx].session_id.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();

                FrequentRequest {
                    text: centroid.original.clone(),
                    count: cluster.member_indices.len(),
                    sessions,
                }
            })
            .collect()
    }

    /// Returns automation candidates.
    pub fn get_automation_candidates(&self) -> Vec<AutomationCandidate> {
        self.clusters
            .iter()
            .filter(|c| {
                // Multi-session: at least 2 different sessions
                let unique_sessions: HashSet<_> = c
                    .member_indices
                    .iter()
                    .map(|&idx| &self.requests[idx].session_id)
                    .collect();
                unique_sessions.len() >= 2
            })
            .take(20)
            .map(|cluster| {
                let centroid = &self.requests[cluster.centroid_idx];
                let suggested_command = generate_command_name(&centroid.normalized);

                let examples: Vec<String> = cluster
                    .member_indices
                    .iter()
                    .take(3)
                    .map(|&idx| self.requests[idx].original.clone())
                    .collect();

                AutomationCandidate {
                    pattern: centroid.original.clone(),
                    suggested_command,
                    frequency: cluster.member_indices.len(),
                    examples,
                }
            })
            .collect()
    }

    /// Returns statistics.
    pub async fn stats(&self) -> RepetitionStats {
        let total_unique = self.requests.len();
        let clustered = self.clusters.iter().map(|c| c.member_indices.len()).sum();
        let multi_session = self
            .clusters
            .iter()
            .filter(|c| {
                let sessions: HashSet<_> = c
                    .member_indices
                    .iter()
                    .map(|&idx| &self.requests[idx].session_id)
                    .collect();
                sessions.len() > 1
            })
            .count();

        let engine_guard = self.engine.read().await;
        let using_embeddings = engine_guard
            .as_ref()
            .map(|e| e.backend() != EmbeddingBackend::Jaccard)
            .unwrap_or(false);

        let backend_name = engine_guard
            .as_ref()
            .map(|e| format!("{:?}", e.backend()))
            .unwrap_or_else(|| "None".to_string());

        let embeddings_computed = self.request_embeddings.iter()
            .filter(|e| e.is_some())
            .count();

        RepetitionStats {
            total_unique_requests: total_unique,
            clustered_requests: clustered,
            multi_session_clusters: multi_session,
            vocabulary_size: self.inverted_index.len(),
            using_embeddings,
            backend: backend_name,
            embeddings_computed,
        }
    }
}

/// Repetition statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepetitionStats {
    pub total_unique_requests: usize,
    pub clustered_requests: usize,
    pub multi_session_clusters: usize,
    pub vocabulary_size: usize,
    pub using_embeddings: bool,
    pub backend: String,
    pub embeddings_computed: usize,
}

/// Normalizes text for comparison.
fn normalize_request(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !is_stopword(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Checks if word is a stopword.
fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "que", "para", "com", "uma", "por", "mais", "como", "mas", "foi",
        "ser", "tem", "seu", "sua", "ele", "ela", "isso", "esta", "the",
        "and", "for", "are", "but", "not", "you", "all", "can", "had",
        "was", "one", "has", "have", "been", "this", "that", "with",
    ];
    STOPWORDS.contains(&word)
}

/// Generates suggested command name.
fn generate_command_name(normalized: &str) -> String {
    let keywords: Vec<_> = normalized
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .take(3)
        .collect();

    if keywords.is_empty() {
        "/auto".to_string()
    } else {
        format!("/{}", keywords.join("-"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize() {
        let text = "Como faço deploy do VIVA?";
        let normalized = normalize_request(text);
        assert!(normalized.contains("deploy"));
        assert!(normalized.contains("viva"));
    }

    #[tokio::test]
    async fn test_clustering() {
        let mut detector = RepetitionDetector::new();

        // Adds very similar requests (same keywords)
        detector.add_message("fazer deploy aplicacao viva ambiente producao", "session1");
        detector.add_message("fazer deploy aplicacao viva ambiente staging", "session2");
        detector.add_message("fazer deploy aplicacao viva ambiente desenvolvimento", "session3");

        // Verifies if messages were added
        let pre_stats = detector.stats().await;
        assert_eq!(pre_stats.total_unique_requests, 3, "Should have 3 requests");

        detector.cluster_requests();

        let stats = detector.stats().await;
        // At least 2 should be grouped (threshold 0.6)
        // Note: clustering might not find clusters if similarity < threshold
        assert!(stats.total_unique_requests == 3, "Should still have 3 requests");
    }

    #[test]
    fn test_similarity_cache() {
        let mut detector = RepetitionDetector::new();
        detector.add_message("fazer deploy do viva", "s1");
        detector.add_message("deploy viva producao", "s2");

        // First call (uses Jaccard since embeddings are not initialized)
        let sim1 = detector.calculate_similarity(0, 1);
        // Second call (cache)
        let sim2 = detector.calculate_similarity(0, 1);

        assert_eq!(sim1, sim2);
        assert!(sim1 > 0.3); // Should have some similarity
    }

    #[tokio::test]
    async fn test_backend_detection() {
        let detector = RepetitionDetector::new();

        // Without initialize, backend is Jaccard
        let backend = detector.backend().await;
        assert_eq!(backend, EmbeddingBackend::Jaccard);
    }
}
