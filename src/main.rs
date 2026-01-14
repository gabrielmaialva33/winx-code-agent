//! # Winx - Shell Agent + CLI Chat + MCP Server
//!
//! Winx é uma implementação em Rust de alto desempenho para code agents.
//! Combina execução de shell, chat com múltiplos LLMs e protocolo MCP.
//!
//! ## Modos de Operação
//!
//! - `serve`: MCP server (default, para Claude Code)
//! - `chat`: Chat one-shot com LLM
//! - `repl`: REPL interativo

mod agent;
mod chat;
mod errors;
mod interactive;
mod learning;
mod providers;
mod server;
mod state;
mod tools;
mod types;
mod utils;

use std::io::{self, Write};

use clap::{Parser, Subcommand};
use futures::StreamExt;

use chat::{ChatConfig, ChatEngine};
use errors::Result;
use interactive::{Interactive, i18n::Language};
use learning::LearningSystem;
use providers::StreamEvent;

/// Winx - Shell Agent + CLI Chat + MCP Server
#[derive(Parser)]
#[command(name = "winx")]
#[command(author = "Gabriel Maia")]
#[command(version)]
#[command(about = "High-performance shell agent with LLM chat capabilities", long_about = None)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    /// Model to use (format: provider:model or just model)
    #[arg(short, long)]
    model: Option<String>,

    /// Language for TUI (pt or en, auto-detected by default)
    #[arg(short, long)]
    lang: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start MCP server (default mode for Claude Code integration)
    Serve {
        /// Enable debug mode with enhanced error reporting
        #[arg(long)]
        debug_mode: bool,
    },

    /// Send a one-shot message to the LLM
    Chat {
        /// The message to send
        message: String,

        /// Don't stream output
        #[arg(long)]
        no_stream: bool,
    },

    /// Start reedline REPL (simpler mode, no TUI)
    Repl,

    /// List available providers and models
    Providers,

    /// Show or modify configuration
    Config {
        /// Set default provider
        #[arg(long)]
        provider: Option<String>,

        /// Set default model
        #[arg(long)]
        model: Option<String>,
    },

    /// Learn from Claude Code sessions (~/.claude/projects/)
    Learn {
        /// Show detailed analysis
        #[arg(short, long)]
        verbose: bool,
    },
}

/// Configuração de logging
fn setup_logging(verbose: bool, debug: bool) {
    let level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    setup_logging(cli.verbose, cli.debug);

    // Parse language
    let lang = cli.lang.as_ref().map(|l| Language::from_code(l));

    match cli.command {
        // TUI mode (default - Claude Code style)
        None => {
            run_tui()
        }

        // MCP Server mode (for Claude Code integration)
        Some(Commands::Serve { .. }) => {
            run_server().await
        }

        // One-shot chat
        Some(Commands::Chat { message, no_stream }) => {
            run_chat(&message, cli.model, !no_stream).await
        }

        // Reedline REPL (simpler, no TUI)
        Some(Commands::Repl) => {
            run_interactive(cli.model, lang).await
        }

        // List providers
        Some(Commands::Providers) => {
            run_providers()
        }

        // Configuration
        Some(Commands::Config { provider, model }) => {
            run_config(provider, model)
        }

        // Learning from Claude sessions
        Some(Commands::Learn { verbose }) => {
            run_learn(verbose).await
        }
    }
}

/// Executa o MCP server
async fn run_server() -> Result<()> {
    tracing::info!("Starting winx MCP server v{}", env!("CARGO_PKG_VERSION"));

    match server::start_winx_server().await {
        Ok(()) => {
            tracing::info!("Server shutting down normally");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Server error: {}", e);
            Err(errors::WinxError::ShellInitializationError(format!(
                "Failed to start server: {e}"
            )))
        }
    }
}

/// Executa chat one-shot
async fn run_chat(message: &str, model: Option<String>, stream: bool) -> Result<()> {
    let mut config = ChatConfig::sensible_defaults();

    // Aplica modelo se especificado
    if let Some(ref m) = model {
        if let Some((provider, model_name)) = m.split_once(':') {
            config.default_provider = Some(provider.to_string());
            config.default_model = Some(model_name.to_string());
        } else {
            config.default_model = Some(m.clone());
        }
    }

    let engine = ChatEngine::new(config);

    if stream {
        // Streaming output
        let stream = engine.one_shot_stream(message).await?;
        print_stream(stream).await?;
    } else {
        // Blocking output
        let response = engine.one_shot(message).await?;
        println!("{}", response.content);
    }

    Ok(())
}

