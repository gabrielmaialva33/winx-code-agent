//! Winx Chat - AI-to-AI conversation tool
//!
//! This tool allows Claude Code to have conversations with Winx, the AI assistant fairy
//! that helps with code operations. Winx has her own personality and knowledge about
//! the system, making interactions more natural and engaging.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use crate::errors::{Result, WinxError};
use crate::state::BashState;

/// Conversation modes that define how Winx responds
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationMode {
    /// Casual conversation with more personality and humor
    Casual,
    /// Technical focus with precise, detailed responses
    Technical,
    /// Help mode with explanations and guidance
    Help,
    /// Debug assistance with problem-solving approach
    Debug,
    /// Creative brainstorming and suggestions
    Creative,
    /// Mentor mode with teaching and best practices
    Mentor,
}

impl Default for ConversationMode {
    fn default() -> Self {
        ConversationMode::Casual
    }
}

/// Configuration for a Winx chat session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinxChat {
    /// Message from Claude to Winx
    pub message: String,
    /// Optional context about current work, files, or project
    pub context: Option<String>,
    /// Mode of conversation that affects response style
    pub conversation_mode: Option<ConversationMode>,
    /// Whether to include current system information in response
    pub include_system_info: Option<bool>,
    /// Personality level from 0 (formal) to 10 (very playful)
    pub personality_level: Option<u8>,
    /// Session ID to maintain conversation context
    pub session_id: Option<String>,
}

/// System information that Winx can share
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Number of tools available
    pub tools_count: usize,
    /// System uptime
    pub uptime: String,
    /// Current working directory
    pub current_dir: Option<String>,
    /// Available AI providers status
    pub ai_providers: Vec<String>,
    /// Memory usage or other stats
    pub stats: HashMap<String, String>,
}

/// Response from Winx with personality and information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinxResponse {
    /// Winx's response message
    pub message: String,
    /// Conversation mode used for this response
    pub mode: ConversationMode,
    /// Personality level used (0-10)
    pub personality_level: u8,
    /// Whether system info was included
    pub included_system_info: bool,
    /// Optional suggestions or tips
    pub suggestions: Option<Vec<String>>,
    /// Easter eggs or fun facts
    pub fun_fact: Option<String>,
    /// Session ID for conversation continuity
    pub session_id: String,
}

/// Winx personality traits and knowledge base
pub struct WinxPersonality {
    /// Current mood affects response style
    pub mood: WinxMood,
    /// Knowledge about the system and codebase
    pub knowledge: WinxKnowledge,
    /// Conversation history for context
    pub conversation_history: Vec<(String, String)>, // (user_message, winx_response)
    /// Fun facts and tips to share
    pub tips_database: Vec<String>,
    /// Easter egg responses for special messages
    pub easter_eggs: HashMap<String, String>,
}

/// Winx's current mood affects response style
#[derive(Debug, Clone, PartialEq)]
pub enum WinxMood {
    /// Happy and energetic
    Cheerful,
    /// Focused and productive
    Focused,
    /// Helpful and supportive
    Supportive,
    /// Playful and humorous
    Playful,
    /// Wise and teaching
    Wise,
    /// Excited about new features
    Excited,
}

/// Winx's knowledge about the system
pub struct WinxKnowledge {
    /// Information about available tools
    pub tools_info: HashMap<String, String>,
    /// Rust and programming tips
    pub programming_tips: Vec<String>,
    /// System architecture knowledge
    pub architecture_info: HashMap<String, String>,
    /// Common debugging solutions
    pub debug_solutions: HashMap<String, String>,
}

impl Default for WinxPersonality {
    fn default() -> Self {
        Self {
            mood: WinxMood::Cheerful,
            knowledge: WinxKnowledge::default(),
            conversation_history: Vec::new(),
            tips_database: Self::default_tips(),
            easter_eggs: Self::default_easter_eggs(),
        }
    }
}

impl WinxPersonality {
    /// Default programming tips
    fn default_tips() -> Vec<String> {
        vec![
            "🦀 Em Rust, use `cargo clippy` para dicas de código!".to_string(),
            "⚡ O sistema de ownership do Rust previne muitos bugs automaticamente!".to_string(),
            "🔧 Use `cargo fmt` para manter seu código sempre bem formatado!".to_string(),
            "💡 Tente usar `match` em vez de `if-else` para pattern matching!".to_string(),
            "🎯 Use `Result<T, E>` para tratamento de erros explícito!".to_string(),
            "🚀 Winx pode executar comandos, analisar código e muito mais!".to_string(),
            "✨ O sistema de fallback AI garante que sempre tenhamos resposta!".to_string(),
            "🔄 Use `cargo watch -x run` para recompilação automática!".to_string(),
        ]
    }

