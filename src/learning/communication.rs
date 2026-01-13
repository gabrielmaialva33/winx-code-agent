//! Communication Learner - Aprende estilo de comunicação.
//!
//! Analisa:
//! - Vocabulário típico ("mano", "massa", "kkk")
//! - Estrutura de pedidos
//! - Como o usuário corrige o AI

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Aprendizado de padrões de comunicação
#[derive(Debug, Default)]
pub struct CommunicationLearner {
    /// Contagem de palavras
    word_counts: HashMap<String, usize>,
    /// Expressões típicas (gírias, etc)
    expressions: HashMap<String, usize>,
    /// Padrões de correção
    corrections: Vec<CorrectionPattern>,
    /// Total de mensagens analisadas
    message_count: usize,
}

/// Padrão de correção detectado
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionPattern {
    /// Frase de correção
    pub phrase: String,
    /// Contexto (o que veio antes)
    pub context: String,
    /// Frequência
    pub count: usize,
}

/// Palavras/expressões típicas para detectar
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

/// Padrões de correção
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
    /// Cria novo learner
    pub fn new() -> Self {
        Self::default()
    }

    /// Analisa uma mensagem
    pub fn analyze(&mut self, content: &str) {
        self.message_count += 1;

        let content_lower = content.to_lowercase();

        // Conta expressões típicas
        for expr in TYPICAL_EXPRESSIONS {
            let count = content_lower.matches(expr).count();
            if count > 0 {
                *self.expressions.entry(expr.to_string()).or_insert(0) += count;
            }
        }

        // Detecta padrões de correção
        for pattern in CORRECTION_PATTERNS {
            if content_lower.contains(pattern) {
                // Adiciona ou incrementa padrão
                let existing = self.corrections.iter_mut().find(|c| c.phrase == *pattern);
                if let Some(correction) = existing {
                    correction.count += 1;
                } else {
                    self.corrections.push(CorrectionPattern {
                        phrase: pattern.to_string(),
                        context: truncate(&content_lower, 100),
                        count: 1,
                    });
                }
            }
        }

        // Conta palavras (filtra stopwords)
        let words = extract_words(&content_lower);
        for word in words {
            if word.len() > 2 && !is_stopword(&word) {
                *self.word_counts.entry(word).or_insert(0) += 1;
            }
        }
    }

    /// Retorna vocabulário ordenado por frequência
    pub fn get_vocabulary(&self) -> Vec<(String, usize)> {
        let mut vocab: Vec<_> = self.expressions.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        vocab.sort_by(|a, b| b.1.cmp(&a.1));
        vocab.truncate(50); // Top 50
        vocab
    }

    /// Retorna palavras mais usadas
    pub fn get_top_words(&self, limit: usize) -> Vec<(String, usize)> {
        let mut words: Vec<_> = self.word_counts.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        words.sort_by(|a, b| b.1.cmp(&a.1));
        words.truncate(limit);
        words
    }

    /// Retorna padrões de correção
    pub fn get_corrections(&self) -> Vec<CorrectionPattern> {
        let mut corrections = self.corrections.clone();
        corrections.sort_by(|a, b| b.count.cmp(&a.count));
        corrections
    }

    /// Retorna estatísticas
    pub fn stats(&self) -> CommunicationStats {
        CommunicationStats {
            messages_analyzed: self.message_count,
            unique_expressions: self.expressions.len(),
            unique_words: self.word_counts.len(),
            correction_patterns: self.corrections.len(),
        }
    }
}

/// Estatísticas de comunicação
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationStats {
    pub messages_analyzed: usize,
    pub unique_expressions: usize,
    pub unique_words: usize,
    pub correction_patterns: usize,
}

/// Extrai palavras de um texto
fn extract_words(text: &str) -> Vec<String> {
    let re = Regex::new(r"[a-záàâãéèêíïóôõöúçñ]+").unwrap();
    re.find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// Trunca string
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Verifica se é stopword (português)
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

        // Deve encontrar "mano", "massa", "kkk"
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