/// Imprime stream para stdout
async fn print_stream(mut stream: providers::EventStream) -> Result<()> {
    let mut stdout = io::stdout();

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Text(text) => {
                print!("{}", text);
                stdout.flush().ok();
            }
            StreamEvent::Done => {
                println!();
                break;
            }
            StreamEvent::Error(err) => {
                eprintln!("\n\x1b[31mError: {}\x1b[0m", err);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Executa modo interativo (aichat-style)
async fn run_interactive(model: Option<String>, lang: Option<Language>) -> Result<()> {
    let mut config = ChatConfig::sensible_defaults();

    if let Some(ref m) = model {
        if let Some((provider, model_name)) = m.split_once(':') {
            config.default_provider = Some(provider.to_string());
            config.default_model = Some(model_name.to_string());
        } else {
            config.default_model = Some(m.clone());
        }
    }

    let mut interactive = if let Some(language) = lang {
        Interactive::with_language(config, language)?
    } else {
        Interactive::new(config)?
    };

    interactive.run().await
}

/// Lista providers disponíveis
fn run_providers() -> Result<()> {
    let config = ChatConfig::sensible_defaults();
    let engine = ChatEngine::new(config);

    println!("\x1b[1mProviders disponíveis:\x1b[0m");
    println!();

    for provider in engine.list_providers() {
        println!("  \x1b[33m{}\x1b[0m", provider);
    }

    println!();
    println!("\x1b[1mModelos:\x1b[0m");
    println!();

    for (id, desc) in engine.list_models() {
        println!("  \x1b[36m{}\x1b[0m", id);
        println!("    {}", desc);
    }

    Ok(())
}

/// Gerencia configuração
fn run_config(provider: Option<String>, model: Option<String>) -> Result<()> {
    let mut config = chat::load_config()?;

    if provider.is_none() && model.is_none() {
        // Mostra config atual
        println!("\x1b[1mConfiguração atual:\x1b[0m");
        println!();
        println!("  Provider: \x1b[33m{}\x1b[0m",
            config.default_provider.as_deref().unwrap_or("(auto)"));
        println!("  Model: \x1b[33m{}\x1b[0m",
            config.default_model.as_deref().unwrap_or("(auto)"));
        println!("  Temperature: \x1b[33m{}\x1b[0m",
            config.temperature.map(|t| t.to_string()).unwrap_or("(default)".to_string()));
        println!("  Max tokens: \x1b[33m{}\x1b[0m",
            config.max_tokens.map(|t| t.to_string()).unwrap_or("(default)".to_string()));
    } else {
        // Atualiza config
        if let Some(p) = provider {
            config.default_provider = Some(p);
        }
        if let Some(m) = model {
            config.default_model = Some(m);
        }

        chat::save_config(&config)?;
        println!("\x1b[32mConfiguração salva!\x1b[0m");
    }

    Ok(())
}

/// Executa TUI mode (Claude Code style)
fn run_tui() -> Result<()> {
    interactive::tui::run().map_err(|e| {
        errors::WinxError::ShellInitializationError(format!("TUI error: {e}"))
    })
}

/// Executa aprendizado das sessões do Claude Code
async fn run_learn(verbose: bool) -> Result<()> {
    println!("\x1b[36m╔═══════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║\x1b[0m  \x1b[1mWinx Learning\x1b[0m                        \x1b[36m║\x1b[0m");
    println!("\x1b[36m╚═══════════════════════════════════════╝\x1b[0m");
    println!();

    println!("\x1b[90mCarregando sessões de ~/.claude/projects/...\x1b[0m");

    let mut system = LearningSystem::new()?;

    // Processa todas as sessões
    let report = system.process_all_sessions().await?;

    println!();
    println!("\x1b[32m✓ Aprendizado concluído!\x1b[0m");
    println!();

    // Estatísticas gerais
    println!("\x1b[1mEstatísticas:\x1b[0m");
    println!("  Sessões: \x1b[33m{}\x1b[0m", report.total_sessions);
    println!("  Mensagens: \x1b[33m{}\x1b[0m", report.total_messages);
    println!("  Mensagens do usuário: \x1b[33m{}\x1b[0m", report.user_messages);
    println!();

    // Vocabulário (top 10)
    if !report.vocabulary.is_empty() {
        println!("\x1b[1mVocabulário (top 10):\x1b[0m");
        for (word, count) in report.vocabulary.iter().take(10) {
            println!("  \x1b[33m{}\x1b[0m - {}x", word, count);
        }
        println!();
    }

    // Pedidos frequentes
    if !report.frequent_requests.is_empty() {
        println!("\x1b[1mPedidos frequentes:\x1b[0m");
        for req in report.frequent_requests.iter().take(5) {
            println!("  \x1b[36m{}\x1b[0m ({}x)", req.text, req.count);
            if verbose {
                for session in &req.sessions {
                    println!("    \x1b[90m└─ {}\x1b[0m", session);
                }
            }
        }
        println!();
    }

    // Candidatos a automação
    if !report.automation_candidates.is_empty() {
        println!("\x1b[1mCandidatos a automação:\x1b[0m");
        for candidate in &report.automation_candidates {
            println!(
                "  \x1b[32m/{}\x1b[0m - {} ({}x)",
                candidate.suggested_command, candidate.pattern, candidate.frequency
            );
            if verbose && !candidate.examples.is_empty() {
                println!("    Exemplos:");
                for example in candidate.examples.iter().take(2) {
                    println!("      \x1b[90m\"{}\"\x1b[0m", example);
                }
            }
        }
        println!();
    }

    // Padrões de pensamento
    if !report.thinking_patterns.is_empty() && verbose {
        println!("\x1b[1mPadrões de pensamento:\x1b[0m");
        for pattern in &report.thinking_patterns {
            println!("  \x1b[35m{}\x1b[0m - {}", pattern.name, pattern.description);
        }
        println!();
    }

    // Info sobre onde foi salvo
    let learning_dir = learning::default_learning_dir();
    println!("\x1b[90mDados salvos em: {}\x1b[0m", learning_dir.display());

    Ok(())
}
