//! Stream Output
//!
//! Utilitários para processar e exibir streaming de respostas LLM.

use std::io::{self, Write};

use futures::StreamExt;

use crate::errors::WinxError;
use crate::providers::{EventStream, StreamEvent};

/// Configuração do printer
#[derive(Debug, Clone)]
pub struct StreamPrinter {
    /// Buffer para acumular texto
    buffer: String,

    /// Habilita cores
    colors: bool,

    /// Mostra tool calls
    show_tools: bool,
}

impl Default for StreamPrinter {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamPrinter {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            colors: true,
            show_tools: true,
        }
    }

    pub fn with_colors(mut self, enabled: bool) -> Self {
        self.colors = enabled;
        self
    }

    pub fn with_tool_output(mut self, enabled: bool) -> Self {
        self.show_tools = enabled;
        self
    }

    /// Processa evento e retorna texto a ser impresso
    pub fn process_event(&mut self, event: &StreamEvent) -> Option<String> {
        match event {
            StreamEvent::Text(text) => {
                self.buffer.push_str(text);
                Some(text.clone())
            }

            StreamEvent::ToolCallStart { id, name } => {
                if self.show_tools {
                    let msg = if self.colors {
                        format!("\n\x1b[33m⚡ Tool: {}\x1b[0m ({})\n", name, id)
                    } else {
                        format!("\n⚡ Tool: {} ({})\n", name, id)
                    };
                    Some(msg)
                } else {
                    None
                }
            }

            StreamEvent::ToolCallDelta { arguments, .. } => {
                if self.show_tools {
                    Some(arguments.clone())
                } else {
                    None
                }
            }

            StreamEvent::ToolCallEnd { .. } => {
                if self.show_tools {
                    Some("\n".to_string())
                } else {
                    None
                }
            }

            StreamEvent::Done => {
                Some("\n".to_string())
            }

            StreamEvent::Error(err) => {
                let msg = if self.colors {
                    format!("\n\x1b[31m✗ Error: {}\x1b[0m\n", err)
                } else {
                    format!("\n✗ Error: {}\n", err)
                };
                Some(msg)
            }
        }
    }

    /// Retorna texto acumulado
    pub fn accumulated_text(&self) -> &str {
        &self.buffer
    }

    /// Limpa buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

/// Imprime stream para stdout
pub async fn print_stream(mut stream: EventStream) -> Result<String, WinxError> {
    let mut printer = StreamPrinter::new();
    let mut stdout = io::stdout();

    while let Some(event) = stream.next().await {
        if let Some(text) = printer.process_event(&event) {
            print!("{}", text);
            stdout.flush().map_err(|e| WinxError::FileError(e.to_string()))?;
        }

        if matches!(event, StreamEvent::Done) {
            break;
        }
    }

    Ok(printer.accumulated_text().to_string())
}

/// Coleta stream em string (sem output)
pub async fn collect_stream(mut stream: EventStream) -> Result<String, WinxError> {
    let mut printer = StreamPrinter::new().with_tool_output(false);

    while let Some(event) = stream.next().await {
        printer.process_event(&event);

        if matches!(event, StreamEvent::Done) {
            break;
        }
    }

    Ok(printer.accumulated_text().to_string())
}

/// Processa stream com callback
pub async fn process_stream<F>(
    mut stream: EventStream,
    mut callback: F,
) -> Result<String, WinxError>
where
    F: FnMut(&StreamEvent),
{
    let mut printer = StreamPrinter::new();

    while let Some(event) = stream.next().await {
        callback(&event);
        printer.process_event(&event);

        if matches!(event, StreamEvent::Done) {
            break;
        }
    }

    Ok(printer.accumulated_text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_printer() {
        let mut printer = StreamPrinter::new();

        let result = printer.process_event(&StreamEvent::Text("Hello ".to_string()));
        assert_eq!(result, Some("Hello ".to_string()));

        let result = printer.process_event(&StreamEvent::Text("World".to_string()));
        assert_eq!(result, Some("World".to_string()));

        assert_eq!(printer.accumulated_text(), "Hello World");
    }

    #[test]
    fn test_tool_output() {
        let mut printer = StreamPrinter::new().with_colors(false);

        let result = printer.process_event(&StreamEvent::ToolCallStart {
            id: "123".to_string(),
            name: "get_weather".to_string(),
        });

        assert!(result.is_some());
        assert!(result.unwrap().contains("get_weather"));
    }

    #[test]
    fn test_error_output() {
        let mut printer = StreamPrinter::new().with_colors(false);

        let result = printer.process_event(&StreamEvent::Error("Test error".to_string()));

        assert!(result.is_some());
        assert!(result.unwrap().contains("Test error"));
    }
}
