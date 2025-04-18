use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::lsp::types::{LSPConfig, Language, Position, Range, Symbol, SymbolLocation};

// Use the more comprehensive sanitize_json_text function from lib.rs instead

/// Message to be sent to the language server
#[derive(Debug)]
enum ClientMessage {
    Initialize {
        root_path: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    Shutdown {
        response_tx: oneshot::Sender<Result<()>>,
    },
    OpenFile {
        file_path: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    CloseFile {
        #[allow(dead_code)]
        file_path: PathBuf,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<()>>,
    },
    FindSymbol {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        within_path: Option<PathBuf>,
        #[allow(dead_code)]
        include_body: bool,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<Vec<Symbol>>>,
    },
    FindReferences {
        #[allow(dead_code)]
        location: SymbolLocation,
        #[allow(dead_code)]
        include_body: bool,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<Vec<Symbol>>>,
    },
    InsertText {
        #[allow(dead_code)]
        file_path: PathBuf,
        #[allow(dead_code)]
        position: Position,
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<Position>>,
    },
    DeleteText {
        #[allow(dead_code)]
        file_path: PathBuf,
        #[allow(dead_code)]
        range: Range,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<String>>,
    },
    GetDocumentSymbols {
        #[allow(dead_code)]
        file_path: PathBuf,
        #[allow(dead_code)]
        include_body: bool,
        #[allow(dead_code)]
        response_tx: oneshot::Sender<Result<Vec<Symbol>>>,
    },
}

/// Wrapper for an LSP client
#[derive(Clone)]
pub struct LSPClient {
    #[allow(dead_code)]
    config: LSPConfig,
    tx: mpsc::Sender<ClientMessage>,
    server_handle: Arc<Mutex<Option<u32>>>,
}

impl LSPClient {
    /// Create a new LSP client for the given language
    pub async fn new(config: LSPConfig, root_path: impl AsRef<Path>) -> Result<Self> {
        let (tx, rx) = mpsc::channel(100);
        let server_handle: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let server_handle_clone = server_handle.clone();

        // Start the client task
        tokio::spawn(Self::run_client_loop(
            config.clone(),
            rx,
            server_handle_clone,
        ));

        // Create the client
        let client = Self {
            config,
            tx,
            server_handle,
        };

        // Initialize the language server
        client.initialize(root_path).await?;

        Ok(client)
    }

    /// Initialize the language server
    pub async fn initialize(&self, root_path: impl AsRef<Path>) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::Initialize {
                root_path: root_path.as_ref().to_path_buf(),
                response_tx,
            })
            .await
            .context("Failed to send initialize message")?;

        response_rx
            .await
            .context("Failed to receive initialize response")??;
        Ok(())
    }

