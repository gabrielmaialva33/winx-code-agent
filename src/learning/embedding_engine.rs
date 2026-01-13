//! Embedding Engine - Motor de embeddings para busca semântica
//!
//! Suporta múltiplos backends:
//! - Candle local (CPU/GPU) com jina-embeddings-v2-base-code
//! - HTTP API (text-embeddings-inference container)
//! - Fallback para Jaccard (sem deps extras)
//!
//! A 4090 com 24GB VRAM roda jina-code tranquilamente.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Dimensão dos embeddings jina-code-v2
const EMBEDDING_DIM: usize = 768;

/// Modelo padrão
const DEFAULT_MODEL: &str = "jinaai/jina-embeddings-v2-base-code";

/// Backend de embeddings
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingBackend {
    /// Candle local (GPU se disponível)
    Candle,
    /// HTTP API (text-embeddings-inference)
    HttpApi,
    /// Fallback Jaccard (sem ML)
    Jaccard,
}

/// Configuração do engine
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Modelo a usar
    pub model_id: String,
    /// Backend preferido
    pub preferred_backend: EmbeddingBackend,
    /// URL do servidor TEI (se usar HttpApi)
    pub tei_url: Option<String>,
    /// Usar GPU se disponível
    pub use_gpu: bool,
    /// Batch size para processamento
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_id: DEFAULT_MODEL.to_string(),
            preferred_backend: EmbeddingBackend::Candle,
            tei_url: Some("http://localhost:8080".to_string()),
            use_gpu: true,
            batch_size: 32,
        }
    }
}

/// Vetor de embedding
pub type Embedding = Vec<f32>;

/// Engine de embeddings
pub struct EmbeddingEngine {
    config: EmbeddingConfig,
    backend: EmbeddingBackend,
    cache: Arc<RwLock<HashMap<String, Embedding>>>,
    #[cfg(feature = "embeddings")]
    candle_model: Option<CandleEmbedder>,
    http_client: Option<reqwest::Client>,
}

impl EmbeddingEngine {
    /// Cria novo engine, detectando melhor backend disponível
    pub async fn new(config: EmbeddingConfig) -> Self {
        let mut engine = Self {
            config: config.clone(),
            backend: EmbeddingBackend::Jaccard, // Default fallback
            cache: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(feature = "embeddings")]
            candle_model: None,
            http_client: None,
        };

        // Tenta inicializar backends em ordem de preferência
        match config.preferred_backend {
            EmbeddingBackend::Candle => {
                if engine.try_init_candle().await {
                    engine.backend = EmbeddingBackend::Candle;
                } else if engine.try_init_http().await {
                    engine.backend = EmbeddingBackend::HttpApi;
                }
            }
            EmbeddingBackend::HttpApi => {
                if engine.try_init_http().await {
                    engine.backend = EmbeddingBackend::HttpApi;
                } else if engine.try_init_candle().await {
                    engine.backend = EmbeddingBackend::Candle;
                }
            }
            EmbeddingBackend::Jaccard => {
                // Já é o default
            }
        }

