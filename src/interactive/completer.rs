//! Autocompletion for winx commands

use reedline::{Completer, Span, Suggestion};

use super::i18n::{Language, COMMANDS};

/// Winx command completer with bilingual support
pub struct WinxCompleter {
    lang: Language,
}

impl WinxCompleter {
    /// Create new completer for language
    pub fn new(lang: Language) -> Self {
        Self { lang }
    }
}

impl Completer for WinxCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let line_to_pos = &line[..pos];

        // Only complete if line starts with '.'
        if !line_to_pos.starts_with('.') {
            return vec![];
        }

        // Find word being completed
        let word_start = line_to_pos
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        let partial = &line_to_pos[word_start..].to_lowercase();

        // Collect all matching commands (both languages)
        let mut suggestions = Vec::new();

        for cmd in COMMANDS {
            // Check PT command
            if cmd.pt.starts_with(partial.as_str()) {
                suggestions.push(Suggestion {
                    value: cmd.pt.to_string(),
                    description: Some(cmd.description(self.lang).to_string()),
                    style: None,
                    extra: None,
                    span: Span::new(word_start, pos),
                    append_whitespace: true,
                });
            }
            // Check EN command (avoid duplicates)
            if cmd.en != cmd.pt && cmd.en.starts_with(partial.as_str()) {
                suggestions.push(Suggestion {
                    value: cmd.en.to_string(),
                    description: Some(cmd.description(self.lang).to_string()),
                    style: None,
                    extra: None,
                    span: Span::new(word_start, pos),
                    append_whitespace: true,
                });
            }
        }

        // Sort: primary language first
        suggestions.sort_by(|a, b| {
            let a_is_primary = match self.lang {
                Language::Portuguese => a.value.starts_with(".a") || a.value.starts_with(".m") || a.value.starts_with(".s") || a.value.starts_with(".c") || a.value.starts_with(".l") || a.value.starts_with(".i"),
                Language::English => a.value.starts_with(".h") || a.value.starts_with(".m") || a.value.starts_with(".e") || a.value.starts_with(".c") || a.value.starts_with(".f"),
            };
            let b_is_primary = match self.lang {
                Language::Portuguese => b.value.starts_with(".a") || b.value.starts_with(".m") || b.value.starts_with(".s") || b.value.starts_with(".c") || b.value.starts_with(".l") || b.value.starts_with(".i"),
                Language::English => b.value.starts_with(".h") || b.value.starts_with(".m") || b.value.starts_with(".e") || b.value.starts_with(".c") || b.value.starts_with(".f"),
            };
            b_is_primary.cmp(&a_is_primary).then(a.value.cmp(&b.value))
        });

        suggestions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completer_pt() {
        let mut completer = WinxCompleter::new(Language::Portuguese);

        // Complete ".aj"
        let suggestions = completer.complete(".aj", 3);
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.value == ".ajuda"));

        // Complete ".mo"
        let suggestions = completer.complete(".mo", 3);
        assert!(suggestions.iter().any(|s| s.value == ".modelo"));
        assert!(suggestions.iter().any(|s| s.value == ".modelos"));
    }

    #[test]
    fn test_completer_en() {
        let mut completer = WinxCompleter::new(Language::English);

        // Complete ".he"
        let suggestions = completer.complete(".he", 3);
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.value == ".help"));

        // Complete ".mo"
        let suggestions = completer.complete(".mo", 3);
        assert!(suggestions.iter().any(|s| s.value == ".model"));
        assert!(suggestions.iter().any(|s| s.value == ".models"));
    }

    #[test]
    fn test_no_completion_for_non_commands() {
        let mut completer = WinxCompleter::new(Language::English);

        let suggestions = completer.complete("hello", 5);
        assert!(suggestions.is_empty());
    }
}
