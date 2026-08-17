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

use serde::Deserialize;
use tokie::Tokenizer;

static PATHS_MODEL: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/paths_tokens.model"));
static PATHS_VOCAB: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/paths_model.vocab"));

struct PathAnalyzer {
    tokenizer: Tokenizer,
    id_tokens: Vec<String>,
    vocab_probs: HashMap<String, f64>,
}

#[derive(Deserialize)]
struct PathTokenizerFile {
    model: PathTokenizerModel,
}

#[derive(Deserialize)]
struct PathTokenizerModel {
    vocab: Vec<(String, f64)>,
}

impl PathAnalyzer {
    fn load() -> Option<Self> {
        let model_json = std::str::from_utf8(PATHS_MODEL).ok()?;
        let tokenizer = match tokie::hf::from_json_str(model_json) {
            Ok(tokenizer) => tokenizer,
            Err(error) => {
                tracing::warn!("Failed to load embedded path-ranking model: {error}");
                return None;
            }
        };
        let id_tokens = serde_json::from_str::<PathTokenizerFile>(model_json)
            .ok()?
            .model
            .vocab
            .into_iter()
            .map(|(token, _)| token)
            .collect::<Vec<_>>();

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

        Some(Self { tokenizer, id_tokens, vocab_probs })
    }

    fn sum_log_prob(&self, token_ids: &[u32]) -> f64 {
        token_ids
            .iter()
            .filter_map(|token_id| self.id_tokens.get(*token_id as usize))
            .filter_map(|token| self.vocab_probs.get(token))
            .sum()
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
pub fn score_paths<S: AsRef<str>>(paths: &[S]) -> Option<Vec<f64>> {
    let analyzer = analyzer()?;
    let scores = paths
        .iter()
        .map(|path| analyzer.sum_log_prob(&analyzer.tokenizer.encode_ids(path.as_ref(), false)))
        .collect();
    Some(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_path_tokenizer_matches_golden_ids() -> anyhow::Result<()> {
        let analyzer =
            analyzer().ok_or_else(|| anyhow::anyhow!("embedded path analyzer failed"))?;
        let cases: &[(&str, &[u32])] = &[
            ("src/main.rs", &[13, 3, 21, 4, 506]),
            ("node_modules/react/index.js", &[14, 5, 7, 3, 75, 3, 63, 4, 9]),
            (
                "a/b/c/d/e/f/zzz_tmp_garbage_9f8a.bin",
                &[
                    3473, 3, 65, 3, 20, 3, 31, 3, 48, 3, 32, 3, 356, 1193, 5, 514, 5, 2101, 102,
                    2662, 5, 147, 32, 105, 42, 4, 268,
                ],
            ),
            (
                "tests/integração/日本語.rs",
                &[146, 3, 180, 5318, 18526, 18560, 137, 3, 18490, 18436, 18581, 4, 506],
            ),
        ];
        for (path, expected) in cases {
            let actual = analyzer.tokenizer.encode_ids(path, false);
            assert_eq!(&actual, expected, "path token ids diverged for {path:?}");
        }
        Ok(())
    }

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
