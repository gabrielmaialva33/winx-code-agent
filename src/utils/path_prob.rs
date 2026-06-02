//! File-path relevance ranking, ported from wcgw's `FastPathAnalyzer`.
//!
//! wcgw ships a tiny unigram language model trained over repo paths: a
//! Hugging Face tokenizer (`paths_tokens.model`) plus a vocab file mapping each
//! token to its log-probability (`paths_model.vocab`). A path's score is the sum
//! of the log-probabilities of its tokens — higher (less negative) means the
//! path looks more like a "real source file worth showing" and less like noise.
//!
//! Both assets are embedded so ranking works offline with zero setup, matching
//! the wcgw package that bundles them alongside `repo_context.py`.

use std::collections::HashMap;
use std::sync::OnceLock;
use tokenizers::Tokenizer;

static PATHS_MODEL: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/paths_tokens.model"));
static PATHS_VOCAB: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/paths_model.vocab"));

struct PathAnalyzer {
    tokenizer: Tokenizer,
    vocab_probs: HashMap<String, f64>,
}

impl PathAnalyzer {
    fn load() -> Option<Self> {
        let tokenizer = match Tokenizer::from_bytes(PATHS_MODEL) {
            Ok(tokenizer) => tokenizer,
            Err(error) => {
                tracing::warn!("Failed to load embedded path-ranking model: {error}");
                return None;
            }
        };

        // Vocab lines are `<token>\t<log_prob>`; mirror wcgw's `split()` + len==2 check.
        let text = std::str::from_utf8(PATHS_VOCAB).ok()?;
        let mut vocab_probs = HashMap::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 {
                if let Ok(prob) = parts[1].parse::<f64>() {
                    vocab_probs.insert(parts[0].to_string(), prob);
                }
            }
        }

        Some(Self { tokenizer, vocab_probs })
    }

    fn sum_log_prob(&self, tokens: &[String]) -> f64 {
        tokens.iter().filter_map(|token| self.vocab_probs.get(token)).sum()
    }
}

fn analyzer() -> Option<&'static PathAnalyzer> {
    static ANALYZER: OnceLock<Option<PathAnalyzer>> = OnceLock::new();
    ANALYZER.get_or_init(PathAnalyzer::load).as_ref()
}

/// Score each path by summed token log-probability (higher = more relevant).
///
/// Returns `None` if the model failed to load, so callers can fall back to a
/// heuristic ordering instead of silently mis-ranking everything.
pub fn score_paths(paths: &[String]) -> Option<Vec<f64>> {
    let analyzer = analyzer()?;
    let scores = paths
        .iter()
        .map(|path| match analyzer.tokenizer.encode(path.as_str(), false) {
            Ok(encoding) => analyzer.sum_log_prob(encoding.get_tokens()),
            // Unencodable path sinks to the bottom rather than poisoning the batch.
            Err(_) => f64::MIN,
        })
        .collect();
    Some(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_source_above_noise_when_model_present() {
        let paths =
            vec!["src/main.rs".to_string(), "a/b/c/d/e/f/zzz_tmp_garbage_9f8a.bin".to_string()];
        if let Some(scores) = score_paths(&paths) {
            assert_eq!(scores.len(), 2);
            // A normal source path should not score worse than deep random noise.
            assert!(scores[0] >= scores[1], "expected src/main.rs >= noise path");
        }
    }
}
