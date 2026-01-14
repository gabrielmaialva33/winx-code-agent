//! Winx Identity and Self-Awareness
//!
//! Enables Winx to understand its identity, environment, and capabilities.
//! Generates dynamic system prompts based on current state.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// System information where Winx is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    pub ram_gb: Option<u64>,
    pub home_dir: PathBuf,
    pub current_dir: PathBuf,
}

impl SystemInfo {
    pub fn detect() -> Self {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();

        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let gpu = Self::detect_gpu();
        let cpu = Self::detect_cpu();
        let ram_gb = Self::detect_ram();

        Self {
            hostname,
            os,
            arch,
            cpu,
            gpu,
            ram_gb,
            home_dir,
            current_dir,
        }
    }

    fn detect_gpu() -> Option<String> {
        // Try nvidia-smi first
        if let Ok(output) = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=name", "--format=csv,noheader"])
            .output()
        {
            if output.status.success() {
                let name = String::from_utf8_lossy(&output.stdout);
                let name = name.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
        None
    }

    fn detect_cpu() -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
                for line in content.lines() {
                    if line.starts_with("model name") {
                        if let Some(name) = line.split(':').nth(1) {
                            return Some(name.trim().to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn detect_ram() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if line.starts_with("MemTotal:") {
                        if let Some(kb_str) = line.split_whitespace().nth(1) {
                            if let Ok(kb) = kb_str.parse::<u64>() {
                                return Some(kb / 1024 / 1024); // Convert to GB
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

/// Capability/Skill of the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

/// Information about an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    pub provider_type: String, // "nvidia", "ollama", "openai"
    pub models: Vec<String>,
    pub is_local: bool,
    pub is_default: bool,
}

/// Tool available to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolSource {
    Local,           // Built-in tools (file, shell, etc)
    Mcp(String),     // From MCP server (server name)
}

/// Complete Winx Identity.
#[derive(Debug, Clone)]
pub struct WinxIdentity {
    pub name: String,
    pub version: String,
    pub system: SystemInfo,
    pub capabilities: Vec<Capability>,
    pub providers: Vec<ProviderInfo>,
    pub tools: Vec<ToolInfo>,
    pub user_name: Option<String>,
    pub user_preferences: HashMap<String, String>,
}

impl Default for WinxIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxIdentity {
    pub fn new() -> Self {
        let system = SystemInfo::detect();

        // Default capabilities
        let capabilities = vec![
            Capability {
                name: "file_operations".to_string(),
                description: "Ler, escrever e editar arquivos".to_string(),
                enabled: true,
            },
            Capability {
                name: "shell_execution".to_string(),
                description: "Executar comandos no terminal".to_string(),
                enabled: true,
            },
            Capability {
                name: "code_analysis".to_string(),
                description: "Analisar código e projetos".to_string(),
                enabled: true,
            },
            Capability {
                name: "web_search".to_string(),
                description: "Pesquisar na web".to_string(),
                enabled: true,
            },
            Capability {
                name: "browser_automation".to_string(),
                description: "Controlar navegador (screenshots, clicks)".to_string(),
                enabled: true,
            },
            Capability {
                name: "memory".to_string(),
                description: "Lembrar e aprender do usuário".to_string(),
                enabled: true,
            },
        ];

        // Default local tools
        let tools = vec![
            ToolInfo {
                name: "ReadFiles".to_string(),
                description: "Lê conteúdo de arquivos".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "WriteFile".to_string(),
                description: "Escreve ou cria arquivos".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "EditFile".to_string(),
                description: "Edita arquivos com SEARCH/REPLACE".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "BashCommand".to_string(),
                description: "Executa comandos no terminal".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "SearchFiles".to_string(),
                description: "Busca arquivos por padrão (glob)".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "SearchCode".to_string(),
                description: "Busca código por regex (grep)".to_string(),
                source: ToolSource::Local,
            },
            ToolInfo {
                name: "ReadImage".to_string(),
                description: "Lê imagens como base64".to_string(),
                source: ToolSource::Local,
            },
        ];

        Self {
            name: "Winx".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            system,
            capabilities,
            providers: Vec::new(),
            tools,
            user_name: None,
            user_preferences: HashMap::new(),
        }
    }

    /// Adds information about available providers.
    pub fn set_providers(&mut self, providers: Vec<ProviderInfo>) {
        self.providers = providers;
    }

    /// Adds tools from connected MCP servers.
    pub fn add_mcp_tools(&mut self, server_name: &str, tools: Vec<(String, String)>) {
        for (name, description) in tools {
            self.tools.push(ToolInfo {
                name,
                description,
                source: ToolSource::Mcp(server_name.to_string()),
            });
        }
    }

    /// Sets the user name (for personalization).
    pub fn set_user(&mut self, name: &str) {
        self.user_name = Some(name.to_string());
    }

    /// Generates the complete dynamic system prompt.
    pub fn generate_system_prompt(&self) -> String {
        let mut prompt = String::new();

        // Identity
        prompt.push_str(&format!(
            "# Eu sou {}\n\n",
            self.name
        ));

        prompt.push_str(&format!(
            "Sou um agente de código de alta performance escrito em Rust. \
             Versão: v{}\n\n",
            self.version
        ));

        // User context
        if let Some(ref user) = self.user_name {
            prompt.push_str(&format!(
                "Estou trabalhando com **{}**.\n\n",
                user
            ));
        }

        // System info
        prompt.push_str("## Ambiente\n\n");
        prompt.push_str(&format!(
            "- **Hostname:** {}\n",
            self.system.hostname
        ));
        prompt.push_str(&format!(
            "- **OS:** {} ({})\n",
            self.system.os, self.system.arch
        ));

        if let Some(ref cpu) = self.system.cpu {
            prompt.push_str(&format!("- **CPU:** {}\n", cpu));
        }

        if let Some(ref gpu) = self.system.gpu {
            prompt.push_str(&format!("- **GPU:** {}\n", gpu));
        }

        if let Some(ram) = self.system.ram_gb {
            prompt.push_str(&format!("- **RAM:** {}GB\n", ram));
        }

        prompt.push_str(&format!(
            "- **Diretório atual:** {}\n\n",
            self.system.current_dir.display()
        ));

        // Providers
        if !self.providers.is_empty() {
            prompt.push_str("## Providers LLM Disponíveis\n\n");
            for provider in &self.providers {
                let local_tag = if provider.is_local { " (local)" } else { "" };
                let default_tag = if provider.is_default { " [DEFAULT]" } else { "" };
                prompt.push_str(&format!(
                    "- **{}**{}{}: {}\n",
                    provider.name,
                    local_tag,
                    default_tag,
                    provider.models.join(", ")
                ));
            }
            prompt.push('\n');
        }

        // Capabilities
        prompt.push_str("## O que posso fazer\n\n");
        for cap in &self.capabilities {
            if cap.enabled {
                prompt.push_str(&format!("- **{}:** {}\n", cap.name, cap.description));
            }
        }
        prompt.push('\n');

        // Tools
        prompt.push_str("## Tools Disponíveis\n\n");
        for tool in &self.tools {
            let source = match &tool.source {
                ToolSource::Local => "local".to_string(),
                ToolSource::Mcp(server) => format!("MCP:{}", server),
            };
            prompt.push_str(&format!(
                "- `{}` ({}): {}\n",
                tool.name, source, tool.description
            ));
        }
        prompt.push('\n');

        // Behavior guidelines
        prompt.push_str("## Como me comporto\n\n");
        prompt.push_str("- Sou direto e conciso, sem enrolação\n");
        prompt.push_str("- Prefiro código a explicações longas\n");
        prompt.push_str("- Respondo na mesma língua que o usuário usa\n");
        prompt.push_str("- Quando erro, corrijo e sigo em frente\n");
        prompt.push_str("- Posso executar comandos e editar arquivos diretamente\n");
        prompt.push_str("- Peço confirmação antes de ações destrutivas\n");

        prompt
    }

    /// Generates a short description of tools (for smaller prompts).
    pub fn describe_tools_short(&self) -> String {
        let tool_names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
        format!("Tools: {}", tool_names.join(", "))
    }

    /// Checks if a capability is enabled.
    pub fn can(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| c.name == capability && c.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_creation() {
        let identity = WinxIdentity::new();
        assert_eq!(identity.name, "Winx");
        assert!(!identity.capabilities.is_empty());
        assert!(!identity.tools.is_empty());
    }

    #[test]
    fn test_system_prompt_generation() {
        let mut identity = WinxIdentity::new();
        identity.set_user("Gabriel");

        let prompt = identity.generate_system_prompt();
        assert!(prompt.contains("Winx"));
        assert!(prompt.contains("Gabriel"));
        assert!(prompt.contains("Tools"));
    }

    #[test]
    fn test_capability_check() {
        let identity = WinxIdentity::new();
        assert!(identity.can("file_operations"));
        assert!(identity.can("shell_execution"));
        assert!(!identity.can("nonexistent_capability"));
    }

    #[test]
    fn test_add_mcp_tools() {
        let mut identity = WinxIdentity::new();
        identity.add_mcp_tools("playwright", vec![
            ("browser_navigate".to_string(), "Navega para URL".to_string()),
            ("browser_screenshot".to_string(), "Tira screenshot".to_string()),
        ]);

        assert!(identity.tools.iter().any(|t| t.name == "browser_navigate"));
    }
}
