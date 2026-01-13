//! Winx Agent Module
//!
//! Sistema de agente autônomo com self-awareness, similar ao conceito do VIVA.
//! A Winx "sente" seu ambiente, sabe suas capacidades, e evolui com o usuário.

pub mod identity;
pub mod sense;

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

pub use identity::{Capability, ProviderInfo, SystemInfo, ToolInfo, WinxIdentity};
pub use sense::{DetectedAgent, AgentType, McpServerInfo, ProjectSense, SenseSystem};

use crate::errors::{Result, WinxError};

/// Estado persistente do agente
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    /// Primeira execução já aconteceu?
    pub onboarded: bool,

    /// Nome do usuário
    pub user_name: Option<String>,

    /// Preferências do usuário
    pub preferences: AgentPreferences,

    /// Hardware detectado
    pub hardware: HardwareProfile,

    /// Estatísticas de uso
    pub stats: UsageStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPreferences {
    /// Língua preferida
    pub language: String,

    /// Modelo preferido
    pub preferred_model: Option<String>,

    /// Provider preferido
    pub preferred_provider: Option<String>,

    /// Modo de aprovação (always, dangerous, never)
    pub approval_mode: String,

    /// Verbose output
    pub verbose: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    pub vram_gb: Option<u64>,
    pub ram_gb: Option<u64>,
    pub has_nvidia: bool,
    pub has_cuda: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_sessions: u64,
    pub total_messages: u64,
    pub total_tokens_used: u64,
    pub files_created: u64,
    pub files_edited: u64,
    pub commands_executed: u64,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            onboarded: false,
            user_name: None,
            preferences: AgentPreferences {
                language: "pt-br".to_string(),
                approval_mode: "dangerous".to_string(),
                ..Default::default()
            },
            hardware: HardwareProfile::default(),
            stats: UsageStats::default(),
        }
    }
}

impl AgentState {
    /// Diretório de dados do agente
    pub fn data_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "winx", "winx-agent")
            .map(|dirs| dirs.data_dir().to_path_buf())
    }

    /// Caminho do arquivo de estado
    pub fn state_path() -> Option<PathBuf> {
        Self::data_dir().map(|dir| dir.join("state.json"))
    }

    /// Carrega estado do disco ou cria novo
    pub fn load() -> Self {
        if let Some(path) = Self::state_path() {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(state) = serde_json::from_str(&content) {
                        return state;
                    }
                }
            }
        }
        Self::default()
    }

    /// Salva estado no disco
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()
            .ok_or_else(|| WinxError::ConfigurationError("Could not determine state path".to_string()))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| WinxError::ConfigurationError(format!("Failed to create dir: {}", e)))?;
        }

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| WinxError::ConfigurationError(format!("Failed to serialize state: {}", e)))?;

        fs::write(&path, content)
            .map_err(|e| WinxError::ConfigurationError(format!("Failed to write state: {}", e)))?;

        Ok(())
    }
}

/// O Agente Winx - com consciência e estado
pub struct WinxAgent {
    /// Identidade (quem sou, o que posso fazer)
    pub identity: WinxIdentity,

    /// Estado persistente
    pub state: AgentState,

    /// Sistema de percepção do ambiente
    pub sense: SenseSystem,
}

impl WinxAgent {
    /// Cria novo agente, carregando estado existente
    pub fn new() -> Self {
        let state = AgentState::load();
        let mut identity = WinxIdentity::new();

        // Aplica estado ao identity
        if let Some(ref name) = state.user_name {
            identity.set_user(name);
        }

        // Inicializa sistema de percepção
        let mut sense = SenseSystem::new();
        sense.scan_all();

        Self { identity, state, sense }
    }

    /// Verifica se precisa de onboarding
    pub fn needs_onboarding(&self) -> bool {
        !self.state.onboarded
    }

