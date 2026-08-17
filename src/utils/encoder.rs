//! Claude-compatible token counting.
//!
//! WCGW counts tokens with the `Xenova/claude-tokenizer` (Hugging Face `tokenizers`).
//! We embed that same tokenizer definition in the binary and load it lazily, so token
//! budgets and truncation match the model that actually runs the agent. Small inputs
//! use a byte-length upper bound and never initialize it; if loading fails for a larger
//! input we fall back to a cheap character/word estimate.

use std::sync::OnceLock;

use tokie::Tokenizer;

/// Embedded `Xenova/claude-tokenizer` definition (Hugging Face `tokenizer.json`).
static CLAUDE_TOKENIZER_JSON: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/claude-tokenizer.json"));

fn tokenizer() -> Option<&'static Tokenizer> {
    static TOKENIZER: OnceLock<Option<Tokenizer>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| {
            let json = match std::str::from_utf8(CLAUDE_TOKENIZER_JSON) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!("Embedded Claude tokenizer is not UTF-8: {error}");
                    return None;
                }
            };
            match tokie::hf::from_json_str(json) {
                Ok(tokenizer) => Some(tokenizer),
                Err(error) => {
                    tracing::warn!(
                        "Failed to load embedded Claude tokenizer, using estimate: {error}"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Count tokens the way Claude does. Falls back to [`estimate_tokens`] on failure.
pub fn count_tokens(text: &str) -> usize {
    match encode_ids(text) {
        Some(ids) => ids.len(),
        None => estimate_tokens(text),
    }
}

/// Encode `text` into Claude token ids. Returns `None` if the tokenizer is
/// unavailable so callers can pick a byte-based fallback.
pub fn encode_ids(text: &str) -> Option<Vec<u32>> {
    let tokenizer = tokenizer()?;
    Some(tokenizer.encode_ids(text, false))
}

/// Decode Claude token ids back into text. Returns `None` on failure.
pub fn decode_ids(ids: &[u32]) -> Option<String> {
    let tokenizer = tokenizer()?;
    tokenizer.decode(ids)
}

/// Return `true` when byte length alone proves `text` fits the token budget.
///
/// The embedded tokenizer is byte-level BPE without added special tokens, so it
/// cannot emit more tokens than the number of UTF-8 input bytes. This conservative
/// fast path avoids loading or running the tokenizer for small payloads while exact
/// tokenization remains in charge near and above the budget boundary.
pub fn definitely_fits_token_budget(text: &str, max_tokens: usize) -> bool {
    text.len() <= max_tokens
}

/// Read a token-budget override from env var `var` (e.g.
/// `WINX_CODING_TOKEN_BUDGET`), falling back to `default` when unset, zero, or
/// unparseable. Lets large-context clients tune how much of each file is pulled
/// into context — and how much saved memory is kept — without a rebuild.
pub fn budget_from_env(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Cheap fallback estimate used only when the tokenizer is unavailable.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(text.split_whitespace().count())
}

#[cfg(test)]
mod budget_tests {
    use super::budget_from_env;

    #[test]
    fn falls_back_to_default_when_unset_or_invalid() {
        // A name nothing sets -> default. (Avoids mutating process env in tests.)
        assert_eq!(budget_from_env("WINX_DEFINITELY_UNSET_BUDGET_XYZ", 24_000), 24_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tokens_for_simple_text() {
        // Whatever the backend, a non-empty string must produce at least one token.
        assert!(count_tokens("hello world") >= 1);
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn estimate_is_nonzero_for_words() {
        assert!(estimate_tokens("a b c d") >= 4);
    }

    #[test]
    fn byte_length_is_a_safe_token_upper_bound() {
        for sample in [
            "plain English prose",
            "fn main() { println!(\"hello\"); }",
            "áéíóú 日本語 🚀",
            "symbols: !@#$%^&*()[]{}",
            "line one\nline two\nline three",
        ] {
            let ids = encode_ids(sample);
            assert!(
                ids.as_ref().is_some_and(|ids| ids.len() <= sample.len()),
                "sample produced more tokens than UTF-8 bytes"
            );
        }
    }

    #[test]
    fn embedded_claude_tokenizer_matches_golden_ids() -> anyhow::Result<()> {
        let cases: &[(&str, &[u32])] = &[
            ("", &[]),
            ("hello world", &[9381, 2253]),
            (
                "fn main() { println!(\"hello\"); }",
                &[3258, 1890, 370, 503, 637, 9706, 5, 496, 9381, 5018, 863],
            ),
            (
                "áéíóú 日本語 🚀",
                &[2273, 1222, 2843, 3024, 8286, 225, 12956, 12163, 23598, 257, 41270, 253, 227],
            ),
            ("line one\nline two\nline three", &[936, 813, 203, 936, 1231, 203, 936, 2119]),
        ];
        for (sample, expected) in cases {
            let actual = encode_ids(sample)
                .ok_or_else(|| anyhow::anyhow!("embedded tokenizer failed for {sample:?}"))?;
            assert_eq!(&actual, expected, "token ids diverged for {sample:?}");
            assert_eq!(decode_ids(&actual).as_deref(), Some(*sample));
        }
        Ok(())
    }

    #[test]
    fn byte_upper_bound_only_accepts_payloads_proven_to_fit() {
        assert!(definitely_fits_token_budget("four", 4));
        assert!(!definitely_fits_token_budget("five!", 4));
        assert!(!definitely_fits_token_budget("é", 1));
        assert!(definitely_fits_token_budget("é", 2));
    }
}
