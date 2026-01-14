#![allow(clippy::unwrap_used)]
use crate::errors::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Minimal file cache for MCP core tools
#[derive(Debug, Default)]
pub struct FileCacheInner {
    pub read_ranges: HashMap<PathBuf, Vec<(usize, usize)>>,
    pub file_hashes: HashMap<PathBuf, String>,
}

#[derive(Debug, Clone, Default)]
pub struct FileCache {
    inner: Arc<Mutex<FileCacheInner>>,
}

impl FileCache {
    pub fn global() -> &'static Self {
        static CACHE: OnceLock<FileCache> = OnceLock::new();
        CACHE.get_or_init(FileCache::default)
    }

    pub fn record_read_range(&self, path: &Path, start: usize, end: usize) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.read_ranges.entry(path.to_path_buf()).or_default().push((start, end));
        Ok(())
    }

    pub fn get_cached_hash(&self, path: &Path) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.file_hashes.get(path).cloned()
    }

    pub fn get_unread_ranges(&self, _path: &Path) -> Vec<(usize, usize)> {
        // Simple placeholder for now
        vec![]
    }

    pub fn record_file_edit(&self, path: &Path) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.file_hashes.remove(path);
        Ok(())
    }

    pub fn record_file_write(&self, path: &Path) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.file_hashes.remove(path);
        Ok(())
    }
}