    /// Executa onboarding interativo
    pub async fn onboard(&mut self) -> Result<OnboardingResult> {
        println!();
        println!("\x1b[36m╔═══════════════════════════════════════════════════════════╗\x1b[0m");
        println!("\x1b[36m║\x1b[0m  \x1b[1m🚀 Bem-vindo ao Winx!\x1b[0m                                    \x1b[36m║\x1b[0m");
        println!("\x1b[36m║\x1b[0m     Vou conhecer seu ambiente pra te ajudar melhor.       \x1b[36m║\x1b[0m");
        println!("\x1b[36m╚═══════════════════════════════════════════════════════════╝\x1b[0m");
        println!();

        // 1. Detectar hardware
        println!("\x1b[90m[1/5] Detectando hardware...\x1b[0m");
        let hardware = self.detect_hardware();
        self.state.hardware = hardware.clone();

        self.report_hardware(&hardware);

        // 2. Detectar providers disponíveis
        println!();
        println!("\x1b[90m[2/5] Verificando providers LLM...\x1b[0m");
        let providers = self.detect_providers().await;

        for provider in &providers {
            let status = if provider.is_default { " \x1b[32m[ATIVO]\x1b[0m" } else { "" };
            let local = if provider.is_local { " (local)" } else { "" };
            println!(
                "  \x1b[33m{}\x1b[0m{}{}: {} modelos",
                provider.name, local, status, provider.models.len()
            );
        }

        // 3. Detectar MCP tools
        println!();
        println!("\x1b[90m[3/5] Verificando tools disponíveis...\x1b[0m");
        println!("  \x1b[90mTools locais:\x1b[0m {}", self.identity.tools.len());

        // 4. Detectar outros agentes e ambiente
        println!();
        println!("\x1b[90m[4/5] Detectando ambiente e outros agentes...\x1b[0m");

        // Refresh sense data
        self.sense.scan_all();

        // Show detected agents
        if !self.sense.agents.is_empty() {
            println!("  \x1b[35mAgentes detectados:\x1b[0m");
            for agent in &self.sense.agents {
                let running = if agent.is_running { " \x1b[32m●\x1b[0m" } else { "" };
                let delegate = if agent.can_delegate { " (posso delegar)" } else { "" };
                println!("    - {}{}{}", agent.name, running, delegate);
            }
        } else {
            println!("  \x1b[90mNenhum outro agente detectado\x1b[0m");
        }

        // Show project info
        if let Some(ref project) = self.sense.project {
            println!();
            println!("  \x1b[36mProjeto atual:\x1b[0m {}", project.name);
            println!("    Linguagem: {:?}", project.language);
            if let Some(ref fw) = project.framework {
                println!("    Framework: {}", fw);
            }
        }

        // Show MCP servers
        if !self.sense.mcp_servers.is_empty() {
            println!();
            println!("  \x1b[33mMCP Servers:\x1b[0m {} configurados", self.sense.mcp_servers.len());
        }

        // 5. Resumo
        println!();
        println!("\x1b[90m[5/5] Pronto!\x1b[0m");
        println!();

        // Marcar como onboarded
        self.state.onboarded = true;
        self.state.save()?;

        // Gerar resultado
        let result = OnboardingResult {
            hardware,
            providers,
            tools_count: self.identity.tools.len(),
        };

        self.print_summary(&result);

        Ok(result)
    }

    /// Detecta hardware disponível
    fn detect_hardware(&self) -> HardwareProfile {
        let system = &self.identity.system;

        let mut profile = HardwareProfile {
            cpu: system.cpu.clone(),
            gpu: system.gpu.clone(),
            ram_gb: system.ram_gb,
            vram_gb: None,
            has_nvidia: system.gpu.is_some(),
            has_cuda: false,
        };

        // Detectar VRAM
        if profile.has_nvidia {
            if let Ok(output) = std::process::Command::new("nvidia-smi")
                .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
                .output()
            {
                if output.status.success() {
                    let vram_str = String::from_utf8_lossy(&output.stdout);
                    if let Ok(vram_mb) = vram_str.trim().parse::<u64>() {
                        profile.vram_gb = Some(vram_mb / 1024);
                    }
                }
            }

            // Check CUDA
            profile.has_cuda = std::process::Command::new("nvcc")
                .arg("--version")
                .output()
                .is_ok();
        }

        profile
    }

    /// Reporta hardware detectado
    fn report_hardware(&self, hw: &HardwareProfile) {
        if let Some(ref cpu) = hw.cpu {
            println!("  \x1b[90mCPU:\x1b[0m {}", cpu);
        }

        if let Some(ref gpu) = hw.gpu {
            let vram = hw.vram_gb.map(|v| format!(" ({}GB VRAM)", v)).unwrap_or_default();
            println!("  \x1b[32mGPU:\x1b[0m {}{}", gpu, vram);

            if hw.has_cuda {
                println!("  \x1b[32mCUDA:\x1b[0m Disponível");
            }
        } else {
            println!("  \x1b[90mGPU:\x1b[0m Não detectada");
        }

        if let Some(ram) = hw.ram_gb {
            println!("  \x1b[90mRAM:\x1b[0m {}GB", ram);
        }
    }

