//! Custom prompt for winx REPL
//!
//! Shows: provider:model session_info >

use std::borrow::Cow;

use reedline::{Prompt, PromptHistorySearch, PromptHistorySearchStatus, PromptEditMode};

use crate::chat::ChatEngine;
use super::i18n::Language;

/// Winx custom prompt with language support
pub struct WinxPrompt {
    left: String,
    right: String,
    lang: Language,
}

impl Default for WinxPrompt {
    fn default() -> Self {
        Self {
            left: String::new(),
            right: String::new(),
            lang: Language::default(),
        }
    }
}

impl WinxPrompt {
    /// Create new prompt
    pub fn new(engine: &ChatEngine, lang: Language) -> Self {
        let mut prompt = Self {
            left: String::new(),
            right: String::new(),
            lang,
        };
        prompt.update(engine, lang);
        prompt
    }

    /// Update prompt with current engine state
    pub fn update(&mut self, engine: &ChatEngine, lang: Language) {
        self.lang = lang;

        // Left: provider:model
        let (provider, model) = if let Some(ref session) = engine.session {
            (session.provider_name(), session.model_name())
        } else {
            ("none", "none")
        };

        // Shorten model name for display
        let model_short = shorten_model_name(model);

        self.left = format!(
            "\x1b[33m{}\x1b[0m:\x1b[36m{}\x1b[0m ",
            provider, model_short
        );

        // Right: message count if session active
        if let Some(ref session) = engine.session {
            let count = session.messages().len();
            if count > 0 {
                let label = match lang {
                    Language::Portuguese => "msgs",
                    Language::English => "msgs",
                };
                self.right = format!("\x1b[90m[{} {}]\x1b[0m", count, label);
            } else {
                self.right = String::new();
            }
        } else {
            self.right = String::new();
        }
    }
}

impl Prompt for WinxPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }

    fn render_prompt_indicator(&self, edit_mode: PromptEditMode) -> Cow<'_, str> {
        match edit_mode {
            PromptEditMode::Default | PromptEditMode::Emacs => Cow::Borrowed("\x1b[32m❯\x1b[0m "),
            PromptEditMode::Vi(vi_mode) => {
                match vi_mode {
                    reedline::PromptViMode::Normal => Cow::Borrowed("\x1b[33m❮\x1b[0m "),
                    reedline::PromptViMode::Insert => Cow::Borrowed("\x1b[32m❯\x1b[0m "),
                }
            }
            PromptEditMode::Custom(_) => Cow::Borrowed("> "),
        }
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("\x1b[90m··· \x1b[0m")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        let (prefix_ok, prefix_fail) = match self.lang {
            Language::Portuguese => ("", "falhou "),
            Language::English => ("", "failing "),
        };

        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => prefix_ok,
            PromptHistorySearchStatus::Failing => prefix_fail,
        };

        let label = match self.lang {
            Language::Portuguese => "busca",
            Language::English => "search",
        };

        Cow::Owned(format!(
            "\x1b[90m({}{}: {})\x1b[0m ",
            prefix, label, history_search.term
        ))
    }
}

/// Shorten model name for display
fn shorten_model_name(model: &str) -> &str {
    // Extract just the model name from paths like "qwen/qwen3-235b-a22b-fp8"
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        // Truncate if too long
        .get(..20)
        .unwrap_or(model.rsplit('/').next().unwrap_or(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorten_model_name() {
        assert_eq!(shorten_model_name("llama3.2"), "llama3.2");
        assert_eq!(
            shorten_model_name("qwen/qwen3-235b-a22b-fp8"),
            "qwen3-235b-a22b-fp8"
        );
        assert_eq!(
            shorten_model_name("meta/llama-3.3-70b-instruct"),
            "llama-3.3-70b-instru"
        );
    }
}