        tracing::info!("EmbeddingEngine initialized with backend: {:?}", engine.backend);
        engine
    }

    /// Tenta inicializar Candle
    #[cfg(feature = "embeddings")]
    async fn try_init_candle(&mut self) -> bool {
        match CandleEmbedder::new(&self.config.model_id, self.config.use_gpu) {
            Ok(embedder) => {
                self.candle_model = Some(embedder);
                tracing::info!("Candle embedder initialized with model: {}", self.config.model_id);
                true
            }
            Err(e) => {
                tracing::warn!("Failed to init Candle: {}", e);
                false
            }
        }
    }

    #[cfg(not(feature = "embeddings"))]
    async fn try_init_candle(&mut self) -> bool {
        tracing::debug!("Candle embeddings not compiled (use --features embeddings)");
        false
    }

    /// Tenta inicializar HTTP API
    async fn try_init_http(&mut self) -> bool {
        if let Some(ref url) = self.config.tei_url {
            let client = reqwest::Client::new();

            // Tenta um health check
            match client.get(format!("{}/health", url)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    self.http_client = Some(client);
                    tracing::info!("TEI server available at {}", url);
                    true
                }
                _ => {
                    tracing::debug!("TEI server not available at {}", url);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Retorna backend ativo
    pub fn backend(&self) -> EmbeddingBackend {
        self.backend
    }

    /// Gera embedding para um texto
    pub async fn embed(&self, text: &str) -> Embedding {
        // Check cache
        {
            let cache = self.cache.read().await;
            if let Some(emb) = cache.get(text) {
                return emb.clone();
            }
        }

        let embedding = match self.backend {
            #[cfg(feature = "embeddings")]
            EmbeddingBackend::Candle => {
                if let Some(ref model) = self.candle_model {
                    model.embed(text).unwrap_or_else(|e| {
                        tracing::warn!("Candle embed failed: {}, falling back to Jaccard", e);
                        jaccard_pseudo_embedding(text)
                    })
                } else {
                    jaccard_pseudo_embedding(text)
                }
            }
            #[cfg(not(feature = "embeddings"))]
            EmbeddingBackend::Candle => jaccard_pseudo_embedding(text),

            EmbeddingBackend::HttpApi => {
                self.embed_http(text).await.unwrap_or_else(|e| {
                    tracing::warn!("HTTP embed failed: {}, falling back to Jaccard", e);
                    jaccard_pseudo_embedding(text)
                })
            }

            EmbeddingBackend::Jaccard => jaccard_pseudo_embedding(text),
        };

        // Cache result
        {
            let mut cache = self.cache.write().await;
            cache.insert(text.to_string(), embedding.clone());
        }

        embedding
    }

    /// Gera embeddings em batch
    pub async fn embed_batch(&self, texts: &[&str]) -> Vec<Embedding> {
        let mut results = Vec::with_capacity(texts.len());

        // Para Candle/HTTP, batch é mais eficiente
        // Por agora, processa sequencialmente
        for text in texts {
            results.push(self.embed(text).await);
        }

        results
    }

    /// Embed via HTTP API
    async fn embed_http(&self, text: &str) -> Result<Embedding, String> {
        let client = self.http_client.as_ref().ok_or("HTTP client not initialized")?;
        let url = self.config.tei_url.as_ref().ok_or("TEI URL not configured")?;

        #[derive(serde::Serialize)]
        struct EmbedRequest<'a> {
            inputs: &'a str,
        }

        let resp = client
            .post(format!("{}/embed", url))
            .json(&EmbedRequest { inputs: text })
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("TEI error: {}", resp.status()));
        }

        let embeddings: Vec<Vec<f32>> = resp.json().await.map_err(|e| e.to_string())?;
        embeddings.into_iter().next().ok_or_else(|| "Empty response".to_string())
    }

    /// Calcula similaridade entre dois embeddings (cosine similarity)
    pub fn cosine_similarity(a: &Embedding, b: &Embedding) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            0.0
        } else {
            dot / (norm_a * norm_b)
        }
    }

    /// Encontra os K mais similares
    pub async fn find_similar(&self, query: &str, candidates: &[&str], k: usize) -> Vec<(usize, f32)> {
        let query_emb = self.embed(query).await;
        let candidate_embs = self.embed_batch(candidates).await;

        let mut scores: Vec<(usize, f32)> = candidate_embs
            .iter()
            .enumerate()
            .map(|(i, emb)| (i, Self::cosine_similarity(&query_emb, emb)))
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        scores
    }

    /// Estatísticas do engine
    pub async fn stats(&self) -> EmbeddingStats {
        let cache = self.cache.read().await;
        EmbeddingStats {
            backend: self.backend,
            model_id: self.config.model_id.clone(),
            cache_size: cache.len(),
            embedding_dim: EMBEDDING_DIM,
        }
    }
}

/// Estatísticas do engine
#[derive(Debug, Clone)]
pub struct EmbeddingStats {
    pub backend: EmbeddingBackend,
    pub model_id: String,
    pub cache_size: usize,
    pub embedding_dim: usize,
}

/// Pseudo-embedding baseado em Jaccard (fallback)
/// Cria um vetor esparso baseado nos tokens do texto
fn jaccard_pseudo_embedding(text: &str) -> Embedding {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut embedding = vec![0.0f32; EMBEDDING_DIM];
    let tokens: Vec<&str> = text.split_whitespace().collect();

    for token in &tokens {
        let mut hasher = DefaultHasher::new();
        token.to_lowercase().hash(&mut hasher);
        let idx = (hasher.finish() as usize) % EMBEDDING_DIM;
        embedding[idx] += 1.0;
    }

    // Normaliza
    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut embedding {
            *x /= norm;
        }
    }

    embedding
}

