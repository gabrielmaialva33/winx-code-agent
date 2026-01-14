//! Chat History
//!
//! Persistência de conversas em formato Markdown (inspirado no chat.md).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::session::{ChatSession, SessionMeta};
use crate::errors::WinxError;
use crate::providers::{Message, MessageContent, Role};

/// Histórico de chats
pub struct ChatHistory {
    /// Diretório do histórico
    pub dir: PathBuf,
}

impl ChatHistory {
    /// Cria novo gerenciador de histórico
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Lista sessões salvas
    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, WinxError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();

        for entry in fs::read_dir(&self.dir)
            .map_err(|e| WinxError::FileError(format!("Failed to read history dir: {e}")))?
        {
            let entry = entry.map_err(|e| WinxError::FileError(e.to_string()))?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "md") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Some(summary) = parse_session_summary(&content, &path) {
                        sessions.push(summary);
                    }
                }
            }
        }

        // Ordena por data (mais recente primeiro)
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(sessions)
    }

    /// Carrega sessão do arquivo
    pub fn load_session(&self, id: &str) -> Result<SavedSession, WinxError> {
        let path = self.dir.join(format!("{id}.md"));

        if !path.exists() {
            return Err(WinxError::FileAccessError {
                path: path.clone(),
                message: "Session file not found".to_string(),
            });
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| WinxError::FileError(format!("Failed to read session: {e}")))?;

        parse_chat_markdown(&content)
    }

    /// Salva sessão
    pub fn save_session(&self, session: &ChatSession) -> Result<PathBuf, WinxError> {
        // Cria diretório se não existe
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)
                .map_err(|e| WinxError::FileError(format!("Failed to create history dir: {e}")))?;
        }

        let path = self.dir.join(format!("{}.md", session.meta.id));
        let content = serialize_chat_markdown(session);

        fs::write(&path, content)
            .map_err(|e| WinxError::FileError(format!("Failed to save session: {e}")))?;

        Ok(path)
    }

    /// Remove sessão
    pub fn delete_session(&self, id: &str) -> Result<(), WinxError> {
        let path = self.dir.join(format!("{id}.md"));

        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| WinxError::FileError(format!("Failed to delete session: {e}")))?;
        }

        Ok(())
    }
}

/// Resumo de sessão (para listagem)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub model: String,
    pub message_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// Sessão salva
#[derive(Debug, Clone)]
pub struct SavedSession {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
}

/// Serializa sessão para markdown
pub fn serialize_chat_markdown(session: &ChatSession) -> String {
    let mut out = String::new();

    // Frontmatter YAML
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", session.meta.id));
    if let Some(ref title) = session.meta.title {
        out.push_str(&format!("title: \"{}\"\n", title.replace('"', "\\\"")));
    }
    out.push_str(&format!("model: {}:{}\n", session.meta.provider, session.meta.model));
    out.push_str(&format!("created: {}\n", session.meta.created_at.to_rfc3339()));
    out.push_str(&format!("updated: {}\n", session.meta.updated_at.to_rfc3339()));
    if !session.meta.tags.is_empty() {
        out.push_str(&format!("tags: [{}]\n", session.meta.tags.join(", ")));
    }
    out.push_str("---\n\n");

    // Título
    if let Some(ref title) = session.meta.title {
        out.push_str(&format!("# {title}\n\n"));
    }

    // Mensagens
    for msg in session.messages() {
        let role_header = match msg.role {
            Role::User => "## User",
            Role::Assistant => "## Assistant",
            Role::System => "## System",
        };

        out.push_str(role_header);
        out.push_str("\n\n");

        let content = match &msg.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => {
                parts.iter().filter_map(|p| {
                    if let crate::providers::ContentPart::Text { text } = p {
                        Some(text.clone())
                    } else {
                        None
                    }
                }).collect::<Vec<_>>().join("\n")
            }
            MessageContent::ToolResult { content, tool_use_id } => {
                format!("*Tool Result ({tool_use_id})*\n\n{content}")
            }
        };

        out.push_str(&content);
        out.push_str("\n\n");
    }

    out
}

/// Parse markdown para sessão
pub fn parse_chat_markdown(content: &str) -> Result<SavedSession, WinxError> {
    // Split frontmatter
    let parts: Vec<&str> = content.splitn(3, "---").collect();

    if parts.len() < 3 {
        return Err(WinxError::ParseError("Invalid markdown format: missing frontmatter".to_string()));
    }

    let frontmatter = parts[1].trim();
    let body = parts[2].trim();

    // Parse frontmatter
    let meta = parse_frontmatter(frontmatter)?;

    // Parse messages
    let messages = parse_messages(body)?;

    Ok(SavedSession { meta, messages })
}

