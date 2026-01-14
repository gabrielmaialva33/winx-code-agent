//! Communication Learner - Learns communication style.
//!
//! Analyzes:
//! - Typical vocabulary ("mano", "massa", "kkk")
//! - Request structure
//! - How the user corrects the AI

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Communication pattern learning.
#[derive(Debug, Default)]
pub struct CommunicationLearner {
    /// Word counts
    word_counts: HashMap<String, usize>,
    /// Typical expressions (slang, etc)
    expressions: HashMap<String, usize>,
    /// Correction patterns
    corrections: Vec<CorrectionPattern>,
    /// Total messages analyzed
    message_count: usize,
}

/// Detected correction pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionPattern {
    /// Correction phrase
    pub phrase: String,
    /// Context (what came before)
    pub context: String,
    /// Frequency
    pub count: usize,
}

/// Typical expressions to detect.
const TYPICAL_EXPRESSIONS: &[&str] = &[
    "mano",
    "massa",
    "kkk",
    "kkkk",
    "kkkkk",
    "saquei",
    "sacou",
    "bora",
    "vamo",
    "blz",
    "beleza",
    "show",
    "dahora",
    "top",
    "foda",
    "pqp",
    "caralho",
    "porra",
    "aff",
    "eita",
    "po",
    "poh",
    "tlgd",
    "entendeu",
    "nao entendeu",
    "vc nao entendeu",
    "errado",
    "nao e isso",
    "quero",
    "preciso",
    "faz",
    "roda",
    "executa",
    "mostra",
    "lista",
    "abre",
];

/// Correction patterns.
const CORRECTION_PATTERNS: &[&str] = &[
    "nao e isso",
    "nao era isso",
    "errado",
    "ta errado",
    "nao quis dizer",
    "vc nao entendeu",
    "nao entendeu",
    "de novo",
    "repete",
    "tenta de novo",
    "para",
    "cancela",
    "esquece",
];

impl CommunicationLearner {
    /// Creates a new learner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyzes a message.
    pub fn analyze(&mut self, content: &str) {
        self.message_count += 1;

        let content_lower = content.to_lowercase();

        // Count typical expressions
        for expr in TYPICAL_EXPRESSIONS {
            let count = content_lower.matches(expr).count();
            if count > 0 {
                *self.expressions.entry((*expr).to_string()).or_insert(0) += count;
            }
        }

        // Detect correction patterns
        for pattern in CORRECTION_PATTERNS {
            if content_lower.contains(pattern) {
                // Add or increment pattern
                let existing = self.corrections.iter_mut().find(|c| c.phrase == *pattern);
                if let Some(correction) = existing {
                    correction.count += 1;
                } else {
                    self.corrections.push(CorrectionPattern {
                        phrase: (*pattern).to_string(),
                        context: truncate(&content_lower, 100),
                        count: 1,
                    });
                }
            }
        }

        // Count words (filter stopwords)
        let words = extract_words(&content_lower);
        for word in words {
            if word.len() > 2 && !is_stopword(&word) {
                *self.word_counts.entry(word).or_insert(0) += 1;
            }
        }
    }

    /// Returns vocabulary sorted by frequency.
    pub fn get_vocabulary(&self) -> Vec<(String, usize)> {
        let mut vocab: Vec<_> = self.expressions.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        vocab.sort_by(|a, b| b.1.cmp(&a.1));
        vocab.truncate(50); // Top 50
        vocab
    }

    /// Returns most used words.
    pub fn get_top_words(&self, limit: usize) -> Vec<(String, usize)> {
        let mut words: Vec<_> = self.word_counts.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        words.sort_by(|a, b| b.1.cmp(&a.1));
        words.truncate(limit);
        words
    }

    /// Returns correction patterns.
    pub fn get_corrections(&self) -> Vec<CorrectionPattern> {
        let mut corrections = self.corrections.clone();
        corrections.sort_by(|a, b| b.count.cmp(&a.count));
        corrections
    }

    /// Returns statistics.
    pub fn stats(&self) -> CommunicationStats {
        CommunicationStats {
            messages_analyzed: self.message_count,
            unique_expressions: self.expressions.len(),
            unique_words: self.word_counts.len(),
            correction_patterns: self.corrections.len(),
        }
    }
}

/// Communication statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationStats {
    pub messages_analyzed: usize,
    pub unique_expressions: usize,
    pub unique_words: usize,
    pub correction_patterns: usize,
}

/// Extracts words from text.
fn extract_words(text: &str) -> Vec<String> {
    let re = Regex::new(r"[a-záàâãéèêíïóôõöúçñ]+").unwrap();
    re.find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// Truncates string.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Checks if word is a stopword (Portuguese).
fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "que", "para", "com", "nao", "uma", "por", "mais", "como", "mas",
        "foi", "ser", "tem", "seu", "sua", "ele", "ela", "isso", "esta",
        "esse", "essa", "dos", "das", "nos", "nas", "pela", "pelo", "aos",
        "the", "and", "for", "are", "but", "not", "you", "all", "can",
        "had", "her", "was", "one", "our", "out", "has", "have", "been",
        "this", "that", "with", "from", "your", "they", "will", "would",
    ];
    STOPWORDS.contains(&word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_expressions() {
        let mut learner = CommunicationLearner::new();
        learner.analyze("mano, isso ta massa! kkk");

        let vocab = learner.get_vocabulary();
        assert!(!vocab.is_empty());

        // Should find "mano", "massa", "kkk"
        let words: Vec<_> = vocab.iter().map(|(w, _)| w.as_str()).collect();
        assert!(words.contains(&"mano"));
        assert!(words.contains(&"massa"));
        assert!(words.contains(&"kkk"));
    }

    #[test]
    fn test_detect_corrections() {
        let mut learner = CommunicationLearner::new();
        learner.analyze("nao e isso que eu quero");
        learner.analyze("ta errado, refaz");

        let corrections = learner.get_corrections();
        assert!(!corrections.is_empty());
    }

    #[test]
    fn test_stats() {
        let mut learner = CommunicationLearner::new();
        learner.analyze("teste um");
        learner.analyze("teste dois");

        let stats = learner.stats();
        assert_eq!(stats.messages_analyzed, 2);
    }
}
