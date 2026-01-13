//! Internationalization support for Winx TUI
//!
//! Supports PT-BR (default) and English

use std::env;

/// Supported languages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    Portuguese,
    English,
}

impl Language {
    /// Detect language from environment or explicit setting
    pub fn detect() -> Self {
        // Check explicit WINX_LANG first
        if let Ok(lang) = env::var("WINX_LANG") {
            return Self::from_code(&lang);
        }

        // Check LANG/LC_ALL
        for var in ["LANG", "LC_ALL", "LC_MESSAGES"] {
            if let Ok(lang) = env::var(var) {
                if lang.starts_with("pt") {
                    return Self::Portuguese;
                } else if lang.starts_with("en") {
                    return Self::English;
                }
            }
        }

        // Default to Portuguese (Gabriel's preference)
        Self::Portuguese
    }

    /// Parse from language code
    pub fn from_code(code: &str) -> Self {
        match code.to_lowercase().as_str() {
            "pt" | "pt-br" | "pt_br" | "portuguese" => Self::Portuguese,
            "en" | "en-us" | "en_us" | "english" => Self::English,
            _ => Self::Portuguese,
        }
    }

    /// Get language code
    pub fn code(&self) -> &'static str {
        match self {
            Self::Portuguese => "pt-br",
            Self::English => "en",
        }
    }
}

/// All translatable strings
pub struct Strings {
    // Banner
    pub banner_subtitle: &'static str,

    // Prompts and hints
    pub hint_help: &'static str,
    pub hint_multiline: &'static str,
    pub hint_editor: &'static str,
    pub prompt_you: &'static str,

    // Commands help
    pub cmd_help: &'static str,
    pub cmd_model: &'static str,
    pub cmd_models: &'static str,
    pub cmd_copy: &'static str,
    pub cmd_continue: &'static str,
    pub cmd_regenerate: &'static str,
    pub cmd_file: &'static str,
    pub cmd_clear: &'static str,
    pub cmd_exit: &'static str,
    pub cmd_history: &'static str,

    // Messages
    pub msg_thinking: &'static str,
    pub msg_copied: &'static str,
    pub msg_copy_error: &'static str,
    pub msg_copy_fallback: &'static str,
    pub msg_no_response: &'static str,
    pub msg_no_message: &'static str,
    pub msg_history_cleared: &'static str,
    pub msg_goodbye: &'static str,
    pub msg_model_changed: &'static str,
    pub msg_current_model: &'static str,
    pub msg_file_included: &'static str,
    pub msg_file_error: &'static str,
    pub msg_input_error: &'static str,
    pub msg_api_error: &'static str,

    // Help sections
    pub help_title: &'static str,
    pub help_commands: &'static str,
    pub help_shortcuts: &'static str,
    pub help_multiline: &'static str,
    pub help_examples: &'static str,

    // Aliases note
    pub aliases_note: &'static str,
}

/// Portuguese (Brazilian) strings
pub const PT_BR: Strings = Strings {
    // Banner
    banner_subtitle: "Chat LLM de alta performance no seu terminal",

    // Prompts and hints
    hint_help: "Digite .ajuda pra comandos, Ctrl+D pra sair",
    hint_multiline: "Use ::: pra input multiline (ex: :::texto:::)",
    hint_editor: "Ctrl+O abre editor externo ($EDITOR)",
    prompt_you: "Voce",

    // Commands help
    cmd_help: "Mostra esta ajuda",
    cmd_model: "Ver/trocar modelo atual",
    cmd_models: "Lista todos os modelos",
    cmd_copy: "Copia ultima resposta pro clipboard",
    cmd_continue: "Continua resposta anterior",
    cmd_regenerate: "Regenera ultima resposta",
    cmd_file: "Inclui conteudo de arquivo",
    cmd_clear: "Limpa historico da conversa",
    cmd_exit: "Sai do chat",
    cmd_history: "Mostra historico de mensagens",

    // Messages
    msg_thinking: "Pensando...",
    msg_copied: "Copiado!",
    msg_copy_error: "Erro ao copiar",
    msg_copy_fallback: "Clipboard indisponivel. Resposta:",
    msg_no_response: "Nenhuma resposta pra copiar.",
    msg_no_message: "Nenhuma mensagem pra regenerar.",
    msg_history_cleared: "Historico limpo!",
    msg_goodbye: "Ate mais!",
    msg_model_changed: "Modelo alterado para",
    msg_current_model: "Modelo atual",
    msg_file_included: "Arquivo incluido",
    msg_file_error: "Erro ao ler arquivo",
    msg_input_error: "Erro de input",
    msg_api_error: "Erro na API",

    // Help sections
    help_title: "Ajuda do Winx Chat",
    help_commands: "Comandos",
    help_shortcuts: "Atalhos",
    help_multiline: "Input multiline",
    help_examples: "Exemplos",

    // Aliases note
    aliases_note: "Aliases em ingles tambem funcionam",
};

