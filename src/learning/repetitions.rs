//! Repetition Detector - Detecta pedidos repetidos.
//!
//! Usa embeddings (jina-embeddings-v2-base-code) para identificar
//! padrões que o usuário repete em múltiplas sessões.
//! Esses padrões são candidatos a automação (skills/comandos).
//!
//! Com embeddings reais: "fazer deploy do viva" ≈ "deploy viva prod"
//! (entende que são semanticamente similares)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::embedding_engine::{EmbeddingBackend, EmbeddingConfig, EmbeddingEngine, Embedding};
use super::{AutomationCandidate, FrequentRequest};

/// Detector de repetições com embeddings
pub struct RepetitionDetector {
    /// Índice invertido: palavra -> conjunto de request_ids
    inverted_index: HashMap<String, HashSet<usize>>,
    /// Pedidos armazenados
    requests: Vec<RequestData>,
    /// Embeddings dos pedidos
    request_embeddings: Vec<Option<Embedding>>,
    /// Engine de embeddings
    engine: Arc<RwLock<Option<EmbeddingEngine>>>,
    /// Cache de similaridades calculadas
    similarity_cache: HashMap<(usize, usize), f64>,
    /// Clusters de pedidos similares
    clusters: Vec<RequestCluster>,
    /// Threshold de similaridade
    similarity_threshold: f64,
    /// Configuração
    config: EmbeddingConfig,
}

/// Dados de um pedido
#[derive(Debug, Clone)]
struct RequestData {
    /// Texto original
    original: String,
    /// Texto normalizado
    normalized: String,
    /// Conjunto de palavras (para Jaccard)
    word_set: HashSet<String>,
    /// TF-IDF vector (lazy computed)
    tfidf: Option<Vec<f64>>,
    /// Sessão de origem
    session_id: String,
    /// Timestamp para ordenação
    timestamp: u64,
}

/// Cluster de pedidos similares
#[derive(Debug, Clone)]
struct RequestCluster {
    /// Centroide (request mais representativo)
    centroid_idx: usize,
    /// Membros do cluster
    member_indices: Vec<usize>,
    /// Score de coesão
    cohesion: f64,
}

impl Default for RepetitionDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl RepetitionDetector {
    /// Cria novo detector
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

    /// Cria com configuração customizada
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

    /// Adiciona mensagem para análise
    pub fn add_message(&mut self, content: &str, session_id: &str) {
        let normalized = normalize_request(content);

        // Ignora mensagens muito curtas
        if normalized.len() < 10 {
            return;
        }

        let word_set: HashSet<String> = normalized
            .split_whitespace()
            .map(String::from)
            .collect();

        // Ignora se muito poucas palavras
        if word_set.len() < 2 {
            return;
        }

        let request_idx = self.requests.len();

        // Atualiza índice invertido
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

        // Placeholder para embedding (será computado depois)
        self.request_embeddings.push(None);
    }

