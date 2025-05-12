//! File cache management module.
//!
//! This module provides functionality for caching file content and metadata,
//! allowing for more efficient file operations when the same file is accessed
//! multiple times.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::utils::repo::WorkspaceStats;

/// Maximum number of files to keep in the cache
const MAX_CACHE_ENTRIES: usize = 100;
/// Maximum size of a cached file in bytes (10MB)
const MAX_CACHED_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Cache entry expiration time (30 minutes)
#[allow(dead_code)]
const CACHE_EXPIRATION: Duration = Duration::from_secs(30 * 60);

/// File statistics to track usage patterns
#[derive(Debug, Clone, Default)]
pub struct FileStats {
    /// Number of times the file has been read
    pub read_count: usize,

    /// Number of times the file has been edited
    pub edit_count: usize,

    /// Number of times the file has been written
    pub write_count: usize,

    /// First time the file was accessed
    pub first_accessed: Option<SystemTime>,

    /// Last time the file was accessed
    pub last_accessed: Option<SystemTime>,

    /// Importance score (calculated based on access patterns)
    pub importance_score: f64,
}

impl FileStats {
    /// Create a new FileStats instance
    pub fn new() -> Self {
        Self {
            first_accessed: Some(SystemTime::now()),
            last_accessed: Some(SystemTime::now()),
            ..Default::default()
        }
    }

    /// Increment read count and update access time
    pub fn increment_read(&mut self) {
        self.read_count += 1;
        self.update_access_time();
    }

    /// Increment edit count and update access time
    pub fn increment_edit(&mut self) {
        self.edit_count += 1;
        self.update_access_time();
    }

    /// Increment write count and update access time
    pub fn increment_write(&mut self) {
        self.write_count += 1;
        self.update_access_time();
    }

    /// Update the access time
    fn update_access_time(&mut self) {
        let now = SystemTime::now();

        if self.first_accessed.is_none() {
            self.first_accessed = Some(now);
        }

        self.last_accessed = Some(now);

        // Update importance score
        self.recalculate_importance();
    }

    /// Recalculate importance score based on access patterns
    fn recalculate_importance(&mut self) {
        let base_score = (self.read_count as f64 * 0.2)
            + (self.edit_count as f64 * 2.0)
            + (self.write_count as f64 * 1.5);

        // Apply a recency factor if we have access times
        if let Some(last_access) = self.last_accessed {
            if let Ok(duration) = last_access.elapsed() {
                // Reduce importance for files not accessed recently
                // But never below 20% of its base value
                let seconds = duration.as_secs() as f64;
                let recency_factor = (1.0 / (1.0 + seconds / 86400.0)).max(0.2); // 86400 = seconds in a day

                self.importance_score = base_score * recency_factor;
                return;
            }
        }

        // Default case if we can't calculate recency
        self.importance_score = base_score;
    }

    /// Get recent access flag - true if accessed in the last hour
    pub fn is_recently_accessed(&self) -> bool {
        if let Some(last_access) = self.last_accessed {
            if let Ok(duration) = last_access.elapsed() {
                return duration < Duration::from_secs(3600); // 1 hour
            }
        }

        false
    }
}

/// Metadata for a cached file
#[derive(Debug, Clone)]
struct FileCacheEntry {
    /// Path to the file
    #[allow(dead_code)]
    path: PathBuf,

    /// SHA-256 hash of the file content
    hash: String,

    /// File size in bytes
    size: u64,

    /// Last modification time of the file
    last_modified: SystemTime,

    /// Time when this entry was last accessed
    last_accessed: SystemTime,

    /// File content (only cached for small files)
    content: Option<Vec<u8>>,

    /// Indicates if the entire file has been read
    fully_read: bool,

    /// Tracks which line ranges have been read
    read_ranges: Vec<(usize, usize)>,

    /// Total number of lines in the file
    total_lines: usize,

    /// Extended file statistics
    stats: FileStats,
}

