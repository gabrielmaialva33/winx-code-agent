//! Claude-compatible token counting.
//!
//! WCGW counts tokens with the `Xenova/claude-tokenizer` (Hugging Face `tokenizers`).
//! We embed that same tokenizer definition in the binary and load it lazily, so token
//! budgets and truncation match the model that actually runs the agent. If the
//! tokenizer fails to load we fall back to a cheap character/word estimate.

use std::sync::OnceLock;
use tokenizers::Tokenizer;

/// Embedded `Xenova/claude-tokenizer` definition (Hugging Face `tokenizer.json`).
static CLAUDE_TOKENIZER_JSON: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/claude-tokenizer.json"));

fn tokenizer() -> Option<&'static Tokenizer> {
    static TOKENIZER: OnceLock<Option<Tokenizer>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| match Tokenizer::from_bytes(CLAUDE_TOKENIZER_JSON) {
            Ok(tokenizer) => Some(tokenizer),
            Err(error) => {
                tracing::warn!("Failed to load embedded Claude tokenizer, using estimate: {error}");
                None
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
    tokenizer.encode(text, false).ok().map(|encoding| encoding.get_ids().to_vec())
}

/// Decode Claude token ids back into text. Returns `None` on failure.
pub fn decode_ids(ids: &[u32]) -> Option<String> {
    let tokenizer = tokenizer()?;
    tokenizer.decode(ids, false).ok()
}

/// Cheap fallback estimate used only when the tokenizer is unavailable.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(text.split_whitespace().count())
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
}
