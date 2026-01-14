//! Application state and main loop
//!
//! Core TUI application struct with async ChatEngine integration.

use std::time::{Duration, Instant};

use color_eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
use tokio::sync::mpsc;

use super::terminal::{self, Tui};
use super::ui;
use crate::chat::{ChatConfig, ChatEngine};
use crate::interactive::i18n::Language;
use crate::interactive::status::Phase;
use crate::providers::StreamEvent;

/// Application mode (vim-style)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    /// Normal mode - navigation, commands
    #[default]
    Normal,
    /// Insert mode - typing message
    Insert,
    /// Streaming mode - receiving AI response
    Streaming,
}

/// Message in the chat
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: Instant,
}

/// Role of a message sender
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            timestamp: Instant::now(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            timestamp: Instant::now(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            timestamp: Instant::now(),
        }
    }
}

/// Events from async tasks
#[derive(Debug)]
pub enum AsyncEvent {
    StreamChunk(String),
    StreamDone,
    StreamError(String),
}

/// Main TUI application state
pub struct App {
    /// Should the app exit?
    pub should_quit: bool,
    /// Current mode
    pub mode: AppMode,
    /// Current phase (connecting, thinking, etc.)
    pub phase: Phase,
    /// Language for i18n
    pub lang: Language,

    /// Chat messages
    pub messages: Vec<ChatMessage>,
    /// Scroll position in chat (0 = bottom)
    pub scroll: u16,

    /// Input buffer
    pub input: String,
    /// Cursor position in input
    pub cursor: usize,

    /// Model name
    pub model: String,
    /// Provider name
    pub provider: String,

    /// Last tick time (for animations)
    pub last_tick: Instant,
    /// Spinner frame index
    pub spinner_frame: usize,

    /// Async event receiver
    async_rx: Option<mpsc::UnboundedReceiver<AsyncEvent>>,
    /// Current streaming content buffer
    streaming_buffer: String,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Tick rate for animations (16ms = ~60fps)
    const TICK_RATE: Duration = Duration::from_millis(16);

    /// Spinner characters
    const SPINNER: &'static [&'static str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    /// Create a new App
    pub fn new() -> Self {
        // Get default provider/model from config
        let config = ChatConfig::sensible_defaults();
        let provider = config.default_provider.clone().unwrap_or_else(|| "nvidia".to_string());
        let model = config.default_model.clone().unwrap_or_else(|| "qwen3-235b".to_string());

        Self {
            should_quit: false,
            mode: AppMode::Insert,
            phase: Phase::Idle,
            lang: Language::detect(),

            messages: vec![ChatMessage::system(
                "Winx TUI - Digite sua mensagem e pressione Enter.",
            )],
            scroll: 0,

            input: String::new(),
            cursor: 0,

            model,
            provider,

            last_tick: Instant::now(),
            spinner_frame: 0,

            async_rx: None,
            streaming_buffer: String::new(),
        }
    }

    /// Run the main event loop
    pub fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        terminal::set_title("Winx");