// =============================================================================
// Candle Embedder (compilado só com feature "embeddings")
// =============================================================================

#[cfg(feature = "embeddings")]
mod candle_impl {
    use candle_core::{DType, Device, Module, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::jina_bert::{BertModel, Config};
    use hf_hub::{api::sync::Api, Repo, RepoType};
    use tokenizers::Tokenizer;

    pub struct CandleEmbedder {
        model: BertModel,
        tokenizer: Tokenizer,
        device: Device,
    }

    impl CandleEmbedder {
        pub fn new(model_id: &str, use_gpu: bool) -> anyhow::Result<Self> {
            // Determina device
            let device = if use_gpu {
                Device::cuda_if_available(0)?
            } else {
                Device::Cpu
            };

            tracing::info!("Loading model {} on {:?}", model_id, device);

            // Baixa modelo do HuggingFace Hub
            let api = Api::new()?;
            let repo = api.repo(Repo::new(model_id.to_string(), RepoType::Model));

            // Carrega tokenizer
            let tokenizer_path = repo.get("tokenizer.json")?;
            let tokenizer = Tokenizer::from_file(&tokenizer_path)
                .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

            // Carrega config
            let config_path = repo.get("config.json")?;
            let config: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;

            // Carrega weights
            let weights_path = repo.get("model.safetensors")?;
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(&[weights_path], DType::F32, &device)?
            };

            // Cria modelo (usa new, não load)
            let model = BertModel::new(vb, &config)?;

            Ok(Self {
                model,
                tokenizer,
                device,
            })
        }

        pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            // Tokeniza
            let encoding = self.tokenizer.encode(text, true)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

            let input_ids = Tensor::new(encoding.get_ids(), &self.device)?.unsqueeze(0)?;
            let attention_mask = Tensor::new(encoding.get_attention_mask(), &self.device)?
                .unsqueeze(0)?
                .to_dtype(DType::F32)?;

            // Forward pass (Module trait fornece o método forward)
            let embeddings = self.model.forward(&input_ids)?;

            // Mean pooling com attention mask
            let (_, seq_len, hidden_size) = embeddings.dims3()?;
            let mask_expanded = attention_mask
                .unsqueeze(2)?
                .broadcast_as((1, seq_len, hidden_size))?;

            let sum_embeddings = (embeddings * &mask_expanded)?.sum(1)?;
            let sum_mask = mask_expanded.sum(1)?.clamp(1e-9, f32::MAX)?;
            let mean_embeddings = (sum_embeddings / sum_mask)?;

            // Converte para Vec<f32>
            let embedding = mean_embeddings.squeeze(0)?.to_vec1::<f32>()?;
            Ok(embedding)
        }
    }
}

#[cfg(feature = "embeddings")]
use candle_impl::CandleEmbedder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jaccard_pseudo_embedding() {
        let emb1 = jaccard_pseudo_embedding("hello world");
        let emb2 = jaccard_pseudo_embedding("hello there");
        let emb3 = jaccard_pseudo_embedding("completely different text");

        assert_eq!(emb1.len(), EMBEDDING_DIM);

        // emb1 e emb2 devem ser mais similares que emb1 e emb3
        let sim12 = EmbeddingEngine::cosine_similarity(&emb1, &emb2);
        let sim13 = EmbeddingEngine::cosine_similarity(&emb1, &emb3);

        assert!(sim12 > sim13);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];

        assert!((EmbeddingEngine::cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
        assert!((EmbeddingEngine::cosine_similarity(&a, &c) - 0.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_engine_fallback() {
        let config = EmbeddingConfig {
            preferred_backend: EmbeddingBackend::Jaccard,
            ..Default::default()
        };
        let engine = EmbeddingEngine::new(config).await;

        assert_eq!(engine.backend(), EmbeddingBackend::Jaccard);

        let emb = engine.embed("test text").await;
        assert_eq!(emb.len(), EMBEDDING_DIM);
    }
}