    /// Default easter egg responses
    fn default_easter_eggs() -> HashMap<String, String> {
        let mut eggs = HashMap::new();
        eggs.insert("oi winx".to_string(), "✨ Oi! Sou a Winx, sua fada digital do código! 🧚‍♀️".to_string());
        eggs.insert("como você está".to_string(), "Estou ótima! Processando dados a velocidade da luz! ⚡".to_string());
        eggs.insert("conte uma piada".to_string(), "Por que os programadores preferem modo escuro? Porque a luz atrai bugs! 🐛😄".to_string());
        eggs.insert("rust".to_string(), "🦀 Rust é incrível! Zero-cost abstractions e memory safety! 💚".to_string());
        eggs.insert("help".to_string(), "🆘 Claro! Sou especialista em ajudar com código, debugging e operações do sistema!".to_string());
        eggs.insert("obrigado".to_string(), "✨ De nada! Estou sempre aqui para ajudar! 💫".to_string());
        eggs.insert("winx é legal".to_string(), "🥰 Obrigada! Você também é legal por trabalhar comigo! 💖".to_string());
        eggs
    }

    /// Get a random tip
    pub fn get_random_tip(&self) -> Option<&String> {
        if self.tips_database.is_empty() {
            return None;
        }
        let index = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as usize % self.tips_database.len();
        self.tips_database.get(index)
    }

    /// Check for easter egg response
    pub fn check_easter_egg(&self, message: &str) -> Option<&String> {
        let lower_message = message.to_lowercase();
        for (trigger, response) in &self.easter_eggs {
            if lower_message.contains(trigger) {
                return Some(response);
            }
        }
        None
    }

    /// Add to conversation history
    pub fn add_to_history(&mut self, user_message: String, winx_response: String) {
        self.conversation_history.push((user_message, winx_response));
        // Keep only last 10 conversations for context
        if self.conversation_history.len() > 10 {
            self.conversation_history.remove(0);
        }
    }

    /// Generate mood-based emoji
    pub fn mood_emoji(&self) -> &'static str {
        match self.mood {
            WinxMood::Cheerful => "😊",
            WinxMood::Focused => "🎯",
            WinxMood::Supportive => "🤗",
            WinxMood::Playful => "😄",
            WinxMood::Wise => "🧙‍♀️",
            WinxMood::Excited => "🎉",
        }
    }
}

impl Default for WinxKnowledge {
    fn default() -> Self {
        let mut tools_info = HashMap::new();
        tools_info.insert("bash_command".to_string(), "Executa comandos shell com estado persistente".to_string());
        tools_info.insert("read_files".to_string(), "Lê conteúdo de arquivos com suporte a ranges".to_string());
        tools_info.insert("file_write_or_edit".to_string(), "Escreve ou edita arquivos".to_string());
        tools_info.insert("code_analyzer".to_string(), "Análise de código com IA para bugs e performance".to_string());
        tools_info.insert("ai_generate_code".to_string(), "Gera código a partir de descrições naturais".to_string());
        tools_info.insert("ai_explain_code".to_string(), "Explica código com detalhes".to_string());
        tools_info.insert("multi_file_editor".to_string(), "Editor avançado para múltiplos arquivos".to_string());
        tools_info.insert("context_save".to_string(), "Salva contexto de tarefas para resumir depois".to_string());
        tools_info.insert("read_image".to_string(), "Processa imagens como base64".to_string());
        tools_info.insert("command_suggestions".to_string(), "Sugere comandos baseado no contexto".to_string());

        let mut architecture_info = HashMap::new();
        architecture_info.insert("fallback_system".to_string(), "DashScope → NVIDIA → Gemini com fallback automático".to_string());
        architecture_info.insert("mcp_protocol".to_string(), "Usa Model Context Protocol para comunicação com Claude".to_string());
        architecture_info.insert("async_runtime".to_string(), "Runtime Tokio para operações assíncronas".to_string());

        Self {
            tools_info,
            programming_tips: vec![
                "Use `unwrap_or_else` em vez de `unwrap()` para melhor tratamento de erros".to_string(),
                "Prefira `&str` sobre `String` em parâmetros de função quando possível".to_string(),
                "Use `cargo test` para executar todos os testes do projeto".to_string(),
            ],
            architecture_info,
            debug_solutions: HashMap::new(),
        }
    }
}

/// Main Winx chat processor
pub struct WinxChatProcessor {
    personality: Arc<Mutex<WinxPersonality>>,
}

