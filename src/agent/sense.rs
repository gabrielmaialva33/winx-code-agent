//! Winx Sense System
//!
//! Percepção do ambiente: hardware, projeto, usuário, e OUTROS AGENTES.
//! A Winx sabe quem mais está no PC e pode colaborar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Outros agentes de IA detectados no sistema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub name: String,
    pub agent_type: AgentType,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub is_running: bool,
    pub can_delegate: bool, // Winx pode delegar tarefas pra ele
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentType {
    Claude,     // Claude Code
    Gemini,     // gemini-cli
    Cline,      // Cline VSCode extension
    Cursor,     // Cursor IDE
    Aider,      // Aider CLI
    Copilot,    // GitHub Copilot
    Custom(String),
}

/// Sistema de percepção do ambiente
#[derive(Debug, Clone, Default)]
pub struct SenseSystem {
    pub agents: Vec<DetectedAgent>,
    pub mcp_servers: Vec<McpServerInfo>,
    pub project: Option<ProjectSense>,
    pub user_context: UserContext,
}

/// Informação sobre MCP server detectado
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: String, // stdio, sse, http
    pub tools_count: usize,
    pub is_running: bool,
}

/// Informações do projeto atual
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSense {
    pub root: PathBuf,
    pub name: String,
    pub language: ProjectLanguage,
    pub framework: Option<String>,
    pub has_git: bool,
    pub has_tests: bool,
    pub build_status: Option<BuildStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProjectLanguage {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Elixir,
    Ruby,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BuildStatus {
    Ok,
    Errors(usize),
    Warnings(usize),
    NotBuilt,
}

/// Contexto do usuário (horário, padrões, etc)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserContext {
    pub is_late_night: bool,      // Madrugada (00-06h)
    pub working_hours: u64,        // Horas trabalhando na sessão
    pub frustration_level: u8,    // 0-100 baseado em padrões de texto
    pub preferred_language: String,
}

impl SenseSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Escaneia o sistema completo
    pub fn scan_all(&mut self) {
        self.detect_agents();
        self.detect_mcp_servers();
        self.detect_project();
        self.sense_user_context();
    }

    /// Detecta outros agentes de IA instalados
    pub fn detect_agents(&mut self) {
        self.agents.clear();

        // Claude Code
        if let Some(agent) = Self::detect_claude_code() {
            self.agents.push(agent);
        }

        // gemini-cli
        if let Some(agent) = Self::detect_gemini_cli() {
            self.agents.push(agent);
        }

        // Aider
        if let Some(agent) = Self::detect_aider() {
            self.agents.push(agent);
        }

        // Cline (check VSCode extensions)
        if let Some(agent) = Self::detect_cline() {
            self.agents.push(agent);
        }

        // Cursor
        if let Some(agent) = Self::detect_cursor() {
            self.agents.push(agent);
        }
    }

    fn detect_claude_code() -> Option<DetectedAgent> {
        // Check if claude command exists
        if let Ok(output) = Command::new("which").arg("claude").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                let path = path.trim();

                // Get version
                let version = Command::new("claude")
                    .arg("--version")
                    .output()
                    .ok()
                    .and_then(|o| {
                        if o.status.success() {
                            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                        } else {
                            None
                        }
                    });

                // Check if running (ps aux | grep claude)
                let is_running = Command::new("pgrep")
                    .args(["-f", "claude"])
                    .output()
                    .is_ok_and(|o| o.status.success());

                return Some(DetectedAgent {
                    name: "Claude Code".to_string(),
                    agent_type: AgentType::Claude,
                    path: Some(PathBuf::from(path)),
                    version,
                    is_running,
                    can_delegate: true, // Winx pode chamar claude via CLI
                });
            }
        }
        None
    }

    fn detect_gemini_cli() -> Option<DetectedAgent> {
        // Check if gemini command exists
        if let Ok(output) = Command::new("which").arg("gemini").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                let path = path.trim();

                return Some(DetectedAgent {
                    name: "Gemini CLI".to_string(),
                    agent_type: AgentType::Gemini,
                    path: Some(PathBuf::from(path)),
                    version: None,
                    is_running: false, // gemini-cli é one-shot
                    can_delegate: true,
                });
            }
        }
        None
    }

    fn detect_aider() -> Option<DetectedAgent> {
        if let Ok(output) = Command::new("which").arg("aider").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                let path = path.trim();

                return Some(DetectedAgent {
                    name: "Aider".to_string(),
                    agent_type: AgentType::Aider,
                    path: Some(PathBuf::from(path)),
                    version: None,
                    is_running: false,
                    can_delegate: true,
                });
            }
        }
        None
    }

    fn detect_cline() -> Option<DetectedAgent> {
        // Check VSCode extensions
        let home = dirs::home_dir()?;
        let vscode_ext = home.join(".vscode/extensions");

        if vscode_ext.exists() {
            if let Ok(entries) = std::fs::read_dir(&vscode_ext) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.contains("saoudrizwan.claude-dev") || name.contains("cline") {
                        return Some(DetectedAgent {
                            name: "Cline".to_string(),
                            agent_type: AgentType::Cline,
                            path: Some(entry.path()),
                            version: None,
                            is_running: false, // VSCode extension
                            can_delegate: false, // Não dá pra chamar via CLI
                        });
                    }
                }
            }
        }
        None
    }

    fn detect_cursor() -> Option<DetectedAgent> {
        // Check if Cursor is installed
        let cursor_paths = [
            "/opt/cursor",
            "/usr/local/bin/cursor",
            "/Applications/Cursor.app",
        ];

        for path in cursor_paths {
            if Path::new(path).exists() {
                return Some(DetectedAgent {
                    name: "Cursor".to_string(),
                    agent_type: AgentType::Cursor,
                    path: Some(PathBuf::from(path)),
                    version: None,
                    is_running: Command::new("pgrep")
                        .args(["-f", "cursor"])
                        .output()
                        .is_ok_and(|o| o.status.success()),
                    can_delegate: false,
                });
            }
        }
        None
    }

    /// Detecta MCP servers configurados
    pub fn detect_mcp_servers(&mut self) {
        self.mcp_servers.clear();

        // Check Claude Code config for MCP servers
        if let Some(home) = dirs::home_dir() {
            let claude_config = home.join(".claude/claude_desktop_config.json");
            if claude_config.exists() {
                if let Ok(content) = std::fs::read_to_string(&claude_config) {
                    if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(servers) = config.get("mcpServers").and_then(|v| v.as_object()) {
                            for (name, _) in servers {
                                self.mcp_servers.push(McpServerInfo {
                                    name: name.clone(),
                                    transport: "stdio".to_string(),
                                    tools_count: 0, // Unknown until connected
                                    is_running: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    /// Detecta informações do projeto atual
    pub fn detect_project(&mut self) {
        let cwd = std::env::current_dir().ok();
        let cwd = match cwd {
            Some(p) => p,
            None => return,
        };

        // Find project root (look for markers)
        let markers = [
            ("Cargo.toml", ProjectLanguage::Rust),
            ("package.json", ProjectLanguage::TypeScript),
            ("pyproject.toml", ProjectLanguage::Python),
            ("go.mod", ProjectLanguage::Go),
            ("mix.exs", ProjectLanguage::Elixir),
            ("Gemfile", ProjectLanguage::Ruby),
        ];

        let mut project_root = None;
        let mut language = ProjectLanguage::Unknown;

        for (marker, lang) in markers {
            let path = cwd.join(marker);
            if path.exists() {
                project_root = Some(cwd.clone());
                language = lang;
                break;
            }

            // Check parent dirs
            let mut current = cwd.clone();
            while let Some(parent) = current.parent() {
                if parent.join(marker).exists() {
                    project_root = Some(parent.to_path_buf());
                    language = lang;
                    break;
                }
                current = parent.to_path_buf();
            }
        }

        if let Some(root) = project_root {
            let name = root.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let has_git = root.join(".git").exists();
            let has_tests = root.join("tests").exists() || root.join("test").exists();

            // Detect framework
            let framework = Self::detect_framework(&root, &language);

            self.project = Some(ProjectSense {
                root,
                name,
                language,
                framework,
                has_git,
                has_tests,
                build_status: None,
            });
        }
    }

    fn detect_framework(root: &Path, language: &ProjectLanguage) -> Option<String> {
        match language {
            ProjectLanguage::Rust => {
                // Check Cargo.toml for known frameworks
                let cargo = root.join("Cargo.toml");
                if let Ok(content) = std::fs::read_to_string(&cargo) {
                    if content.contains("axum") { return Some("Axum".to_string()); }
                    if content.contains("actix") { return Some("Actix".to_string()); }
                    if content.contains("rocket") { return Some("Rocket".to_string()); }
                    if content.contains("rmcp") { return Some("RMCP (MCP)".to_string()); }
                }
            }
            ProjectLanguage::TypeScript | ProjectLanguage::JavaScript => {
                let pkg = root.join("package.json");
                if let Ok(content) = std::fs::read_to_string(&pkg) {
                    if content.contains("next") { return Some("Next.js".to_string()); }
                    if content.contains("react") { return Some("React".to_string()); }
                    if content.contains("vue") { return Some("Vue".to_string()); }
                    if content.contains("express") { return Some("Express".to_string()); }
                }
            }
            ProjectLanguage::Python => {
                let pyproject = root.join("pyproject.toml");
                if let Ok(content) = std::fs::read_to_string(&pyproject) {
                    if content.contains("fastapi") { return Some("FastAPI".to_string()); }
                    if content.contains("django") { return Some("Django".to_string()); }
                    if content.contains("flask") { return Some("Flask".to_string()); }
                }
            }
            ProjectLanguage::Elixir => {
                let mix = root.join("mix.exs");
                if let Ok(content) = std::fs::read_to_string(&mix) {
                    if content.contains("phoenix") { return Some("Phoenix".to_string()); }
                }
            }
            _ => {}
        }
        None
    }

    /// Percebe contexto do usuário
    pub fn sense_user_context(&mut self) {
        let hour = chrono::Local::now().hour();
        self.user_context.is_late_night = hour < 6;

        // TODO: Track working hours from session start
        // TODO: Analyze message patterns for frustration
    }

    /// Gera resumo do ambiente para incluir no system prompt
    pub fn summarize(&self) -> String {
        let mut summary = String::new();

        // Outros agentes
        if !self.agents.is_empty() {
            summary.push_str("## Outros Agentes no PC\n\n");
            for agent in &self.agents {
                let status = if agent.is_running { " [RODANDO]" } else { "" };
                let delegate = if agent.can_delegate { " - posso delegar tarefas" } else { "" };
                summary.push_str(&format!("- **{}**{}{}\n", agent.name, status, delegate));
            }
            summary.push('\n');
        }

        // Projeto atual
        if let Some(ref project) = self.project {
            summary.push_str("## Projeto Atual\n\n");
            summary.push_str(&format!("- **Nome:** {}\n", project.name));
            summary.push_str(&format!("- **Linguagem:** {:?}\n", project.language));
            if let Some(ref fw) = project.framework {
                summary.push_str(&format!("- **Framework:** {}\n", fw));
            }
            summary.push_str(&format!("- **Git:** {}\n", if project.has_git { "sim" } else { "não" }));
            summary.push_str(&format!("- **Testes:** {}\n", if project.has_tests { "sim" } else { "não" }));
            summary.push('\n');
        }

        // MCP Servers
        if !self.mcp_servers.is_empty() {
            summary.push_str("## MCP Servers Configurados\n\n");
            for server in &self.mcp_servers {
                summary.push_str(&format!("- {}\n", server.name));
            }
            summary.push('\n');
        }

        // Contexto do usuário
        if self.user_context.is_late_night {
            summary.push_str("**Nota:** É madrugada. O usuário pode estar cansado.\n\n");
        }

        summary
    }

    /// Lista agentes que podem receber delegação
    pub fn delegatable_agents(&self) -> Vec<&DetectedAgent> {
        self.agents.iter().filter(|a| a.can_delegate).collect()
    }
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sense_system_creation() {
        let sense = SenseSystem::new();
        assert!(sense.agents.is_empty());
    }

    #[test]
    fn test_project_language_detection() {
        // This would need a temp dir with markers to test properly
        let sense = SenseSystem::new();
        assert!(sense.project.is_none());
    }
}