/// Parse frontmatter YAML
fn parse_frontmatter(content: &str) -> Result<SessionMeta, WinxError> {
    let mut id = String::new();
    let mut title = None;
    let mut model = String::new();
    let mut provider = String::new();
    let mut created_at = Utc::now();
    let mut updated_at = Utc::now();
    let mut tags = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');

            match key {
                "id" => id = value.to_string(),
                "title" => title = Some(value.to_string()),
                "model" => {
                    // Formato: provider:model
                    if let Some((p, m)) = value.split_once(':') {
                        provider = p.to_string();
                        model = m.to_string();
                    } else {
                        model = value.to_string();
                    }
                }
                "created" => {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
                        created_at = dt.with_timezone(&Utc);
                    }
                }
                "updated" => {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
                        updated_at = dt.with_timezone(&Utc);
                    }
                }
                "tags" => {
                    let tag_str = value.trim_matches(|c| c == '[' || c == ']');
                    tags = tag_str.split(',').map(|t| t.trim().to_string()).collect();
                }
                _ => {}
            }
        }
    }

    if id.is_empty() {
        return Err(WinxError::ParseError("Missing session id in frontmatter".to_string()));
    }

    Ok(SessionMeta {
        id,
        title,
        model,
        provider,
        created_at,
        updated_at,
        tags,
    })
}

/// Parse mensagens do body markdown
fn parse_messages(content: &str) -> Result<Vec<Message>, WinxError> {
    let mut messages = Vec::new();
    let mut current_role: Option<Role> = None;
    let mut current_content = String::new();

    for line in content.lines() {
        if line.starts_with("## User") {
            // Salva mensagem anterior
            if let Some(role) = current_role.take() {
                if !current_content.trim().is_empty() {
                    messages.push(create_message(role, &current_content));
                }
            }
            current_role = Some(Role::User);
            current_content.clear();
        } else if line.starts_with("## Assistant") {
            if let Some(role) = current_role.take() {
                if !current_content.trim().is_empty() {
                    messages.push(create_message(role, &current_content));
                }
            }
            current_role = Some(Role::Assistant);
            current_content.clear();
        } else if line.starts_with("## System") {
            if let Some(role) = current_role.take() {
                if !current_content.trim().is_empty() {
                    messages.push(create_message(role, &current_content));
                }
            }
            current_role = Some(Role::System);
            current_content.clear();
        } else if line.starts_with("# ") {
            // Título, ignora
            continue;
        } else if current_role.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    // Última mensagem
    if let Some(role) = current_role {
        if !current_content.trim().is_empty() {
            messages.push(create_message(role, &current_content));
        }
    }

    Ok(messages)
}

fn create_message(role: Role, content: &str) -> Message {
    let content = content.trim().to_string();
    Message {
        role,
        content: MessageContent::Text(content),
    }
}

fn parse_session_summary(content: &str, path: &Path) -> Option<SessionSummary> {
    let session = parse_chat_markdown(content).ok()?;

    Some(SessionSummary {
        id: session.meta.id,
        title: session.meta.title,
        model: format!("{}:{}", session.meta.provider, session.meta.model),
        message_count: session.messages.len(),
        created_at: session.meta.created_at,
        updated_at: session.meta.updated_at,
        tags: session.meta.tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frontmatter_parse() {
        let frontmatter = r#"
id: sess_abc123
title: "Test Session"
model: ollama:llama3.2
created: 2025-01-13T00:00:00Z
updated: 2025-01-13T01:00:00Z
tags: [rust, test]
"#;

        let meta = parse_frontmatter(frontmatter).unwrap();

        assert_eq!(meta.id, "sess_abc123");
        assert_eq!(meta.title, Some("Test Session".to_string()));
        assert_eq!(meta.model, "llama3.2");
        assert_eq!(meta.provider, "ollama");
        assert_eq!(meta.tags, vec!["rust", "test"]);
    }

    #[test]
    fn test_message_parse() {
        let body = r#"
# Test Session

## User

Hello, how are you?

## Assistant

I'm doing great, thanks for asking!

## User

Good to hear.
"#;

        let messages = parse_messages(body).unwrap();

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert!(messages[0].text().contains("Hello"));
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[test]
    fn test_round_trip() {
        let markdown = r#"---
id: sess_test123
model: ollama:llama3.2
created: 2025-01-13T00:00:00+00:00
updated: 2025-01-13T00:00:00+00:00
---

## User

Hello

## Assistant

Hi there!
"#;

        let session = parse_chat_markdown(markdown).unwrap();

        assert_eq!(session.meta.id, "sess_test123");
        assert_eq!(session.messages.len(), 2);
    }
}