impl FileCacheEntry {
    /// Create a new cache entry
    fn new(
        path: PathBuf,
        hash: String,
        size: u64,
        last_modified: SystemTime,
        content: Option<Vec<u8>>,
        total_lines: usize,
    ) -> Self {
        Self {
            path,
            hash,
            size,
            last_modified,
            last_accessed: SystemTime::now(),
            content,
            fully_read: false,
            read_ranges: Vec::new(),
            total_lines,
            stats: FileStats::new(),
        }
    }

    /// Update the last accessed time
    fn touch(&mut self) {
        self.last_accessed = SystemTime::now();
        self.stats.increment_read();
    }

    /// Record a file edit operation
    fn record_edit(&mut self) {
        self.stats.increment_edit();
    }

    /// Record a file write operation
    fn record_write(&mut self) {
        self.stats.increment_write();
    }

    /// Check if this entry is expired
    #[allow(dead_code)]
    fn is_expired(&self) -> bool {
        match self.last_accessed.elapsed() {
            Ok(elapsed) => elapsed > CACHE_EXPIRATION,
            Err(_) => false, // Time went backwards, not expired
        }
    }

    /// Add a range of lines that have been read
    fn add_read_range(&mut self, start: usize, end: usize) {
        // Update fully_read flag if this range covers the entire file
        if start <= 1 && end >= self.total_lines {
            self.fully_read = true;
            self.read_ranges = vec![(1, self.total_lines)];
            return;
        }

        // Add the new range
        self.read_ranges.push((start, end));

        // Merge overlapping ranges
        if self.read_ranges.len() > 1 {
            self.read_ranges.sort_by_key(|&(start, _)| start);

            let mut merged_ranges = Vec::new();
            let mut current = self.read_ranges[0];

            for &(start, end) in &self.read_ranges[1..] {
                if start <= current.1 + 1 {
                    // Ranges overlap or are adjacent
                    current.1 = current.1.max(end);
                } else {
                    // No overlap
                    merged_ranges.push(current);
                    current = (start, end);
                }
            }

            merged_ranges.push(current);
            self.read_ranges = merged_ranges;

            // Check if the entire file has been read
            if self.read_ranges.len() == 1
                && self.read_ranges[0].0 <= 1
                && self.read_ranges[0].1 >= self.total_lines
            {
                self.fully_read = true;
            }
        }
    }

    /// Check if a specific line has been read
    #[allow(dead_code)]
    fn is_line_read(&self, line: usize) -> bool {
        self.fully_read
            || self
                .read_ranges
                .iter()
                .any(|&(start, end)| line >= start && line <= end)
    }

    /// Get the percentage of the file that has been read
    #[allow(dead_code)]
    fn read_percentage(&self) -> f64 {
        if self.fully_read || self.total_lines == 0 {
            return 100.0;
        }

        let mut lines_read = std::collections::HashSet::new();
        for &(start, end) in &self.read_ranges {
            for line in start..=end {
                lines_read.insert(line);
            }
        }

        (lines_read.len() as f64 / self.total_lines as f64) * 100.0
    }

    /// Check if enough of the file has been read (>=99%)
    #[allow(dead_code)]
    fn is_read_enough(&self) -> bool {
        self.fully_read || self.read_percentage() >= 99.0
    }

    /// Get the ranges of lines that have not been read
    fn get_unread_ranges(&self) -> Vec<(usize, usize)> {
        if self.fully_read || self.total_lines == 0 {
            return Vec::new();
        }

        let mut lines_read = std::collections::HashSet::new();
        for &(start, end) in &self.read_ranges {
            for line in start..=end {
                lines_read.insert(line);
            }
        }

        let mut unread_ranges = Vec::new();
        let mut start_range = None;

        for line in 1..=self.total_lines {
            if !lines_read.contains(&line) {
                if start_range.is_none() {
                    start_range = Some(line);
                }
            } else if let Some(start) = start_range {
                unread_ranges.push((start, line - 1));
                start_range = None;
            }
        }

        if let Some(start) = start_range {
            unread_ranges.push((start, self.total_lines));
        }

        unread_ranges
    }
}