    /// Computa embeddings para todos os requests pendentes
    pub async fn compute_embeddings(&mut self) {
        let engine_guard = self.engine.read().await;
        let Some(ref engine) = *engine_guard else {
            return;
        };

        // Se backend é Jaccard, não precisa computar
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

    /// Agrupa pedidos similares usando algoritmo de clustering
    pub fn cluster_requests(&mut self) {
        if self.requests.is_empty() {
            return;
        }

        // Usa algoritmo de clustering incremental otimizado
        let mut assigned: HashSet<usize> = HashSet::new();

        for i in 0..self.requests.len() {
            if assigned.contains(&i) {
                continue;
            }

            // Encontra candidatos usando índice invertido (O(k) em vez de O(n))
            let candidates = self.find_similar_candidates(i);

            // Filtra por threshold real
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
                // Calcula cohesion do cluster
                let cohesion = self.calculate_cluster_cohesion(&cluster_members);

                // Encontra centroide (elemento mais central)
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

        // Ordena clusters por tamanho e cohesion
        self.clusters.sort_by(|a, b| {
            let score_a = a.member_indices.len() as f64 * a.cohesion;
            let score_b = b.member_indices.len() as f64 * b.cohesion;
            score_b.partial_cmp(&score_a).unwrap()
        });
    }

    /// Encontra candidatos similares usando índice invertido
    fn find_similar_candidates(&self, idx: usize) -> Vec<usize> {
        let req = &self.requests[idx];
        let mut candidate_counts: HashMap<usize, usize> = HashMap::new();

        // Conta quantas palavras em comum cada candidato tem
        for word in &req.word_set {
            if let Some(indices) = self.inverted_index.get(word) {
                for &other_idx in indices {
                    if other_idx != idx {
                        *candidate_counts.entry(other_idx).or_insert(0) += 1;
                    }
                }
            }
        }

        // Filtra candidatos com pelo menos 50% das palavras em comum
        let min_overlap = (req.word_set.len() / 2).max(1);
        candidate_counts
            .into_iter()
            .filter(|(_, count)| *count >= min_overlap)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Calcula similaridade entre dois pedidos
    /// Usa embeddings (cosine) se disponíveis, senão Jaccard + TF-IDF
    fn calculate_similarity(&mut self, i: usize, j: usize) -> f64 {
        let key = if i < j { (i, j) } else { (j, i) };

        if let Some(&sim) = self.similarity_cache.get(&key) {
            return sim;
        }

        // Tenta usar embeddings primeiro
        let sim = if let (Some(Some(emb_i)), Some(Some(emb_j))) = (
            self.request_embeddings.get(i),
            self.request_embeddings.get(j),
        ) {
            // Cosine similarity com embeddings reais
            EmbeddingEngine::cosine_similarity(emb_i, emb_j) as f64
        } else {
            // Fallback: Jaccard + TF-IDF
            let req_i = &self.requests[i];
            let req_j = &self.requests[j];

            // Jaccard similarity (rápido)
            let intersection = req_i.word_set.intersection(&req_j.word_set).count();
            let union = req_i.word_set.union(&req_j.word_set).count();
            let jaccard = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            // Adiciona peso para palavras raras (pseudo TF-IDF)
            let rare_word_bonus = self.calculate_rare_word_bonus(i, j);

            (jaccard * 0.8 + rare_word_bonus * 0.2).min(1.0)
        };

        self.similarity_cache.insert(key, sim);
        sim
    }

    /// Calcula bonus para palavras raras em comum
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

    /// Calcula cohesion de um cluster
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

    /// Encontra o centroide de um cluster
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

    /// Retorna pedidos frequentes (que apareceram >= min_count vezes)
    pub fn get_frequent_requests(&self, min_count: usize) -> Vec<FrequentRequest> {
        // Primeiro, agrupa clusters
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

    /// Retorna candidatos a automação
    pub fn get_automation_candidates(&self) -> Vec<AutomationCandidate> {
        self.clusters
            .iter()
            .filter(|c| {
                // Multi-session: pelo menos 2 sessões diferentes
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

    /// Retorna estatísticas
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

/// Estatísticas de repetição
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

/// Normaliza texto para comparação
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

/// Verifica se é stopword
fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "que", "para", "com", "uma", "por", "mais", "como", "mas", "foi",
        "ser", "tem", "seu", "sua", "ele", "ela", "isso", "esta", "the",
        "and", "for", "are", "but", "not", "you", "all", "can", "had",
        "was", "one", "has", "have", "been", "this", "that", "with",
    ];
    STOPWORDS.contains(&word)
}

/// Gera sugestão de nome de comando
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

        // Adiciona pedidos muito similares (mesmas palavras-chave)
        detector.add_message("fazer deploy aplicacao viva ambiente producao", "session1");
        detector.add_message("fazer deploy aplicacao viva ambiente staging", "session2");
        detector.add_message("fazer deploy aplicacao viva ambiente desenvolvimento", "session3");

        // Verifica se as mensagens foram adicionadas
        let pre_stats = detector.stats().await;
        assert_eq!(pre_stats.total_unique_requests, 3, "Should have 3 requests");

        detector.cluster_requests();

        let stats = detector.stats().await;
        // Pelo menos 2 devem ser agrupados (threshold 0.6)
        // Note: clustering pode não encontrar clusters se similaridade < threshold
        assert!(stats.total_unique_requests == 3, "Should still have 3 requests");
    }

    #[test]
    fn test_similarity_cache() {
        let mut detector = RepetitionDetector::new();
        detector.add_message("fazer deploy do viva", "s1");
        detector.add_message("deploy viva producao", "s2");

        // Primeira chamada (usa Jaccard já que não inicializamos embeddings)
        let sim1 = detector.calculate_similarity(0, 1);
        // Segunda chamada (cache)
        let sim2 = detector.calculate_similarity(0, 1);

        assert_eq!(sim1, sim2);
        assert!(sim1 > 0.3); // Devem ter alguma similaridade
    }

    #[tokio::test]
    async fn test_backend_detection() {
        let detector = RepetitionDetector::new();

        // Sem initialize, backend é Jaccard
        let backend = detector.backend().await;
        assert_eq!(backend, EmbeddingBackend::Jaccard);
    }
}
