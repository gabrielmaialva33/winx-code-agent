//! Interactive REPL Module
//!
//! aichat-style chat interface with:
//! - Custom prompt showing model/session
//! - Autocompletion for commands
//! - Syntax highlighting
//! - Markdown rendering for responses
//! - Spinner during API calls
//! - Clipboard support
//! - Bilingual support (PT-BR / English)
//! - Self-aware agent with onboarding

mod completer;
mod highlighter;
pub mod i18n;
mod prompt;
mod render;

use std::io::{self, Write};
use std::time::Duration;

use arboard::Clipboard;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, EditCommand, EditMode, Emacs, KeyCode, KeyModifiers,
    Keybindings, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, ValidationResult,
    Validator,
};

use crate::agent::WinxAgent;
use crate::chat::{ChatConfig, ChatEngine};
use crate::errors::{Result, WinxError};
use crate::providers::StreamEvent;

use self::completer::WinxCompleter;
use self::highlighter::WinxHighlighter;
use self::i18n::{Language, COMMANDS};
use self::prompt::WinxPrompt;
use self::render::MarkdownRender;

const MENU_NAME: &str = "completion_menu";

/// Interactive REPL
pub struct Interactive {
    engine: ChatEngine,
    editor: Reedline,
    prompt: WinxPrompt,
    render: MarkdownRender,
    last_response: Option<String>,
    last_input: Option<String>,
    lang: Language,
    agent: WinxAgent,
}

impl Interactive {
    /// Create new interactive REPL
    pub fn new(config: ChatConfig) -> Result<Self> {
        let mut config = config;
        let agent = WinxAgent::new();

        // Set system prompt from agent's identity
        config.system_prompt = Some(agent.system_prompt());

        let engine = ChatEngine::new(config);
        let lang = Language::detect();
        let editor = Self::create_editor(&lang)?;
        let prompt = WinxPrompt::new(&engine, lang);
        let render = MarkdownRender::new();

        Ok(Self {
            engine,
            editor,
            prompt,
            render,
            last_response: None,
            last_input: None,
            lang,
            agent,
        })
    }

    /// Create with specific language
    pub fn with_language(config: ChatConfig, lang: Language) -> Result<Self> {
        let mut config = config;
        let agent = WinxAgent::new();

        // Set system prompt from agent's identity
        config.system_prompt = Some(agent.system_prompt());

        let engine = ChatEngine::new(config);
        let editor = Self::create_editor(&lang)?;
        let prompt = WinxPrompt::new(&engine, lang);
        let render = MarkdownRender::new();

        Ok(Self {
            engine,
            editor,
            prompt,
            render,
            last_response: None,
            last_input: None,
            lang,
            agent,
        })
    }