    /// Shutdown the language server
    pub async fn shutdown(&self) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::Shutdown { response_tx })
            .await
            .context("Failed to send shutdown message")?;

        response_rx
            .await
            .context("Failed to receive shutdown response")??;

        // Also kill the server process
        let mut handle = self.server_handle.lock().unwrap();
        if let Some(_pid) = handle.take() {
            // We only have the process ID, not the Child struct
            // In a real implementation, you would use the process ID to kill the process
            debug!("Would kill LSP server process");
        }

        Ok(())
    }

    /// Open a file in the language server
    pub async fn open_file(&self, file_path: impl AsRef<Path>) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::OpenFile {
                file_path: file_path.as_ref().to_path_buf(),
                response_tx,
            })
            .await
            .context("Failed to send open file message")?;

        response_rx
            .await
            .context("Failed to receive open file response")??;
        Ok(())
    }

    /// Close a file in the language server
    pub async fn close_file(&self, file_path: impl AsRef<Path>) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::CloseFile {
                file_path: file_path.as_ref().to_path_buf(),
                response_tx,
            })
            .await
            .context("Failed to send close file message")?;

        response_rx
            .await
            .context("Failed to receive close file response")??;
        Ok(())
    }

    /// Find a symbol by name in the workspace
    pub async fn find_symbol(
        &self,
        name: &str,
        within_path: Option<impl AsRef<Path>>,
        include_body: bool,
    ) -> Result<Vec<Symbol>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::FindSymbol {
                name: name.to_string(),
                within_path: within_path.map(|p| p.as_ref().to_path_buf()),
                include_body,
                response_tx,
            })
            .await
            .context("Failed to send find symbol message")?;

        response_rx
            .await
            .context("Failed to receive find symbol response")??;
        Ok(vec![]) // Placeholder until implementation
    }

    /// Find references to a symbol
    pub async fn find_references(
        &self,
        location: SymbolLocation,
        include_body: bool,
    ) -> Result<Vec<Symbol>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::FindReferences {
                location,
                include_body,
                response_tx,
            })
            .await
            .context("Failed to send find references message")?;

        response_rx
            .await
            .context("Failed to receive find references response")??;
        Ok(vec![]) // Placeholder until implementation
    }

    /// Insert text at a position in a file
    pub async fn insert_text(
        &self,
        file_path: impl AsRef<Path>,
        position: Position,
        text: &str,
    ) -> Result<Position> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::InsertText {
                file_path: file_path.as_ref().to_path_buf(),
                position,
                text: text.to_string(),
                response_tx,
            })
            .await
            .context("Failed to send insert text message")?;

        response_rx
            .await
            .context("Failed to receive insert text response")??;
        Ok(Position {
            line: 0,
            character: 0,
        }) // Placeholder until implementation
    }

    /// Delete text in a range in a file
    pub async fn delete_text(&self, file_path: impl AsRef<Path>, range: Range) -> Result<String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::DeleteText {
                file_path: file_path.as_ref().to_path_buf(),
                range,
                response_tx,
            })
            .await
            .context("Failed to send delete text message")?;

        response_rx
            .await
            .context("Failed to receive delete text response")??;
        Ok("".to_string()) // Placeholder until implementation
    }

    /// Get symbols in a document
    pub async fn get_document_symbols(
        &self,
        file_path: impl AsRef<Path>,
        include_body: bool,
    ) -> Result<Vec<Symbol>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ClientMessage::GetDocumentSymbols {
                file_path: file_path.as_ref().to_path_buf(),
                include_body,
                response_tx,
            })
            .await
            .context("Failed to send get document symbols message")?;

        response_rx
            .await
            .context("Failed to receive get document symbols response")??;
        Ok(vec![]) // Placeholder until implementation
    }

    /// Send a request to the language server using JSON-RPC
    pub async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // This is a placeholder implementation for now
        // In a real implementation, this would send the request to the language server
        // and wait for a response
        debug!("Sending request: {} with params: {:?}", method, params);

        // For now, we'll return an empty object as a placeholder
        Ok(serde_json::json!({}))
    }

    /// Run the client loop that communicates with the language server
    async fn run_client_loop(
        config: LSPConfig,
        mut rx: mpsc::Receiver<ClientMessage>,
        server_handle: Arc<Mutex<Option<u32>>>,
    ) {
        // Communication state
        let mut _server_process: Option<Child> = None; // Prefixado com _ pois não é usado
        let mut stdin_writer = None;
        let mut stdout_reader = None;
        let mut request_id = 0;

        // Track pending requests and their response channels
        let _pending_requests: HashMap<usize, oneshot::Sender<Result<Value>>> = HashMap::new(); // Prefixado com _ pois não é usado

        while let Some(message) = rx.recv().await {
            match message {
                ClientMessage::Initialize {
                    root_path,
                    response_tx,
                } => {
                    // Start the language server process
                    match Self::start_server_process(&config.language, &root_path).await {
                        Ok(mut process) => {
                            // Get the process's stdin and stdout
                            let process_stdin = process
                                .stdin
                                .take()
                                .context("Failed to capture language server stdin");
                            let process_stdout = process
                                .stdout
                                .take()
                                .context("Failed to capture language server stdout");

                            match (process_stdin, process_stdout) {
                                (Ok(stdin), Ok(stdout)) => {
                                    // Store the server process ID first
                                    {
                                        let mut handle = server_handle.lock().unwrap();
                                        if let Some(pid) = process.id() {
                                            *handle = Some(pid);
                                        }
                                    }
                                    _server_process = Some(process);

                                    // Create async writers and readers - Tokio já fornece streams assíncronos
                                    stdin_writer = Some(BufWriter::new(stdin));
                                    stdout_reader = Some(BufReader::new(stdout));

                                    // Send initialize request
                                    let initialize_params = json!({
                                        "processId": std::process::id(),
                                        "rootPath": root_path.to_string_lossy(),
                                        "rootUri": format!("file://{}", root_path.to_string_lossy()),
                                        "capabilities": {
                                            "textDocument": {
                                                "synchronization": {
                                                    "didSave": true,
                                                    "willSave": false
                                                },
                                                "completion": {
                                                    "dynamicRegistration": false,
                                                    "completionItem": {
                                                        "snippetSupport": false
                                                    }
                                                },
                                                "hover": {
                                                    "dynamicRegistration": false
                                                },
                                                "definition": {
                                                    "dynamicRegistration": false
                                                },
                                                "references": {
                                                    "dynamicRegistration": false
                                                },
                                                "documentSymbol": {
                                                    "dynamicRegistration": false,
                                                    "hierarchicalDocumentSymbolSupport": true
                                                }
                                            },
                                            "workspace": {
                                                "symbol": {
                                                    "dynamicRegistration": false
                                                }
                                            }
                                        }
                                    });

                                    request_id += 1;
                                    let init_request = json!({
                                        "jsonrpc": "2.0",
                                        "id": request_id,
                                        "method": "initialize",
                                        "params": initialize_params
                                    });

                                    if let Some(writer) = stdin_writer.as_mut() {
                                        // Ensure the outgoing request doesn't have ANSI codes
                                        let raw_request_str = init_request.to_string();
                                        let request_str = crate::strip_ansi_codes(&raw_request_str);
                                        let content_length = request_str.len();

                                        if let Err(e) = writer
                                            .write_all(
                                                format!(
                                                    "Content-Length: {}\r\n\r\n{}",
                                                    content_length, request_str
                                                )
                                                .as_bytes(),
                                            )
                                            .await
                                        {
                                            error!("Failed to send initialize request: {}", e);
                                            let _ = response_tx.send(Err(anyhow::anyhow!(
                                                "Failed to send initialize request: {}",
                                                e
                                            )));
                                            return;
                                        }

                                        if let Err(e) = writer.flush().await {
                                            error!("Failed to flush stdin after initialize request: {}", e);
                                            let _ = response_tx.send(Err(anyhow::anyhow!(
                                                "Failed to flush stdin: {}",
                                                e
                                            )));
                                            return;
                                        }

                                        // Now read the response from stdout
                                        if let Some(reader) = stdout_reader.as_mut() {
                                            let mut content_length = 0;
                                            let mut line = String::new();

                                            // Parse headers
                                            loop {
                                                line.clear();
                                                if let Err(e) = reader.read_line(&mut line).await {
                                                    error!("Failed to read response header: {}", e);
                                                    let _ = response_tx.send(Err(anyhow::anyhow!(
                                                        "Failed to read response: {}",
                                                        e
                                                    )));
                                                    return;
                                                }

                                                if line.trim().is_empty() {
                                                    break; // End of headers
                                                }

                                                if line.starts_with("Content-Length:") {
                                                    if let Some(len_str) =
                                                        line.strip_prefix("Content-Length:")
                                                    {
                                                        if let Ok(len) =
                                                            len_str.trim().parse::<usize>()
                                                        {
                                                            content_length = len;
                                                        }
                                                    }
                                                }
                                            }

                                            // Read the response body
                                            if content_length > 0 {
                                                let mut buffer = vec![0; content_length];
                                                if let Err(e) = reader.read_exact(&mut buffer).await
                                                {
                                                    error!("Failed to read response body: {}", e);
                                                    let _ = response_tx.send(Err(anyhow::anyhow!(
                                                        "Failed to read response body: {}",
                                                        e
                                                    )));
                                                    return;
                                                }

                                                // Log raw data for debugging
                                                crate::debug_json_bytes(
                                                    &buffer,
                                                    "LSP-RESPONSE-RAW",
                                                );

                                                // Convert to string with proper UTF-8 handling
                                                let text = String::from_utf8_lossy(&buffer);

                                                // Apply multiple sanitization passes to ensure complete removal of all ANSI codes
                                                let text_no_ansi = crate::strip_ansi_codes(&text);
                                                let text_double_sanitized =
                                                    crate::strip_ansi_codes(&text_no_ansi);

                                                // Log if ANSI codes were detected
                                                if text != text_no_ansi {
                                                    info!("ANSI codes detected and sanitization passes applied");
                                                }

                                                // Perform final sanitization with the improved sanitize_json_text function
                                                let clean_text = crate::sanitize_json_text(
                                                    &text_double_sanitized,
                                                );

                                                // Log the fully sanitized text for debugging
                                                crate::debug_json_bytes(
                                                    clean_text.as_bytes(),
                                                    "LSP-RESPONSE-FULLY-SANITIZED",
                                                );

                                                // Use the sanitized text for JSON parsing
                                                match serde_json::from_str::<Value>(&clean_text) {
                                                    Ok(response) => {
                                                        // Check for success
                                                        if response.get("error").is_some() {
                                                            error!("Language server initialization error: {:?}", response);
                                                            let _ = response_tx.send(Err(anyhow::anyhow!("Language server initialization error: {:?}", response)));
                                                        } else {
                                                            debug!("Language server initialized successfully");

                                                            // Send initialized notification
                                                            let initialized_notification = json!({
                                                                "jsonrpc": "2.0",
                                                                "method": "initialized",
                                                                "params": {}
                                                            });

                                                            // Ensure the outgoing notification doesn't have ANSI codes
                                                            let notification_str =
                                                                initialized_notification
                                                                    .to_string();
                                                            // Apply the improved strip_ansi_codes and sanitize_json_text functions
                                                            let clean_notification_str =
                                                                crate::strip_ansi_codes(
                                                                    &notification_str,
                                                                );
                                                            let final_clean_str =
                                                                crate::sanitize_json_text(
                                                                    &clean_notification_str,
                                                                );
                                                            let content_length =
                                                                final_clean_str.len();

                                                            if let Err(e) = writer.write_all(format!("Content-Length: {}\r\n\r\n{}", content_length, final_clean_str).as_bytes()).await {
                                                                error!("Failed to send initialized notification: {}", e);
                                                            }

                                                            if let Err(e) = writer.flush().await {
                                                                error!("Failed to flush stdin after initialized notification: {}", e);
                                                            }

                                                            let _ = response_tx.send(Ok(()));
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!("Failed to parse initialization response: {}", e);
                                                        let _ = response_tx.send(Err(anyhow::anyhow!("Failed to parse initialization response: {}", e)));
                                                    }
                                                }
                                            } else {
                                                error!("Invalid content length in response");
                                                let _ = response_tx.send(Err(anyhow::anyhow!(
                                                    "Invalid content length in response"
                                                )));
                                            }
                                        } else {
                                            error!("No stdout reader available");
                                            let _ = response_tx.send(Err(anyhow::anyhow!(
                                                "No stdout reader available"
                                            )));
                                        }
                                    } else {
                                        error!("No stdin writer available");
                                        let _ = response_tx.send(Err(anyhow::anyhow!(
                                            "No stdin writer available"
                                        )));
                                    }
                                }
                                (Err(e), _) => {
                                    error!("Failed to capture language server stdin: {}", e);
                                    let _ = response_tx.send(Err(e));
                                }
                                (_, Err(e)) => {
                                    error!("Failed to capture language server stdout: {}", e);
                                    let _ = response_tx.send(Err(e));
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to start language server: {}", e);
                            let _ = response_tx.send(Err(e));
                        }
                    }
                }
                ClientMessage::Shutdown { response_tx } => {
                    // Send shutdown request to the server
                    if let (Some(writer), Some(_reader)) =
                        (stdin_writer.as_mut(), stdout_reader.as_mut())
                    {
                        request_id += 1;
                        let shutdown_request = json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "method": "shutdown",
                            "params": null
                        });

                        let request_str = shutdown_request.to_string();
                        let content_length = request_str.len();

                        if let Err(e) = writer
                            .write_all(
                                format!(
                                    "Content-Length: {}\r\n\r\n{}",
                                    content_length, request_str
                                )
                                .as_bytes(),
                            )
                            .await
                        {
                            error!("Failed to send shutdown request: {}", e);
                            let _ = response_tx.send(Err(anyhow::anyhow!(
                                "Failed to send shutdown request: {}",
                                e
                            )));
                            break;
                        }

                        if let Err(e) = writer.flush().await {
                            error!("Failed to flush stdin after shutdown request: {}", e);
                            let _ = response_tx
                                .send(Err(anyhow::anyhow!("Failed to flush stdin: {}", e)));
                            break;
                        }

                        // Read the response
                        // ... (similar to reading initialize response)

                        // Send exit notification
                        let exit_notification = json!({
                            "jsonrpc": "2.0",
                            "method": "exit",
                            "params": null
                        });

                        let notification_str = exit_notification.to_string();
                        let content_length = notification_str.len();

                        if let Err(e) = writer
                            .write_all(
                                format!(
                                    "Content-Length: {}\r\n\r\n{}",
                                    content_length, notification_str
                                )
                                .as_bytes(),
                            )
                            .await
                        {
                            error!("Failed to send exit notification: {}", e);
                        }

                        if let Err(e) = writer.flush().await {
                            error!("Failed to flush stdin after exit notification: {}", e);
                        }
                    }

                    let _ = response_tx.send(Ok(()));
                    break;
                }
                ClientMessage::OpenFile {
                    file_path,
                    response_tx,
                } => {
                    if let (Some(writer), Some(_)) = (stdin_writer.as_mut(), stdout_reader.as_mut())
                    {
                        // Read file content
                        match tokio::fs::read_to_string(&file_path).await {
                            Ok(content) => {
                                let uri = format!("file://{}", file_path.to_string_lossy());
                                let open_notification = json!({
                                    "jsonrpc": "2.0",
                                    "method": "textDocument/didOpen",
                                    "params": {
                                        "textDocument": {
                                            "uri": uri,
                                            "languageId": Self::get_language_id(&config.language),
                                            "version": 1,
                                            "text": content
                                        }
                                    }
                                });

                                let notification_str = open_notification.to_string();
                                let content_length = notification_str.len();

                                if let Err(e) = writer
                                    .write_all(
                                        format!(
                                            "Content-Length: {}\r\n\r\n{}",
                                            content_length, notification_str
                                        )
                                        .as_bytes(),
                                    )
                                    .await
                                {
                                    error!("Failed to send didOpen notification: {}", e);
                                    let _ = response_tx.send(Err(anyhow::anyhow!(
                                        "Failed to send didOpen notification: {}",
                                        e
                                    )));
                                    continue;
                                }

                                if let Err(e) = writer.flush().await {
                                    error!(
                                        "Failed to flush stdin after didOpen notification: {}",
                                        e
                                    );
                                    let _ = response_tx
                                        .send(Err(anyhow::anyhow!("Failed to flush stdin: {}", e)));
                                    continue;
                                }

                                let _ = response_tx.send(Ok(()));
                            }
                            Err(e) => {
                                error!("Failed to read file content: {}", e);
                                let _ = response_tx.send(Err(anyhow::anyhow!(
                                    "Failed to read file content: {}",
                                    e
                                )));
                            }
                        }
                    } else {
                        let _ = response_tx
                            .send(Err(anyhow::anyhow!("Language server not initialized")));
                    }
                }
                // Other message handlers will be implemented in future PRs
                _ => {
                    warn!("Message type not yet implemented");
                }
            }
        }

        // Clean up resources here if needed
        info!("LSP client loop terminated");
    }

    /// Start the language server process for the given language
    async fn start_server_process(language: &Language, root_path: &Path) -> Result<Child> {
        // Execute the appropriate command for the language
        let command = match language {
            Language::Rust => Command::new("rust-analyzer")
                .args(["--stdio"])
                .current_dir(root_path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context("Failed to start rust-analyzer")?,
            Language::Python => Command::new("pyright-langserver")
                .args(["--stdio"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start pyright-langserver")?,
            Language::JavaScript | Language::TypeScript => {
                Command::new("typescript-language-server")
                    .args(["--stdio"])
                    .current_dir(root_path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("Failed to start typescript-language-server")?
            }
            Language::Go => Command::new("gopls")
                .args(["serve", "-rpc.trace", "--debug=localhost:6060"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start gopls")?,
            Language::Java => Command::new("jdtls")
                .args(["--stdio"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start jdtls")?,
            Language::CSharp => Command::new("omnisharp-lsp")
                .args(["--stdio"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start omnisharp-lsp")?,
            Language::CPlusPlus => Command::new("clangd")
                .args(["--stdio"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start clangd")?,
            Language::Ruby => Command::new("solargraph")
                .args(["stdio"])
                .current_dir(root_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to start solargraph")?,
        };

        Ok(command)
    }

    /// Get the language ID string for the given language
    fn get_language_id(language: &Language) -> &'static str {
        match language {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Go => "go",
            Language::Java => "java",
            Language::CSharp => "csharp",
            Language::CPlusPlus => "cpp",
            Language::Ruby => "ruby",
        }
    }
}

impl Drop for LSPClient {
    fn drop(&mut self) {
        // Kill the server process if it's still running
        let mut handle = self.server_handle.lock().unwrap();
        if handle.take().is_some() {
            // We only have the process ID, not the Child struct
            // In a real implementation, you would use the process ID to kill the process
            // For now, we'll just log that we would kill it
            debug!("Would kill LSP server process");
        }
    }
}
