//! Session Parser - Lê sessões do Claude Code.
//!
//! Parse de arquivos JSONL em `~/.claude/projects/**/*.jsonl`
//! Extrai mensagens user/assistant para análise.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::errors::WinxError;

/// Parser de sessões do Claude Code
pub struct SessionParser {
    /// Diretório das sessões
    sessions_dir: PathBuf,
    /// Contagem de sessões processadas
    session_count: usize,
}

/// Mensagem extraída de uma sessão
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    /// ID da sessão
    pub session_id: String,
    /// Role: "user" ou "assistant"
    pub role: String,
    /// Conteúdo da mensagem
    pub content: String,
    /// Timestamp
    pub timestamp: String,
    /// Diretório de trabalho
    pub cwd: Option<String>,
    /// Projeto associado
    pub project: Option<String>,
}

/// Entrada JSONL de uma sessão
#[derive(Debug, Deserialize)]
struct SessionEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<MessageContent>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
}

/// Conteúdo da mensagem (pode ser string ou array)
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Simple { role: String, content: String },
    Complex { role: String, content: Vec<ContentPart> },
}

/// Parte do conteúdo (para mensagens complexas)
#[derive(Debug, Deserialize)]
struct ContentPart {
    #[serde(rename = "type")]
    part_type: String,
    text: Option<String>,
}

impl SessionParser {
    /// Cria novo parser
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self {
            sessions_dir,
            session_count: 0,
        }
    }

    /// Retorna contagem de sessões
    pub fn session_count(&self) -> usize {
        self.session_count
    }

    /// Encontra todos os arquivos de sessão
    pub async fn find_session_files(&self) -> Result<Vec<PathBuf>, WinxError> {
        let mut files = Vec::new();

        if !self.sessions_dir.exists() {
            warn!("Sessions directory not found: {:?}", self.sessions_dir);
            return Ok(files);
        }

        // Recursivamente encontra arquivos .jsonl
        let mut stack = vec![self.sessions_dir.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = fs::read_dir(&dir).await?;

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Ok(file_type) = entry.file_type().await {
                    if file_type.is_dir() {
                        stack.push(path);
                    } else if path.extension().map_or(false, |ext| ext == "jsonl") {
                        files.push(path);
                    }
                }
            }
        }

        info!("Found {} session files", files.len());
        Ok(files)
    }

    /// Parse um único arquivo de sessão
    pub async fn parse_session_file(
        &self,
        path: &PathBuf,
    ) -> Result<Vec<SessionMessage>, WinxError> {
        let mut messages = Vec::new();

        let file = fs::File::open(path).await?;

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // Extrai session_id do nome do arquivo
        let default_session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<SessionEntry>(&line) {
                Ok(entry) => {
                    // Só processa entradas com mensagem
                    if let Some(msg) = entry.message {
                        let session_id = entry
                            .session_id
                            .clone()
                            .unwrap_or_else(|| default_session_id.clone());

                        let (role, content) = match msg {
                            MessageContent::Simple { role, content } => (role, content),
                            MessageContent::Complex { role, content } => {
                                // Concatena textos das partes
                                let text = content
                                    .iter()
                                    .filter_map(|p| {
                                        if p.part_type == "text" {
                                            p.text.clone()
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                (role, text)
                            }
                        };

                        // Ignora mensagens vazias ou de sistema
                        if !content.trim().is_empty() && !content.contains("<command-message>") {
                            messages.push(SessionMessage {
                                session_id,
                                role,
                                content,
                                timestamp: entry.timestamp.unwrap_or_default(),
                                cwd: entry.cwd,
                                project: None, // TODO: extrair do path
                            });
                        }
                    }
                }
                Err(e) => {
                    // Log mas continua - algumas linhas podem ter formato diferente
                    debug!("Failed to parse line in {:?}: {}", path, e);
                }
            }
        }

        Ok(messages)
    }

    /// Parse todas as sessões
    pub async fn parse_all_sessions(&mut self) -> Result<Vec<SessionMessage>, WinxError> {
        let files = self.find_session_files().await?;
        self.session_count = files.len();

        let mut all_messages = Vec::new();

        for (i, file) in files.iter().enumerate() {
            if i % 100 == 0 {
                info!("Processing session {}/{}", i + 1, files.len());
            }

            match self.parse_session_file(file).await {
                Ok(messages) => {
                    // Adiciona projeto baseado no path
                    let project = file
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string());

                    for mut msg in messages {
                        msg.project = project.clone();
                        all_messages.push(msg);
                    }
                }
                Err(e) => {
                    warn!("Failed to parse {:?}: {}", file, e);
                }
            }
        }

        info!(
            "Parsed {} messages from {} sessions",
            all_messages.len(),
            self.session_count
        );

        Ok(all_messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_parser() {
        let parser = SessionParser::new(PathBuf::from("/tmp/test"));
        assert_eq!(parser.session_count(), 0);
    }

    #[tokio::test]
    async fn test_find_nonexistent_dir() {
        let parser = SessionParser::new(PathBuf::from("/nonexistent/path"));
        let files = parser.find_session_files().await.unwrap();
        assert!(files.is_empty());
    }
}