        while !self.should_quit {
            // Process async events first
            self.process_async_events();

            // Draw the UI
            terminal.draw(|frame| self.draw(frame))?;

            // Handle events with timeout for animations
            let timeout = Self::TICK_RATE.saturating_sub(self.last_tick.elapsed());

            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) => self.handle_key(key)?,
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            // Update animations on tick
            if self.last_tick.elapsed() >= Self::TICK_RATE {
                self.on_tick();
                self.last_tick = Instant::now();
            }
        }

        Ok(())
    }

    /// Process async events from streaming
    fn process_async_events(&mut self) {
        if let Some(ref mut rx) = self.async_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AsyncEvent::StreamChunk(chunk) => {
                        self.streaming_buffer.push_str(&chunk);
                        self.phase = Phase::Streaming;
                        // Update the last message if it's from assistant
                        if let Some(msg) = self.messages.last_mut() {
                            if msg.role == MessageRole::Assistant {
                                msg.content = self.streaming_buffer.clone();
                            }
                        }
                    }
                    AsyncEvent::StreamDone => {
                        self.phase = Phase::Done;
                        self.mode = AppMode::Insert;
                        self.streaming_buffer.clear();
                        terminal::send_notification("Winx", "Resposta completa");
                    }
                    AsyncEvent::StreamError(err) => {
                        self.phase = Phase::Error;
                        self.mode = AppMode::Insert;
                        self.messages.push(ChatMessage::system(format!("Erro: {}", err)));
                        self.streaming_buffer.clear();
                    }
                }
            }
        }
    }

    /// Draw the UI
    fn draw(&mut self, frame: &mut Frame) {
        ui::render(frame, self);
    }

    /// Handle a key event
    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Global shortcuts
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    if self.mode == AppMode::Streaming {
                        self.cancel_streaming();
                    } else {
                        self.should_quit = true;
                    }
                    return Ok(());
                }
                KeyCode::Char('l') => {
                    self.messages.clear();
                    self.scroll = 0;
                    return Ok(());
                }
                _ => {}
            }
        }

        match self.mode {
            AppMode::Normal => self.handle_normal_key(key),
            AppMode::Insert => self.handle_insert_key(key),
            AppMode::Streaming => Ok(()),
        }
    }

    /// Handle key in Normal mode
    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('i') | KeyCode::Char('a') => self.mode = AppMode::Insert,
            KeyCode::Char('j') | KeyCode::Down => self.scroll_down(),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_up(),
            KeyCode::Char('G') => self.scroll_to_bottom(),
            KeyCode::Char('g') => self.scroll_to_top(),
            KeyCode::Char('y') => self.copy_last_response()?,
            KeyCode::Char('r') => self.regenerate()?,
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
        Ok(())
    }

    /// Handle key in Insert mode
    fn handle_insert_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Normal,
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.cursor, '\n');
                    self.cursor += 1;
                } else if !self.input.trim().is_empty() {
                    self.send_message()?;
                }
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
            }
            _ => {}
        }
        Ok(())
    }

    fn scroll_down(&mut self) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    fn scroll_to_top(&mut self) {
        self.scroll = u16::MAX;
    }

    /// Send the current input as a message
    fn send_message(&mut self) -> Result<()> {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;

        // Add user message
        self.messages.push(ChatMessage::user(&text));
        self.scroll_to_bottom();

        // Set up streaming state
        self.phase = Phase::Connecting;
        self.mode = AppMode::Streaming;

        // Create placeholder for assistant response
        self.messages.push(ChatMessage::assistant(""));
        self.streaming_buffer.clear();

        // Create channel for async events
        let (tx, rx) = mpsc::unbounded_channel();
        self.async_rx = Some(rx);

        // Spawn async task for streaming
        let message = text.clone();
        tokio::spawn(async move {
            let config = ChatConfig::sensible_defaults();
            let engine = ChatEngine::new(config);

            match engine.one_shot_stream(&message).await {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        match event {
                            StreamEvent::Text(chunk) => {
                                let _ = tx.send(AsyncEvent::StreamChunk(chunk));
                            }
                            StreamEvent::Done => {
                                let _ = tx.send(AsyncEvent::StreamDone);
                                break;
                            }
                            StreamEvent::Error(err) => {
                                let _ = tx.send(AsyncEvent::StreamError(err));
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::StreamError(e.to_string()));
                }
            }
        });

        Ok(())
    }

    /// Cancel ongoing streaming
    fn cancel_streaming(&mut self) {
        self.async_rx = None;
        self.phase = Phase::Idle;
        self.mode = AppMode::Insert;
        self.streaming_buffer.clear();
    }

    /// Copy last response to clipboard
    fn copy_last_response(&mut self) -> Result<()> {
        if let Some(msg) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
        {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(&msg.content);
            }
        }
        Ok(())
    }

    /// Regenerate last response
    fn regenerate(&mut self) -> Result<()> {
        // Find last user message
        if let Some(last_user_msg) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.content.clone())
        {
            // Remove last assistant response if exists
            if let Some(last) = self.messages.last() {
                if last.role == MessageRole::Assistant {
                    self.messages.pop();
                }
            }

            // Re-send the message
            self.input = last_user_msg;
            self.cursor = self.input.len();
            self.send_message()?;
        }
        Ok(())
    }

    fn on_tick(&mut self) {
        if self.phase.is_active() {
            self.spinner_frame = (self.spinner_frame + 1) % Self::SPINNER.len();
        }
    }

    pub fn spinner_char(&self) -> &'static str {
        Self::SPINNER[self.spinner_frame]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_new() {
        let app = App::new();
        assert!(!app.should_quit);
        assert_eq!(app.mode, AppMode::Insert);
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn test_chat_message() {
        let user = ChatMessage::user("hello");
        assert_eq!(user.role, MessageRole::User);
        assert_eq!(user.content, "hello");

        let assistant = ChatMessage::assistant("hi");
        assert_eq!(assistant.role, MessageRole::Assistant);
    }

    #[test]
    fn test_scroll() {
        let mut app = App::new();
        app.scroll = 5;

        app.scroll_down();
        assert_eq!(app.scroll, 4);

        app.scroll_up();
        assert_eq!(app.scroll, 5);

        app.scroll_to_bottom();
        assert_eq!(app.scroll, 0);
    }
}