    /// Run the interactive REPL
    pub async fn run(&mut self) -> Result<()> {
        // Check if onboarding is needed
        if self.agent.needs_onboarding() {
            self.agent.onboard().await?;
        }

        self.print_banner();

        // Initialize session with agent's system prompt
        self.engine.new_session()?;

        // Set system prompt from agent (includes sense data)
        if let Some(ref mut session) = self.engine.session {
            session.set_system_prompt(&self.agent.system_prompt());
        }

        let s = self.lang.strings();

        loop {
            // Update prompt with current state
            self.prompt.update(&self.engine, self.lang);

            match self.editor.read_line(&self.prompt) {
                Ok(Signal::Success(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    // Process multiline :::
                    let line = self.process_multiline(line);

                    match self.handle_input(&line).await {
                        Ok(true) => break, // Exit requested
                        Ok(false) => {}
                        Err(e) => {
                            eprintln!("\x1b[31m✗ Error: {}\x1b[0m\n", e);
                        }
                    }
                }
                Ok(Signal::CtrlC) => {
                    let hint = match self.lang {
                        Language::Portuguese => "(Pressione Ctrl+D ou digite .sair pra sair)",
                        Language::English => "(Press Ctrl+D or type .exit to quit)",
                    };
                    println!("\x1b[90m{}\x1b[0m\n", hint);
                }
                Ok(Signal::CtrlD) => {
                    break;
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗ {}: {}\x1b[0m\n", s.msg_input_error, e);
                    break;
                }
            }
        }

        println!("\x1b[90m{} 👋\x1b[0m", s.msg_goodbye);
        Ok(())
    }

    /// Process multiline blocks with :::
    fn process_multiline(&self, line: &str) -> String {
        if line.starts_with(":::") && line.len() > 3 {
            let content = line[3..].trim();
            if content.ends_with(":::") {
                // Remove trailing :::
                content[..content.len() - 3].trim().to_string()
            } else {
                content.to_string()
            }
        } else {
            line.to_string()
        }
    }

    /// Process user input
    async fn handle_input(&mut self, line: &str) -> Result<bool> {
        // Check for commands
        if line.starts_with('.') {
            return self.handle_command(line).await;
        }

        // Send message to LLM
        self.last_input = Some(line.to_string());
        self.send_message(line).await?;
        Ok(false)
    }

    /// Process dot commands
    async fn handle_command(&mut self, line: &str) -> Result<bool> {
        let (cmd, args) = match line.split_once(' ') {
            Some((c, a)) => (c, Some(a.trim())),
            None => (line, None),
        };

        let s = self.lang.strings();
        let cmd_lower = cmd.to_lowercase();

        // Exit
        if matches!(cmd_lower.as_str(), ".sair" | ".exit" | ".quit" | ".q") {
            return Ok(true);
        }

        // Help
        if matches!(cmd_lower.as_str(), ".ajuda" | ".help" | ".?") {
            self.print_help();
            return Ok(false);
        }

        // Language switch
        if matches!(cmd_lower.as_str(), ".idioma" | ".lang") {
            if let Some(lang_code) = args {
                self.lang = Language::from_code(lang_code);
                self.prompt = WinxPrompt::new(&self.engine, self.lang);
                let msg = match self.lang {
                    Language::Portuguese => "Idioma alterado para Português",
                    Language::English => "Language changed to English",
                };
                println!("\x1b[32m✓ {}\x1b[0m\n", msg);
            } else {
                let usage = match self.lang {
                    Language::Portuguese => "Uso: .idioma <pt|en>",
                    Language::English => "Usage: .lang <pt|en>",
                };
                println!("{}\n", usage);
            }
            return Ok(false);
        }

        // Model
        if matches!(cmd_lower.as_str(), ".modelo" | ".model") {
            if let Some(model) = args {
                self.engine.set_model(model);
                println!("\x1b[32m✓ {} {}\x1b[0m\n", s.msg_model_changed, model);
            } else {
                let usage = match self.lang {
                    Language::Portuguese => {
                        "Uso: .modelo <provider:modelo>\n\nExemplos:\n  .modelo nvidia:qwen/qwen3-235b-a22b-fp8\n  .modelo ollama:llama3.2"
                    }
                    Language::English => {
                        "Usage: .model <provider:model>\n\nExamples:\n  .model nvidia:qwen/qwen3-235b-a22b-fp8\n  .model ollama:llama3.2"
                    }
                };
                println!("{}\n", usage);
            }
            return Ok(false);
        }

        // Models list
        if matches!(cmd_lower.as_str(), ".modelos" | ".models") {
            let title = match self.lang {
                Language::Portuguese => "Modelos disponíveis",
                Language::English => "Available models",
            };
            println!("\x1b[1m{}:\x1b[0m\n", title);
            for (id, desc) in self.engine.list_models() {
                println!("  \x1b[36m{}\x1b[0m", id);
                println!("    {}\n", desc);
            }
            return Ok(false);
        }

        // Providers list
        if cmd_lower == ".providers" {
            let title = match self.lang {
                Language::Portuguese => "Providers disponíveis",
                Language::English => "Available providers",
            };
            println!("\x1b[1m{}:\x1b[0m\n", title);
            for provider in self.engine.list_providers() {
                println!("  \x1b[33m{}\x1b[0m", provider);
            }
            println!();
            return Ok(false);
        }

        // Clear history
        if matches!(cmd_lower.as_str(), ".limpar" | ".clear") {
            if let Some(ref mut session) = self.engine.session {
                session.clear();
            }
            self.last_response = None;
            self.last_input = None;
            println!("\x1b[90m{}\x1b[0m\n", s.msg_history_cleared);
            return Ok(false);
        }

        // New session
        if matches!(cmd_lower.as_str(), ".sessao" | ".session") {
            self.engine.new_session()?;
            self.last_response = None;
            self.last_input = None;
            let msg = match self.lang {
                Language::Portuguese => "Nova sessão iniciada",
                Language::English => "New session started",
            };
            println!("\x1b[32m✓ {}\x1b[0m\n", msg);
            return Ok(false);
        }

        // Info
        if cmd_lower == ".info" {
            self.print_info();
            return Ok(false);
        }

        // Copy to clipboard
        if matches!(cmd_lower.as_str(), ".copiar" | ".copy") {
            self.copy_to_clipboard()?;
            return Ok(false);
        }

        // Continue response
        if matches!(cmd_lower.as_str(), ".continuar" | ".continue") {
            if let Some(ref response) = self.last_response {
                if response.is_empty() {
                    println!("\x1b[90m{}\x1b[0m\n", s.msg_no_response);
                } else {
                    let prompt = match self.lang {
                        Language::Portuguese => "Continue a resposta anterior de onde parou.",
                        Language::English => "Continue the previous response from where it stopped.",
                    };
                    self.send_message(prompt).await?;
                }
            } else {
                println!("\x1b[90m{}\x1b[0m\n", s.msg_no_response);
            }
            return Ok(false);
        }

        // Regenerate
        if matches!(cmd_lower.as_str(), ".regenerar" | ".regenerate") {
            if let Some(ref input) = self.last_input.clone() {
                let msg = match self.lang {
                    Language::Portuguese => "Regenerando resposta...",
                    Language::English => "Regenerating response...",
                };
                println!("\x1b[90m{}\x1b[0m\n", msg);
                self.send_message(&input).await?;
            } else {
                println!("\x1b[90m{}\x1b[0m\n", s.msg_no_message);
            }
            return Ok(false);
        }

        // Include file
        if matches!(cmd_lower.as_str(), ".arquivo" | ".file") {
            if let Some(path) = args {
                self.include_file(path).await?;
            } else {
                let usage = match self.lang {
                    Language::Portuguese => "Uso: .arquivo <caminho>\n\nExemplos:\n  .arquivo src/main.rs\n  .arquivo ~/docs/texto.md",
                    Language::English => "Usage: .file <path>\n\nExamples:\n  .file src/main.rs\n  .file ~/docs/text.md",
                };
                println!("{}\n", usage);
            }
            return Ok(false);
        }

        // History
        if matches!(cmd_lower.as_str(), ".historico" | ".history") {
            self.print_history();
            return Ok(false);
        }

        // Unknown command
        let unknown_msg = match self.lang {
            Language::Portuguese => format!("Comando desconhecido: {}\nDigite .ajuda pra ver comandos disponíveis.", cmd),
            Language::English => format!("Unknown command: {}\nType .help to see available commands.", cmd),
        };
        println!("\x1b[31m{}\x1b[0m\n", unknown_msg);

        Ok(false)
    }

    /// Send message to LLM with spinner and stream response
    async fn send_message(&mut self, message: &str) -> Result<()> {
        let s = self.lang.strings();

        let session = self.engine.session.as_mut().ok_or_else(|| {
            WinxError::ConfigurationError("No active session".to_string())
        })?;

        // Create spinner
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        spinner.set_message(s.msg_thinking);
        spinner.enable_steady_tick(Duration::from_millis(80));

        // Start stream
        let stream_result = session.send_stream(message).await;

        // Stop spinner
        spinner.finish_and_clear();

        let mut stream = stream_result?;
        let mut response = String::new();

        // Print with markdown rendering
        print!("\x1b[36m");
        io::stdout().flush().ok();

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Text(text) => {
                    response.push_str(&text);
                    // Render markdown incrementally
                    let rendered = self.render.render_incremental(&text);
                    print!("{}", rendered);
                    io::stdout().flush().ok();
                }
                StreamEvent::Done => {
                    println!("\x1b[0m\n");
                    break;
                }
                StreamEvent::Error(err) => {
                    println!("\x1b[0m");
                    return Err(WinxError::AIError(err));
                }
                _ => {}
            }
        }

        self.last_response = Some(response);
        Ok(())
    }