/// English strings
pub const EN: Strings = Strings {
    // Banner
    banner_subtitle: "High-performance LLM chat in your terminal",

    // Prompts and hints
    hint_help: "Type .help for commands, Ctrl+D to exit",
    hint_multiline: "Use ::: for multiline input (e.g., :::text:::)",
    hint_editor: "Ctrl+O opens external editor ($EDITOR)",
    prompt_you: "You",

    // Commands help
    cmd_help: "Show this help",
    cmd_model: "View/change current model",
    cmd_models: "List all available models",
    cmd_copy: "Copy last response to clipboard",
    cmd_continue: "Continue previous response",
    cmd_regenerate: "Regenerate last response",
    cmd_file: "Include file content",
    cmd_clear: "Clear conversation history",
    cmd_exit: "Exit chat",
    cmd_history: "Show message history",

    // Messages
    msg_thinking: "Thinking...",
    msg_copied: "Copied!",
    msg_copy_error: "Copy error",
    msg_copy_fallback: "Clipboard unavailable. Response:",
    msg_no_response: "No response to copy.",
    msg_no_message: "No message to regenerate.",
    msg_history_cleared: "History cleared!",
    msg_goodbye: "Goodbye!",
    msg_model_changed: "Model changed to",
    msg_current_model: "Current model",
    msg_file_included: "File included",
    msg_file_error: "Error reading file",
    msg_input_error: "Input error",
    msg_api_error: "API error",

    // Help sections
    help_title: "Winx Chat Help",
    help_commands: "Commands",
    help_shortcuts: "Shortcuts",
    help_multiline: "Multiline input",
    help_examples: "Examples",

    // Aliases note
    aliases_note: "Portuguese aliases also work",
};

impl Language {
    /// Get strings for this language
    pub fn strings(&self) -> &'static Strings {
        match self {
            Self::Portuguese => &PT_BR,
            Self::English => &EN,
        }
    }
}

/// Command definitions with both PT-BR and EN names
pub struct Command {
    pub pt: &'static str,
    pub en: &'static str,
    pub description_pt: &'static str,
    pub description_en: &'static str,
}

/// All commands with bilingual support
pub const COMMANDS: &[Command] = &[
    Command {
        pt: ".ajuda",
        en: ".help",
        description_pt: "Mostra esta ajuda",
        description_en: "Show this help",
    },
    Command {
        pt: ".modelo",
        en: ".model",
        description_pt: "Ver/trocar modelo atual",
        description_en: "View/change current model",
    },
    Command {
        pt: ".modelos",
        en: ".models",
        description_pt: "Lista todos os modelos",
        description_en: "List all available models",
    },
    Command {
        pt: ".copiar",
        en: ".copy",
        description_pt: "Copia ultima resposta pro clipboard",
        description_en: "Copy last response to clipboard",
    },
    Command {
        pt: ".continuar",
        en: ".continue",
        description_pt: "Continua resposta anterior",
        description_en: "Continue previous response",
    },
    Command {
        pt: ".regenerar",
        en: ".regenerate",
        description_pt: "Regenera ultima resposta",
        description_en: "Regenerate last response",
    },
    Command {
        pt: ".arquivo",
        en: ".file",
        description_pt: "Inclui conteudo de arquivo",
        description_en: "Include file content",
    },
    Command {
        pt: ".limpar",
        en: ".clear",
        description_pt: "Limpa historico da conversa",
        description_en: "Clear conversation history",
    },
    Command {
        pt: ".historico",
        en: ".history",
        description_pt: "Mostra historico de mensagens",
        description_en: "Show message history",
    },
    Command {
        pt: ".sair",
        en: ".exit",
        description_pt: "Sai do chat",
        description_en: "Exit chat",
    },
    Command {
        pt: ".idioma",
        en: ".lang",
        description_pt: "Troca idioma (pt/en)",
        description_en: "Change language (pt/en)",
    },
];

impl Command {
    /// Get description for language
    pub fn description(&self, lang: Language) -> &'static str {
        match lang {
            Language::Portuguese => self.description_pt,
            Language::English => self.description_en,
        }
    }

    /// Get primary name for language
    pub fn name(&self, lang: Language) -> &'static str {
        match lang {
            Language::Portuguese => self.pt,
            Language::English => self.en,
        }
    }

    /// Check if input matches this command (either language)
    pub fn matches(&self, input: &str) -> bool {
        let input_lower = input.to_lowercase();
        input_lower == self.pt || input_lower == self.en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        // Default should be Portuguese
        assert_eq!(Language::from_code("pt"), Language::Portuguese);
        assert_eq!(Language::from_code("pt-br"), Language::Portuguese);
        assert_eq!(Language::from_code("en"), Language::English);
        assert_eq!(Language::from_code("en-us"), Language::English);
        assert_eq!(Language::from_code("unknown"), Language::Portuguese);
    }

    #[test]
    fn test_command_matching() {
        let help_cmd = &COMMANDS[0];
        assert!(help_cmd.matches(".ajuda"));
        assert!(help_cmd.matches(".help"));
        assert!(help_cmd.matches(".AJUDA"));
        assert!(!help_cmd.matches(".modelo"));
    }
}
