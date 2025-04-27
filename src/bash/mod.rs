use crate::error::{WinxError, WinxResult};
use crate::types::Mode;
use chrono::{DateTime, Utc};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

pub mod state;
pub use state::*;

/// Console trait to log messages
pub trait Console: Send + Sync {
    /// Print a message to the console
    fn print(&self, msg: &str);

    /// Log a message to the console
    fn log(&self, msg: &str);
}

/// Simple console implementation that logs to stdout
pub struct SimpleConsole;

impl Console for SimpleConsole {
    fn print(&self, msg: &str) {
        println!("{}", msg);
    }

    fn log(&self, msg: &str) {
        println!("{}", msg);
    }
}

/// FileWhitelistData struct to track file access
#[derive(Debug, Clone)]
pub struct FileWhitelistData {
    /// The hash of the file contents
    pub file_hash: String,

    /// The ranges of lines that have been read
    pub line_ranges_read: Vec<(usize, usize)>,

    /// The total number of lines in the file
    pub total_lines: usize,
}

impl FileWhitelistData {
    /// Create a new FileWhitelistData instance
    pub fn new(
        file_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
    ) -> Self {
        Self {
            file_hash,
            line_ranges_read,
            total_lines,
        }
    }

    /// Calculate the percentage of the file that has been read
    pub fn get_percentage_read(&self) -> f64 {
        if self.total_lines == 0 {
            return 100.0;
        }

        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }

        (lines_read.len() as f64 / self.total_lines as f64) * 100.0
    }

    /// Check if enough of the file has been read (>=99%)
    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= 99.0
    }

    /// Get ranges of lines that haven't been read yet
    pub fn get_unread_ranges(&self) -> Vec<(usize, usize)> {
        if self.total_lines == 0 {
            return vec![];
        }

        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }

        let mut unread_ranges = vec![];
        let mut start_range = None;

        for i in 1..=self.total_lines {
            if !lines_read.contains(&i) {
                if start_range.is_none() {
                    start_range = Some(i);
                }
            } else if let Some(start) = start_range {
                unread_ranges.push((start, i - 1));
                start_range = None;
            }
        }

        if let Some(start) = start_range {
            unread_ranges.push((start, self.total_lines));
        }

        unread_ranges
    }

    /// Add a new range of lines that have been read
    pub fn add_range(&mut self, start: usize, end: usize) {
        self.line_ranges_read.push((start, end));
    }
}

/// Generate a random chat ID
pub fn generate_chat_id() -> String {
    let mut rng = rand::rng();
    format!("i{}", rng.random_range(1000..=9999))
}

/// Context struct to pass around bash state
pub struct Context {
    /// The bash state
    pub bash_state: Arc<Mutex<BashState>>,
}

impl Context {
    /// Create a new Context with the given BashState
    pub fn new(bash_state: BashState) -> Self {
        Self {
            bash_state: Arc::new(Mutex::new(bash_state)),
        }
    }
}

/// Expand ~ in paths to the user's home directory
pub fn expand_user(path: &str) -> String {
    if path.starts_with("~") {
        if let Some(home_dir) = home::home_dir() {
            return path.replacen("~", home_dir.to_string_lossy().as_ref(), 1);
        }
    }
    path.to_string()
}