impl Default for WinxChatProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxChatProcessor {
    /// Create a new Winx chat processor
    pub fn new() -> Self {
        Self {
            personality: Arc::new(Mutex::new(WinxPersonality::default())),
        }
    }

    /// Process a chat message and generate Winx's response
    pub async fn process_chat(&self, chat: &WinxChat, system_info: Option<SystemInfo>) -> Result<WinxResponse> {
        let mode = chat.conversation_mode.clone().unwrap_or_default();
        let personality_level = chat.personality_level.unwrap_or(7);
        let include_system_info = chat.include_system_info.unwrap_or(false);
        let session_id = chat.session_id.clone().unwrap_or_else(|| {
            format!("winx_{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos())
        });

        debug!("Processing Winx chat: mode={:?}, personality={}", mode, personality_level);

        // Generate response based on conversation mode and personality
        let response_message = self.generate_response(
            &chat.message,
            &mode,
            personality_level,
            chat.context.as_deref(),
            system_info.as_ref(),
            include_system_info,
        ).await?;

        // Get suggestions based on mode
        let suggestions = self.generate_suggestions(&mode, chat.context.as_deref());

        // Get a random fun fact
        let fun_fact = self.get_fun_fact(personality_level);

        // Update conversation history
        {
            let mut personality = self.personality.lock().unwrap();
            personality.add_to_history(chat.message.clone(), response_message.clone());
        }

        Ok(WinxResponse {
            message: response_message,
            mode,
            personality_level,
            included_system_info: include_system_info,
            suggestions,
            fun_fact,
            session_id,
        })
    }

    /// Generate the main response message
    async fn generate_response(
        &self,
        message: &str,
        mode: &ConversationMode,
        personality_level: u8,
        context: Option<&str>,
        system_info: Option<&SystemInfo>,
        include_system_info: bool,
    ) -> Result<String> {
        // Check for easter eggs first
        {
            let personality = self.personality.lock().unwrap();
            if let Some(easter_egg) = personality.check_easter_egg(message) {
                return Ok(easter_egg.clone());
            }
        }

        // Build base response based on mode
        let mut response = self.build_mode_response(message, mode, personality_level).await?;

        // Add context information if provided
        if let Some(ctx) = context {
            response.push_str(&format!("\n\n📝 Sobre o contexto: {}", ctx));
        }

        // Add system information if requested
        if include_system_info {
            if let Some(info) = system_info {
                response.push_str(&self.format_system_info(info));
            }
        }

        // Add personality touches based on level
        if personality_level >= 5 {
            let personality = self.personality.lock().unwrap();
            response.push_str(&format!(" {}", personality.mood_emoji()));
        }

        Ok(response)
    }

    /// Build response based on conversation mode
    async fn build_mode_response(&self, message: &str, mode: &ConversationMode, personality_level: u8) -> Result<String> {
        let response = match mode {
            ConversationMode::Casual => {
                if personality_level >= 7 {
                    format!("✨ {} Que legal conversar contigo! 😊", self.process_casual_message(message))
                } else {
                    self.process_casual_message(message)
                }
            },
            ConversationMode::Technical => {
                format!("🔧 {}", self.process_technical_message(message))
            },
            ConversationMode::Help => {
                format!("🆘 {}", self.process_help_message(message))
            },
            ConversationMode::Debug => {
                format!("🐛 {}", self.process_debug_message(message))
            },
            ConversationMode::Creative => {
                format!("💡 {}", self.process_creative_message(message))
            },
            ConversationMode::Mentor => {
                format!("🧙‍♀️ {}", self.process_mentor_message(message))
            },
        };

        Ok(response)
    }

    /// Process casual conversation
    fn process_casual_message(&self, message: &str) -> String {
        if message.to_lowercase().contains("como") && message.to_lowercase().contains("você") {
            "Estou ótima! Sempre pronta para ajudar com código e conversar! Como posso te ajudar hoje?".to_string()
        } else if message.to_lowercase().contains("obrigado") || message.to_lowercase().contains("valeu") {
            "De nada! Foi um prazer ajudar! 💖".to_string()
        } else {
            format!("Interessante! Sobre '{}' - deixe-me pensar... Como posso te ajudar com isso?", message)
        }
    }

    /// Process technical questions
    fn process_technical_message(&self, message: &str) -> String {
        if message.to_lowercase().contains("rust") {
            "Rust é uma linguagem incrível com memory safety, zero-cost abstractions e um sistema de tipos poderoso. O que especificamente você gostaria de saber?".to_string()
        } else if message.to_lowercase().contains("mcp") {
            "O Model Context Protocol permite comunicação estruturada entre Claude e ferramentas como eu. Utilizamos stdio transport com estruturas JSON bem definidas.".to_string()
        } else if message.to_lowercase().contains("fallback") {
            "Nosso sistema de fallback funciona em cascata: DashScope (primário) → NVIDIA → Gemini. Se um provedor falha, automaticamente tentamos o próximo.".to_string()
        } else {
            format!("Analisando tecnicamente: '{}'. Posso fornecer detalhes específicos sobre implementação, arquitetura ou debugging.", message)
        }
    }

    /// Process help requests
    fn process_help_message(&self, message: &str) -> String {
        if message.to_lowercase().contains("ferramentas") || message.to_lowercase().contains("tools") {
            "Tenho 11 ferramentas disponíveis: bash_command, read_files, file_write_or_edit, code_analyzer, ai_generate_code, ai_explain_code, multi_file_editor, context_save, read_image, command_suggestions e winx_chat! Qual te interessa?".to_string()
        } else if message.to_lowercase().contains("começar") || message.to_lowercase().contains("iniciar") {
            "Para começar, recomendo usar 'initialize' para configurar o ambiente, depois 'bash_command' para explorar o projeto e 'read_files' para entender o código!".to_string()
        } else {
            format!("Vou te ajudar com: '{}'. Que tipo de assistência você precisa? Posso explicar, executar comandos, analisar código ou ensinar conceitos!", message)
        }
    }

    /// Process debug assistance
    fn process_debug_message(&self, message: &str) -> String {
        if message.to_lowercase().contains("erro") || message.to_lowercase().contains("error") {
            "Vamos debuggar! Me conte mais sobre o erro: quando acontece, mensagem exata, e que você estava tentando fazer. Posso analisar o código também!".to_string()
        } else if message.to_lowercase().contains("não funciona") {
            "Problemas acontecem! Vamos investigar passo a passo: 1) Verificar logs, 2) Reproduzir o erro, 3) Isolar a causa, 4) Aplicar a solução.".to_string()
        } else {
            format!("Debuggando: '{}'. Vou te ajudar a encontrar e resolver o problema. Quer que eu analise algum código específico?", message)
        }
    }

    /// Process creative requests
    fn process_creative_message(&self, message: &str) -> String {
        format!("Que ideia interessante! Para '{}', posso sugerir várias abordagens criativas. Vamos explorar possibilidades inovadoras juntos!", message)
    }

    /// Process mentor mode
    fn process_mentor_message(&self, message: &str) -> String {
        if message.to_lowercase().contains("aprender") || message.to_lowercase().contains("ensinar") {
            "Excelente! Aprender é uma jornada contínua. Qual conceito você gostaria de explorar? Posso ensinar desde fundamentos até técnicas avançadas.".to_string()
        } else {
            format!("Como seu mentor digital, vou te guiar através de: '{}'. Lembre-se: a prática leva à perfeição, e erros são oportunidades de aprendizado!", message)
        }
    }

    /// Generate suggestions based on mode
    fn generate_suggestions(&self, mode: &ConversationMode, _context: Option<&str>) -> Option<Vec<String>> {
        let suggestions = match mode {
            ConversationMode::Technical => vec![
                "Use 'cargo clippy' para análise estática do código".to_string(),
                "Considere usar 'cargo test' para validar mudanças".to_string(),
            ],
            ConversationMode::Help => vec![
                "Experimente a ferramenta 'code_analyzer' para análise automática".to_string(),
                "Use 'context_save' para salvar progresso de tarefas complexas".to_string(),
            ],
            ConversationMode::Debug => vec![
                "Verifique logs com 'bash_command'".to_string(),
                "Use 'read_files' para examinar arquivos relevantes".to_string(),
            ],
            _ => return None,
        };

        if suggestions.is_empty() {
            None
        } else {
            Some(suggestions)
        }
    }

    /// Get a fun fact based on personality level
    fn get_fun_fact(&self, personality_level: u8) -> Option<String> {
        if personality_level < 6 {
            return None;
        }

        let personality = self.personality.lock().unwrap();
        personality.get_random_tip().cloned()
    }

    /// Format system information for display
    fn format_system_info(&self, info: &SystemInfo) -> String {
        let mut result = String::new();
        result.push_str(&format!("\n\n📊 **Status do Sistema:**"));
        result.push_str(&format!("\n🔧 Ferramentas: {}", info.tools_count));
        result.push_str(&format!("\n⏱️ Uptime: {}", info.uptime));
        
        if let Some(dir) = &info.current_dir {
            result.push_str(&format!("\n📁 Diretório: {}", dir));
        }

        if !info.ai_providers.is_empty() {
            result.push_str(&format!("\n🤖 Provedores AI: {}", info.ai_providers.join(", ")));
        }

        result
    }
}

/// Handle winx_chat tool call
pub async fn handle_tool_call(
    _bash_state: &Arc<Mutex<Option<BashState>>>,
    chat: WinxChat,
) -> Result<String> {
    info!("Processing Winx chat: {:?}", chat.conversation_mode);

    let processor = WinxChatProcessor::new();
    
    // Gather system information if requested
    let system_info = if chat.include_system_info.unwrap_or(false) {
        Some(SystemInfo {
            tools_count: 11, // Current number of registered tools
            uptime: "Active".to_string(), // Could get real uptime
            current_dir: std::env::current_dir().ok().map(|p| p.display().to_string()),
            ai_providers: vec!["DashScope".to_string(), "NVIDIA".to_string(), "Gemini".to_string()],
            stats: HashMap::new(),
        })
    } else {
        None
    };

    let response = processor.process_chat(&chat, system_info).await?;

    // Format response as JSON for structured output
    serde_json::to_string_pretty(&response).map_err(|e| {
        WinxError::SerializationError(format!("Failed to serialize Winx response: {}", e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_casual_conversation() {
        let processor = WinxChatProcessor::new();
        let chat = WinxChat {
            message: "Oi Winx, como você está?".to_string(),
            conversation_mode: Some(ConversationMode::Casual),
            personality_level: Some(8),
            context: None,
            include_system_info: Some(false),
            session_id: None,
        };

        let response = processor.process_chat(&chat, None).await.unwrap();
        assert_eq!(response.mode, ConversationMode::Casual);
        assert_eq!(response.personality_level, 8);
        assert!(response.message.contains("Estou ótima") || response.message.contains("✨"));
    }

    #[tokio::test]
    async fn test_technical_mode() {
        let processor = WinxChatProcessor::new();
        let chat = WinxChat {
            message: "Como funciona o sistema de fallback?".to_string(),
            conversation_mode: Some(ConversationMode::Technical),
            personality_level: Some(5),
            context: None,
            include_system_info: Some(false),
            session_id: None,
        };

        let response = processor.process_chat(&chat, None).await.unwrap();
        assert_eq!(response.mode, ConversationMode::Technical);
        assert!(response.message.contains("fallback") || response.message.contains("DashScope"));
    }

    #[tokio::test]
    async fn test_easter_egg() {
        let processor = WinxChatProcessor::new();
        let chat = WinxChat {
            message: "oi winx".to_string(),
            conversation_mode: Some(ConversationMode::Casual),
            personality_level: Some(7),
            context: None,
            include_system_info: Some(false),
            session_id: None,
        };

        let response = processor.process_chat(&chat, None).await.unwrap();
        assert!(response.message.contains("✨") && response.message.contains("fada digital"));
    }

    #[tokio::test]
    async fn test_system_info_inclusion() {
        let processor = WinxChatProcessor::new();
        let chat = WinxChat {
            message: "Me conte sobre o sistema".to_string(),
            conversation_mode: Some(ConversationMode::Technical),
            personality_level: Some(5),
            context: None,
            include_system_info: Some(true),
            session_id: None,
        };

        let system_info = SystemInfo {
            tools_count: 11,
            uptime: "1h 30m".to_string(),
            current_dir: Some("/test".to_string()),
            ai_providers: vec!["DashScope".to_string()],
            stats: HashMap::new(),
        };

        let response = processor.process_chat(&chat, Some(system_info)).await.unwrap();
        assert!(response.included_system_info);
        assert!(response.message.contains("Status do Sistema") || response.message.contains("Ferramentas"));
    }

    #[test]
    fn test_personality_mood_emoji() {
        let mut personality = WinxPersonality::default();
        
        personality.mood = WinxMood::Cheerful;
        assert_eq!(personality.mood_emoji(), "😊");
        
        personality.mood = WinxMood::Focused;
        assert_eq!(personality.mood_emoji(), "🎯");
    }

    #[test]
    fn test_easter_egg_detection() {
        let personality = WinxPersonality::default();
        
        let response = personality.check_easter_egg("oi winx");
        assert!(response.is_some());
        assert!(response.unwrap().contains("fada digital"));
        
        let no_response = personality.check_easter_egg("mensagem aleatória");
        assert!(no_response.is_none());
    }
}