/// A thread-safe file cache
#[derive(Debug, Clone)]
pub struct FileCache {
    entries: Arc<RwLock<HashMap<PathBuf, FileCacheEntry>>>,
    workspace_stats: Arc<RwLock<Option<WorkspaceStats>>>,
}

lazy_static::lazy_static! {
    // Singleton pattern: global file cache instance
    static ref GLOBAL_CACHE: FileCache = FileCache::new();
}

impl FileCache {
    /// Create a new file cache
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            workspace_stats: Arc::new(RwLock::new(None)),
        }
    }

    /// Initialize workspace statistics
    pub fn init_workspace_stats(&self, workspace_path: &Path) -> Result<()> {
        let mut stats = WorkspaceStats::new();
        stats.refresh(workspace_path)?;

        if let Ok(mut ws_guard) = self.workspace_stats.write() {
            *ws_guard = Some(stats);
            info!(
                "Initialized workspace statistics for {}",
                workspace_path.display()
            );
        } else {
            warn!("Failed to acquire write lock for workspace stats");
        }

        Ok(())
    }

    /// Get workspace statistics
    pub fn get_workspace_stats(&self) -> Option<WorkspaceStats> {
        if let Ok(ws_guard) = self.workspace_stats.read() {
            ws_guard.clone()
        } else {
            None
        }
    }

    /// Get the global cache instance
    pub fn global() -> &'static FileCache {
        &GLOBAL_CACHE
    }

    /// Clear all entries from the cache
    #[allow(dead_code)]
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    /// Remove expired entries from the cache
    #[allow(dead_code)]
    pub fn cleanup(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|_, entry| !entry.is_expired());
        }
    }

    /// Get metadata for a file, checking the cache first
    #[allow(dead_code)]
    pub fn get_metadata(&self, path: &Path) -> Result<fs::Metadata> {
        // Check if we have a cached entry
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                // Check if the file has been modified
                if let Ok(metadata) = fs::metadata(path) {
                    if metadata.modified()? == entry.last_modified {
                        return Ok(metadata);
                    }
                }
            }
        }

        // Not in cache or modified, get fresh metadata
        fs::metadata(path).with_context(|| format!("Failed to get metadata for {}", path.display()))
    }

    /// Calculate the SHA-256 hash of a file's contents
    #[allow(dead_code)]
    pub fn calculate_hash(&self, path: &Path) -> Result<String> {
        // Check if we have a cached entry with content
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                // Check if the file has been modified
                if let Ok(metadata) = fs::metadata(path) {
                    if metadata.modified()? == entry.last_modified {
                        return Ok(entry.hash.clone());
                    }
                }
            }
        }

        // Not in cache or modified, calculate hash
        let content = fs::read(path)
            .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;

        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = format!("{:x}", hasher.finalize());

        Ok(hash)
    }

    /// Read a file's contents, using the cache if possible
    pub fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        // Use a two-phase approach to reduce lock contention:
        // 1. Try to get content from cache with read lock
        // 2. If cache miss or file modified, acquire write lock to update cache

        // Phase 1: Try to get from cache with read lock
        let _cached_content: Option<Vec<u8>> = {
            // Scope for read lock
            let result = match self.entries.read() {
                Ok(entries) => {
                    if let Some(entry) = entries.get(path) {
                        // Check if the file has been modified
                        if let Ok(metadata) = fs::metadata(path) {
                            if let Ok(last_mod) = metadata.modified() {
                                if last_mod == entry.last_modified {
                                    // Return cached content if available
                                    if let Some(content) = &entry.content {
                                        debug!("Returning cached content for {}", path.display());
                                        let content_copy = content.clone();
                                        // Update access time in a background thread to avoid blocking
                                        let cache_ref = self.clone();
                                        let path_copy = path.to_path_buf();
                                        std::thread::spawn(move || {
                                            if let Ok(mut entries) = cache_ref.entries.write() {
                                                if let Some(entry) = entries.get_mut(&path_copy) {
                                                    entry.touch();
                                                }
                                            }
                                        });
                                        return Ok(content_copy);
                                    }
                                }
                            }
                        }
                    }
                    Ok(None::<(Vec<u8>, PathBuf)>)
                }
                Err(e) => Err(anyhow::anyhow!("Failed to acquire read lock: {}", e)),
            };

            // Handle errors explicitly to avoid nested handling
            match result {
                Ok(None) => None, // Cache miss, continue to read from file
                Err(e) => {
                    debug!("Error reading from cache, continuing to file: {}", e);
                    None
                }
                _ => None, // Other cases handled via early returns
            }
        };

        // If we reach here, we need to read the file and update the cache

        // Phase 2: Read the file and potentially update cache
        // Read the file outside any locks to minimize contention
        let content = match fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to read file {}: {}",
                    path.display(),
                    e
                ))
            }
        };

        // Get metadata for the file (for size and modification time)
        let metadata = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) => {
                // We have the content but couldn't get metadata - not critical
                debug!("Failed to get metadata for {}: {}", path.display(), e);
                return Ok(content); // Return content without caching
            }
        };

        // Update cache with file content if small enough
        if metadata.len() <= MAX_CACHED_FILE_SIZE {
            // Try to update cache but don't fail if we can't
            if let Err(e) = self.update_cache_entry(path, &content, &metadata) {
                debug!("Failed to update cache for {}: {}", path.display(), e);
                // Continue without caching
            }
        } else {
            // For large files, just cache metadata
            let hash = {
                let mut hasher = Sha256::new();
                hasher.update(&content);
                format!("{:x}", hasher.finalize())
            };

            // Count lines - we only do this for large files when needed
            let total_lines = count_lines(&content);

            // Try to update cache but don't block or fail if we can't
            match self.entries.try_write() {
                Ok(mut entries) => {
                    entries.insert(
                        path.to_path_buf(),
                        FileCacheEntry::new(
                            path.to_path_buf(),
                            hash,
                            metadata.len(),
                            metadata.modified().unwrap_or_else(|_| SystemTime::now()),
                            None, // Don't cache content for large files
                            total_lines,
                        ),
                    );

                    // Ensure cache doesn't grow too large
                    if entries.len() > MAX_CACHE_ENTRIES {
                        self.evict_oldest_entries(&mut entries);
                    }
                }
                Err(_) => {
                    // Couldn't get write lock immediately, continue without caching
                    debug!(
                        "Skipping cache update for large file: {} (couldn't get write lock)",
                        path.display()
                    );
                }
            }
        }

        Ok(content)
    }

    /// Update a file's cache entry
    fn update_cache_entry(
        &self,
        path: &Path,
        content: &[u8],
        metadata: &fs::Metadata,
    ) -> Result<()> {
        // Calculate hash
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(content);
            format!("{:x}", hasher.finalize())
        };

        // Count lines
        let total_lines = count_lines(content);

        // Try to get a write lock with timeout to prevent deadlocks
        let write_result = tokio::task::block_in_place(|| {
            use std::time::Duration;
            // Use a timeout for the write lock to prevent long waits
            let timeout = Duration::from_millis(500);
            let start = std::time::Instant::now();

            while start.elapsed() < timeout {
                // Try to get write lock without blocking
                match self.entries.try_write() {
                    Ok(mut entries) => {
                        // Update the cache entry

                        // Check if we're updating an existing entry
                        let mut should_insert = true;

                        if let Some(entry) = entries.get_mut(path) {
                            // Update existing entry
                            entry.hash = hash.clone();
                            entry.size = metadata.len();
                            if let Ok(modified) = metadata.modified() {
                                entry.last_modified = modified;
                            }
                            entry.last_accessed = SystemTime::now();

                            // Only update content if file is small enough
                            if metadata.len() <= MAX_CACHED_FILE_SIZE {
                                entry.content = Some(content.to_vec());
                            } else {
                                entry.content = None;
                            }

                            // Preserve read ranges unless file changed significantly
                            if entry.total_lines != total_lines {
                                entry.read_ranges.clear();
                                entry.fully_read = false;
                            }

                            entry.total_lines = total_lines;

                            should_insert = false;
                        }

                        // Insert new entry if needed
                        if should_insert {
                            let modified =
                                metadata.modified().unwrap_or_else(|_| SystemTime::now());

                            entries.insert(
                                path.to_path_buf(),
                                FileCacheEntry::new(
                                    path.to_path_buf(),
                                    hash,
                                    metadata.len(),
                                    modified,
                                    if metadata.len() <= MAX_CACHED_FILE_SIZE {
                                        Some(content.to_vec())
                                    } else {
                                        None
                                    },
                                    total_lines,
                                ),
                            );

                            // Ensure cache doesn't grow too large
                            if entries.len() > MAX_CACHE_ENTRIES {
                                self.evict_oldest_entries(&mut entries);
                            }
                        }

                        return Ok(());
                    }
                    Err(_) => {
                        // Couldn't get lock, sleep briefly and retry
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }

            // Could not get lock within timeout
            Err(anyhow::anyhow!("Timed out waiting for write lock"))
        });

        // Log errors but don't fail the operation
        if let Err(e) = write_result {
            debug!("Failed to update cache entry for {}: {}", path.display(), e);
        }

        Ok(())
    }

    /// Evict the oldest entries from the cache to maintain size limit
    fn evict_oldest_entries(&self, entries: &mut HashMap<PathBuf, FileCacheEntry>) {
        // Sort entries by last access time
        let mut entry_times: Vec<_> = entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.last_accessed))
            .collect();

        entry_times.sort_by(|a, b| a.1.cmp(&b.1));

        // Remove oldest entries
        let to_remove = entry_times.len().saturating_sub(MAX_CACHE_ENTRIES / 2);
        for (path, _) in entry_times.into_iter().take(to_remove) {
            entries.remove(&path);
        }
    }

    /// Record that a range of lines has been read from a file
    pub fn record_read_range(&self, path: &Path, start: usize, end: usize) -> Result<()> {
        // Try to update with write lock first
        let update_result = self.try_update_read_range(path, start, end);

        if update_result.is_err() {
            // If we couldn't update, the file might not be in cache yet
            // Read the file first to ensure it's cached
            let _ = self.read_file(path)?;

            // Try once more to update the read range
            let _ = self.try_update_read_range(path, start, end);
        }

        // Always return success - failing to track read ranges is not critical
        Ok(())
    }

    /// Try to update read range without blocking
    fn try_update_read_range(&self, path: &Path, start: usize, end: usize) -> Result<()> {
        // Try to get a write lock with timeout
        tokio::task::block_in_place(|| {
            use std::time::Duration;
            // Use a shorter timeout since this is a frequent operation
            let timeout = Duration::from_millis(200);
            let start_time = std::time::Instant::now();

            while start_time.elapsed() < timeout {
                // Try to get write lock without blocking
                match self.entries.try_write() {
                    Ok(mut entries) => {
                        if let Some(entry) = entries.get_mut(path) {
                            entry.add_read_range(start, end);
                            entry.touch();

                            // Also update workspace stats if available
                            self.update_workspace_stats_file_access(path);

                            return Ok(());
                        } else {
                            return Err(anyhow::anyhow!("File not in cache"));
                        }
                    }
                    Err(_) => {
                        // Couldn't get lock, sleep briefly and retry
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }

            // Could not get lock within timeout
            Err(anyhow::anyhow!("Timed out waiting for write lock"))
        })
    }

    /// Record a file edit operation
    pub fn record_file_edit(&self, path: &Path) -> Result<()> {
        // Update the cache entry
        if let Ok(mut entries) = self.entries.write() {
            if let Some(entry) = entries.get_mut(path) {
                entry.record_edit();
            }
        }

        // Update workspace stats
        self.update_workspace_stats_file_edit(path);

        Ok(())
    }

    /// Record a file write operation
    pub fn record_file_write(&self, path: &Path) -> Result<()> {
        // Update the cache entry
        if let Ok(mut entries) = self.entries.write() {
            if let Some(entry) = entries.get_mut(path) {
                entry.record_write();
            }
        }

        // Update workspace stats
        self.update_workspace_stats_file_edit(path);

        Ok(())
    }

    /// Update workspace stats with file access
    fn update_workspace_stats_file_access(&self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();

        if let Ok(mut ws_guard) = self.workspace_stats.write() {
            if let Some(stats) = ws_guard.as_mut() {
                stats.record_file_access(&path_str);
            }
        }
    }

    /// Update workspace stats with file edit
    fn update_workspace_stats_file_edit(&self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();

        if let Ok(mut ws_guard) = self.workspace_stats.write() {
            if let Some(stats) = ws_guard.as_mut() {
                stats.record_file_edit(&path_str);
            }
        }
    }

    /// Get file statistics for a particular file
    pub fn get_file_stats(&self, path: &Path) -> Option<FileStats> {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                return Some(entry.stats.clone());
            }
        }

        None
    }

    /// Get the most active files based on their statistics
    pub fn get_most_active_files(&self, limit: usize) -> Vec<(PathBuf, FileStats)> {
        let mut results = Vec::new();

        if let Ok(entries) = self.entries.read() {
            // Collect all entries with their stats
            let mut file_stats: Vec<_> = entries
                .iter()
                .map(|(path, entry)| (path.clone(), entry.stats.clone()))
                .collect();

            // Sort by importance score (highest first)
            file_stats.sort_by(|a, b| {
                b.1.importance_score
                    .partial_cmp(&a.1.importance_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            // Return top entries
            results = file_stats.into_iter().take(limit).collect();
        }

        results
    }

    /// Check if a file has been fully read
    #[allow(dead_code)]
    pub fn is_file_fully_read(&self, path: &Path) -> bool {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                return entry.fully_read;
            }
        }

        false
    }

    /// Check if a file has been read enough (>=99%)
    #[allow(dead_code)]
    pub fn is_file_read_enough(&self, path: &Path) -> bool {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                return entry.is_read_enough();
            }
        }

        false
    }

    /// Get the unread ranges for a file
    pub fn get_unread_ranges(&self, path: &Path) -> Vec<(usize, usize)> {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                return entry.get_unread_ranges();
            }
        }

        if let Ok(metadata) = fs::metadata(path) {
            if metadata.is_file() {
                // File exists but not in cache, consider the whole file unread
                if let Ok(content) = fs::read(path) {
                    let total_lines = count_lines(&content);
                    if total_lines > 0 {
                        return vec![(1, total_lines)];
                    }
                }
            }
        }

        Vec::new()
    }

    /// Check if a file has changed since it was last cached
    #[allow(dead_code)]
    pub fn has_file_changed(&self, path: &Path) -> Result<bool> {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                if let Ok(metadata) = fs::metadata(path) {
                    let current_modified = metadata.modified()?;
                    return Ok(current_modified != entry.last_modified);
                }
            }
        }

        // Not in cache or can't get metadata, consider changed
        Ok(true)
    }

    /// Get the hash for a file from the cache
    pub fn get_cached_hash(&self, path: &Path) -> Option<String> {
        if let Ok(entries) = self.entries.read() {
            if let Some(entry) = entries.get(path) {
                return Some(entry.hash.clone());
            }
        }

        None
    }

    /// Check if a file can be safely written to (it's been read enough)
    #[allow(dead_code)]
    pub fn can_write_file(&self, path: &Path) -> Result<bool> {
        // If file doesn't exist, it can be written
        if !path.exists() {
            return Ok(true);
        }

        // Check if file has been read enough
        if !self.is_file_read_enough(path) {
            return Ok(false);
        }

        // Check if file has changed since last read
        if self.has_file_changed(path)? {
            return Ok(false);
        }

        Ok(true)
    }

    /// Get error details about why a file can't be written
    #[allow(dead_code)]
    pub fn get_write_error_details(&self, path: &Path) -> Result<String> {
        // If file doesn't exist, no error
        if !path.exists() {
            return Ok("File can be written".to_string());
        }

        // Check if file has been read enough
        if !self.is_file_read_enough(path) {
            let unread_ranges = self.get_unread_ranges(path);
            let ranges_str = unread_ranges
                .iter()
                .map(|(start, end)| format!("{}-{}", start, end))
                .collect::<Vec<_>>()
                .join(", ");

            return Ok(format!(
                "You need to read more of the file before it can be overwritten. Unread line ranges: {}",
                ranges_str
            ));
        }

        // Check if file has changed
        if self.has_file_changed(path)? {
            return Ok("The file has changed since it was last read".to_string());
        }

        Ok("File can be written".to_string())
    }
}