    /// Detecta providers LLM disponíveis
    async fn detect_providers(&self) -> Vec<ProviderInfo> {
        let mut providers = Vec::new();

        // Check NVIDIA NIM
        if std::env::var("NVIDIA_API_KEY").is_ok() {
            providers.push(ProviderInfo {
                name: "nvidia".to_string(),
                provider_type: "nvidia".to_string(),
                models: vec![
                    "qwen3-235b".to_string(),
                    "llama-3.3-70b".to_string(),
                    "phi-4-mini".to_string(),
                ],
                is_local: false,
                is_default: true,
            });
        }

        // Check Ollama
        if let Ok(output) = std::process::Command::new("ollama")
            .arg("list")
            .output()
        {
            if output.status.success() {
                let content = String::from_utf8_lossy(&output.stdout);
                let models: Vec<String> = content
                    .lines()
                    .skip(1) // Skip header
                    .filter_map(|line| line.split_whitespace().next())
                    .map(|s| s.to_string())
                    .collect();

                if !models.is_empty() {
                    providers.push(ProviderInfo {
                        name: "ollama".to_string(),
                        provider_type: "ollama".to_string(),
                        models,
                        is_local: true,
                        is_default: providers.is_empty(),
                    });
                }
            }
        }

        // Check OpenAI
        if std::env::var("OPENAI_API_KEY").is_ok() {
            providers.push(ProviderInfo {
                name: "openai".to_string(),
                provider_type: "openai".to_string(),
                models: vec!["gpt-4".to_string(), "gpt-4o".to_string()],
                is_local: false,
                is_default: providers.is_empty(),
            });
        }

        providers
    }

    /// Imprime resumo final do onboarding
    fn print_summary(&self, result: &OnboardingResult) {
        println!("\x1b[36m┌─────────────────────────────────────────────────────────────┐\x1b[0m");
        println!("\x1b[36m│\x1b[0m  \x1b[1m✨ Winx está pronta!\x1b[0m                                      \x1b[36m│\x1b[0m");
        println!("\x1b[36m├─────────────────────────────────────────────────────────────┤\x1b[0m");

        // Hardware summary
        if let Some(ref gpu) = result.hardware.gpu {
            let vram = result.hardware.vram_gb.unwrap_or(0);
            println!("\x1b[36m│\x1b[0m  GPU: \x1b[32m{} ({}GB)\x1b[0m", gpu, vram);
            if vram >= 24 {
                println!("\x1b[36m│\x1b[0m       Posso rodar modelos grandes localmente!");
            }
        }

        // Providers summary
        let provider_names: Vec<&str> = result.providers.iter().map(|p| p.name.as_str()).collect();
        println!("\x1b[36m│\x1b[0m  Providers: \x1b[33m{}\x1b[0m", provider_names.join(", "));

        // Tools summary
        println!("\x1b[36m│\x1b[0m  Tools: \x1b[90m{} disponíveis\x1b[0m", result.tools_count);

        println!("\x1b[36m├─────────────────────────────────────────────────────────────┤\x1b[0m");
        println!("\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m");
        println!("\x1b[36m│\x1b[0m  Agora sei o que posso fazer neste computador.             \x1b[36m│\x1b[0m");
        println!("\x1b[36m│\x1b[0m  Me conta o que você precisa!                              \x1b[36m│\x1b[0m");
        println!("\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m");
        println!("\x1b[36m└─────────────────────────────────────────────────────────────┘\x1b[0m");
        println!();
    }

    /// Gera system prompt completo baseado no estado atual
    pub fn system_prompt(&self) -> String {
        let mut prompt = self.identity.generate_system_prompt();

        // Adiciona percepção do ambiente
        let sense_summary = self.sense.summarize();
        if !sense_summary.is_empty() {
            prompt.push_str("\n\n# Percepção do Ambiente\n\n");
            prompt.push_str(&sense_summary);
        }

        prompt
    }

    /// Refresh sense data (chama quando contexto pode ter mudado)
    pub fn refresh_sense(&mut self) {
        self.sense.scan_all();
    }

    /// Lista agentes que podem receber delegação
    pub fn delegatable_agents(&self) -> Vec<&DetectedAgent> {
        self.sense.delegatable_agents()
    }

    /// Incrementa estatísticas
    pub fn track_message(&mut self) {
        self.state.stats.total_messages += 1;
    }

    pub fn track_file_created(&mut self) {
        self.state.stats.files_created += 1;
    }

    pub fn track_file_edited(&mut self) {
        self.state.stats.files_edited += 1;
    }

    pub fn track_command(&mut self) {
        self.state.stats.commands_executed += 1;
    }

    /// Salva estado
    pub fn save(&self) -> Result<()> {
        self.state.save()
    }
}

impl Default for WinxAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Resultado do onboarding
#[derive(Debug)]
pub struct OnboardingResult {
    pub hardware: HardwareProfile,
    pub providers: Vec<ProviderInfo>,
    pub tools_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_creation() {
        let agent = WinxAgent::new();
        assert_eq!(agent.identity.name, "Winx");
    }

    #[test]
    fn test_state_default() {
        let state = AgentState::default();
        assert!(!state.onboarded);
        assert_eq!(state.preferences.language, "pt-br");
    }
}