    /// Copy last response to clipboard
    fn copy_to_clipboard(&self) -> Result<()> {
        let s = self.lang.strings();

        if let Some(ref response) = self.last_response {
            if response.is_empty() {
                println!("\x1b[90m{}\x1b[0m\n", s.msg_no_response);
                return Ok(());
            }

            match Clipboard::new() {
                Ok(mut clipboard) => {
                    match clipboard.set_text(response.clone()) {
                        Ok(_) => {
                            println!(
                                "\x1b[32m✓ {} ({} chars)\x1b[0m\n",
                                s.msg_copied,
                                response.len()
                            );
                        }
                        Err(e) => {
                            println!("\x1b[31m✗ {}: {}\x1b[0m\n", s.msg_copy_error, e);
                        }
                    }
                }
                Err(e) => {
                    // Fallback: show response for manual copy
                    println!("\x1b[33m⚠ {}: {}\x1b[0m", s.msg_copy_fallback, e);
                    println!("\x1b[90m({} chars):\x1b[0m\n", response.len());
                    println!("{}\n", response);
                }
            }
        } else {
            println!("\x1b[90m{}\x1b[0m\n", s.msg_no_response);
        }
        Ok(())
    }

    /// Include file in conversation
    async fn include_file(&mut self, path: &str) -> Result<()> {
        let s = self.lang.strings();
        let path = shellexpand::tilde(path);
        let path = std::path::Path::new(path.as_ref());

        if !path.exists() {
            let msg = match self.lang {
                Language::Portuguese => format!("Arquivo não encontrado: {}", path.display()),
                Language::English => format!("File not found: {}", path.display()),
            };
            println!("\x1b[31m✗ {}\x1b[0m\n", msg);
            return Ok(());
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                let message = match self.lang {
                    Language::Portuguese => format!(
                        "Analise este arquivo ({}):\n\n```\n{}\n```",
                        filename, content
                    ),
                    Language::English => format!(
                        "Analyze this file ({}):\n\n```\n{}\n```",
                        filename, content
                    ),
                };
                println!("\x1b[90m{}: {} ({} bytes)\x1b[0m\n", s.msg_file_included, filename, content.len());
                self.send_message(&message).await?;
            }
            Err(e) => {
                println!("\x1b[31m✗ {}: {}\x1b[0m\n", s.msg_file_error, e);
            }
        }
        Ok(())
    }

    /// Print message history
    fn print_history(&self) {
        use crate::providers::{Role, MessageContent};

        let title = match self.lang {
            Language::Portuguese => "Histórico de Mensagens",
            Language::English => "Message History",
        };
        println!("\n\x1b[1m{}:\x1b[0m\n", title);

        if let Some(ref session) = self.engine.session {
            let messages = session.messages();
            if messages.is_empty() {
                let empty = match self.lang {
                    Language::Portuguese => "Nenhuma mensagem ainda.",
                    Language::English => "No messages yet.",
                };
                println!("  \x1b[90m{}\x1b[0m\n", empty);
            } else {
                for (i, msg) in messages.iter().enumerate() {
                    let is_user = matches!(msg.role, Role::User);
                    let role_color = if is_user { "33" } else { "36" };
                    let role_label = match (&msg.role, self.lang) {
                        (Role::User, Language::Portuguese) => "Você",
                        (Role::User, Language::English) => "You",
                        (Role::Assistant, _) => "AI",
                        (Role::System, _) => "System",
                    };

                    // Extract text content
                    let content_text = match &msg.content {
                        MessageContent::Text(s) => s.clone(),
                        MessageContent::Parts(parts) => {
                            parts.iter()
                                .filter_map(|p| {
                                    if let crate::providers::ContentPart::Text { text } = p {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        }
                        MessageContent::ToolResult { content, .. } => content.clone(),
                    };

                    let preview: String = content_text.chars().take(60).collect();
                    let ellipsis = if content_text.len() > 60 { "..." } else { "" };
                    println!("  \x1b[{}m[{}] {}:\x1b[0m {}{}", role_color, i + 1, role_label, preview, ellipsis);
                }
                println!();
            }
        }
    }

    /// Create reedline editor with custom settings
    fn create_editor(lang: &Language) -> Result<Reedline> {
        let completer = WinxCompleter::new(*lang);
        let highlighter = WinxHighlighter::new();
        let menu = Self::create_menu();
        let edit_mode = Self::create_edit_mode();

        let editor = Reedline::create()
            .with_completer(Box::new(completer))
            .with_highlighter(Box::new(highlighter))
            .with_menu(menu)
            .with_edit_mode(edit_mode)
            .with_quick_completions(true)
            .with_partial_completions(true)
            .use_bracketed_paste(true)
            .with_validator(Box::new(InputValidator))
            .with_ansi_colors(true);

        Ok(editor)
    }

    fn create_menu() -> ReedlineMenu {
        let menu = ColumnarMenu::default().with_name(MENU_NAME);
        ReedlineMenu::EngineCompleter(Box::new(menu))
    }

    fn create_edit_mode() -> Box<dyn EditMode> {
        let mut keybindings = default_emacs_keybindings();

        // Tab for completion
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu(MENU_NAME.to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );

        // Shift+Tab for previous completion
        keybindings.add_binding(
            KeyModifiers::SHIFT,
            KeyCode::BackTab,
            ReedlineEvent::MenuPrevious,
        );

        // Ctrl+Enter for newline
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Enter,
            ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
        );

        // Ctrl+J for newline (alternative)
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('j'),
            ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
        );

        // Ctrl+O to open external editor (uses $EDITOR or $VISUAL)
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('o'),
            ReedlineEvent::OpenEditor,
        );

        Box::new(Emacs::new(keybindings))
    }

    fn print_banner(&self) {
        let s = self.lang.strings();

        println!();
        println!("\x1b[36m╔═══════════════════════════════════════════════════════════╗\x1b[0m");
        println!(
            "\x1b[36m║\x1b[0m  \x1b[1m🚀 Winx Chat\x1b[0m v{}                                     \x1b[36m║\x1b[0m",
            env!("CARGO_PKG_VERSION")
        );
        println!("\x1b[36m║\x1b[0m     {}          \x1b[36m║\x1b[0m", s.banner_subtitle);
        println!("\x1b[36m╚═══════════════════════════════════════════════════════════╝\x1b[0m");
        println!();

        // Show available providers
        let providers = self.engine.list_providers();
        print!("  \x1b[90mProviders:\x1b[0m ");
        for (i, p) in providers.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("\x1b[33m{}\x1b[0m", p);
        }
        println!();
        println!();
        println!("  \x1b[90m{}\x1b[0m", s.hint_help);
        println!("  \x1b[90m{}\x1b[0m", s.hint_multiline);
        println!("  \x1b[90m{}\x1b[0m", s.hint_editor);
        println!();
    }

    fn print_help(&self) {
        let s = self.lang.strings();

        println!();
        println!("\x1b[1m{}:\x1b[0m", s.help_commands);
        println!();

        for cmd in COMMANDS {
            let name = cmd.name(self.lang);
            let desc = cmd.description(self.lang);
            println!("  \x1b[33m{:<16}\x1b[0m {}", name, desc);
        }

        println!();
        println!("\x1b[90m{}\x1b[0m", s.aliases_note);

        println!();
        println!("\x1b[1m{}:\x1b[0m", s.help_shortcuts);
        println!();

        let shortcuts = match self.lang {
            Language::Portuguese => vec![
                ("Tab", "Autocomplete"),
                ("Ctrl+Enter", "Nova linha (input multiline)"),
                ("Ctrl+O", "Abre editor externo ($EDITOR)"),
                ("Ctrl+C", "Cancela input atual"),
                ("Ctrl+D", "Sair"),
            ],
            Language::English => vec![
                ("Tab", "Autocomplete"),
                ("Ctrl+Enter", "New line (multiline input)"),
                ("Ctrl+O", "Open external editor ($EDITOR)"),
                ("Ctrl+C", "Cancel current input"),
                ("Ctrl+D", "Exit"),
            ],
        };

        for (key, desc) in shortcuts {
            println!("  \x1b[90m{:<16}\x1b[0m {}", key, desc);
        }

        println!();
        println!("\x1b[1m{}:\x1b[0m", s.help_multiline);
        println!();

        let multiline_hint = match self.lang {
            Language::Portuguese => vec![
                "Digite ::: pra iniciar bloco multiline",
                "Digite ::: novamente pra finalizar",
            ],
            Language::English => vec![
                "Type ::: to start multiline block",
                "Type ::: again to finish",
            ],
        };

        for hint in multiline_hint {
            println!("  {}", hint);
        }
        println!();
    }

    fn print_info(&self) {
        let title = match self.lang {
            Language::Portuguese => "Info da Sessão",
            Language::English => "Session Info",
        };
        println!();
        println!("\x1b[1m{}:\x1b[0m", title);
        println!();

        if let Some(ref session) = self.engine.session {
            let labels = match self.lang {
                Language::Portuguese => ("ID", "Provider", "Modelo", "Mensagens", "Criado em"),
                Language::English => ("ID", "Provider", "Model", "Messages", "Created at"),
            };
            println!("  \x1b[90m{}:\x1b[0m          {}", labels.0, session.meta.id);
            println!(
                "  \x1b[90m{}:\x1b[0m    \x1b[33m{}\x1b[0m",
                labels.1, session.meta.provider
            );
            println!("  \x1b[90m{}:\x1b[0m      \x1b[36m{}\x1b[0m", labels.2, session.meta.model);
            println!("  \x1b[90m{}:\x1b[0m   {}", labels.3, session.messages().len());
            println!(
                "  \x1b[90m{}:\x1b[0m   {}",
                labels.4,
                session.meta.created_at.format("%d/%m/%Y %H:%M")
            );
        } else {
            let no_session = match self.lang {
                Language::Portuguese => "Nenhuma sessão ativa",
                Language::English => "No active session",
            };
            println!("  \x1b[90m{}\x1b[0m", no_session);
        }
        println!();
    }
}

/// Input validator for multiline support
struct InputValidator;

impl Validator for InputValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        let line = line.trim();

        // Support ::: for multiline blocks (like aichat)
        if line.starts_with(":::") && !line[3..].trim_end().ends_with(":::") {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        // Should default to Portuguese if no env set
        let lang = Language::from_code("unknown");
        assert_eq!(lang, Language::Portuguese);
    }

    #[test]
    fn test_commands_bilingual() {
        let help = &COMMANDS[0];
        assert!(help.matches(".ajuda"));
        assert!(help.matches(".help"));
    }

    #[test]
    fn test_multiline_processing() {
        let interactive = Interactive {
            engine: ChatEngine::new(ChatConfig::default()),
            editor: Reedline::create(),
            prompt: WinxPrompt::default(),
            render: MarkdownRender::new(),
            last_response: None,
            last_input: None,
            lang: Language::English,
            agent: WinxAgent::new(),
        };

        assert_eq!(interactive.process_multiline(":::hello:::"), "hello");
        assert_eq!(interactive.process_multiline(":::multi\nline:::"), "multi\nline");
        assert_eq!(interactive.process_multiline("normal text"), "normal text");
    }
}