/// Count the number of lines in a byte array
fn count_lines(content: &[u8]) -> usize {
    // Count newlines and add 1 if there's content after the last newline
    let newline_count = content.iter().filter(|&&b| b == b'\n').count();

    if content.is_empty() || content[content.len() - 1] == b'\n' {
        newline_count
    } else {
        newline_count + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_temp_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_file_cache_basic() {
        let cache = FileCache::new();
        let content = "Line 1\nLine 2\nLine 3\n";
        let file = create_temp_file(content);
        let path = file.path();

        // Read the file
        let read_content = cache.read_file(path).unwrap();
        assert_eq!(read_content, content.as_bytes());

        // Should be cached now
        assert!(cache.entries.read().unwrap().contains_key(path));
    }

    #[test]
    fn test_read_ranges() {
        let cache = FileCache::new();
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n";
        let file = create_temp_file(content);
        let path = file.path();

        // Read the file
        let _ = cache.read_file(path).unwrap();

        // Record some reads
        cache.record_read_range(path, 1, 2).unwrap(); // Read lines 1-2

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert_eq!(entry.read_ranges, vec![(1, 2)]);
        assert!(entry.is_line_read(1));
        assert!(entry.is_line_read(2));
        assert!(!entry.is_line_read(3));

        // Read more lines
        cache.record_read_range(path, 4, 5).unwrap(); // Read lines 4-5

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert_eq!(entry.read_ranges, vec![(1, 2), (4, 5)]);

        // Get unread ranges
        let unread = cache.get_unread_ranges(path);
        assert_eq!(unread, vec![(3, 3)]);

        // Read the rest
        cache.record_read_range(path, 3, 3).unwrap();

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert!(entry.fully_read);

        // No more unread ranges
        let unread = cache.get_unread_ranges(path);
        assert!(unread.is_empty());
    }

    #[test]
    fn test_read_percentage() {
        let cache = FileCache::new();
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n";
        let file = create_temp_file(content);
        let path = file.path();

        // Read the file
        let _ = cache.read_file(path).unwrap();

        // Record some reads
        cache.record_read_range(path, 1, 2).unwrap(); // Read lines 1-2

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert_eq!(entry.read_percentage(), 40.0); // 2/5 = 40%

        // Read more lines
        cache.record_read_range(path, 4, 5).unwrap(); // Read lines 4-5

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert_eq!(entry.read_percentage(), 80.0); // 4/5 = 80%

        // Read everything
        cache.record_read_range(path, 1, 5).unwrap();

        let entry = cache.entries.read().unwrap().get(path).unwrap().clone();
        assert_eq!(entry.read_percentage(), 100.0);
    }

    #[test]
    fn test_can_write_file() {
        let cache = FileCache::new();
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n";
        let file = create_temp_file(content);
        let path = file.path();

        // Read the file
        let _ = cache.read_file(path).unwrap();

        // Record some reads
        cache.record_read_range(path, 1, 2).unwrap(); // Read lines 1-2

        // Not read enough
        assert!(!cache.can_write_file(path).unwrap());

        // Read enough of the file
        cache.record_read_range(path, 1, 5).unwrap(); // Read all lines

        // Now should be writable
        assert!(cache.can_write_file(path).unwrap());
    }
}
