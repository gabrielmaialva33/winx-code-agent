//! File cache management module.
//!
//! This module provides functionality for caching file content and metadata,
//! allowing for more efficient file operations when the same file is accessed
//! multiple times.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};

use crate::utils::repo::WorkspaceStats;

/// Maximum number of files to keep in the cache
const MAX_CACHE_ENTRIES: usize = 100;
/// Maximum size of a cached file in bytes (10MB)
const MAX_CACHED_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Maximum total memory budget for file content caching (default: 100MB)
const DEFAULT_MEMORY_BUDGET: u64 = 100 * 1024 * 1024;
/// Cache entry expiration time (30 minutes)
#[allow(dead_code)]
const CACHE_EXPIRATION: Duration = Duration::from_secs(30 * 60);
/// How often to check for cache eviction (every 10 seconds)
const EVICTION_CHECK_INTERVAL: Duration = Duration::from_secs(10);

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
        if let Some(last_access) = self.last_accessed
            && let Ok(duration) = last_access.elapsed()
        {
            // Reduce importance for files not accessed recently
            // But never below 20% of its base value
            let seconds = duration.as_secs() as f64;
            let recency_factor = (1.0 / (1.0 + seconds / 86400.0)).max(0.2); // 86400 = seconds in a day

            self.importance_score = base_score * recency_factor;
            return;
        }

        // Default case if we can't calculate recency
        self.importance_score = base_score;
    }

    /// Get recent access flag - true if accessed in the last hour
    pub fn is_recently_accessed(&self) -> bool {
        if let Some(last_access) = self.last_accessed
            && let Ok(duration) = last_access.elapsed()
        {
            return duration < Duration::from_secs(3600); // 1 hour
        }

        false
    }
}

/// Content loading strategy for file cache entries
#[derive(Debug, Clone, PartialEq)]
enum ContentLoadingStrategy {
    /// Always cache content in memory if within size limit
    AlwaysCache,
    /// Only load content on demand and discard after use (for large files)
    OnDemand,
    /// Only keep metadata, never cache content (for very large files)
    MetadataOnly,
    /// Only cache frequently accessed file segments (memory-mapped)
    SegmentedCache,
}

/// Metadata for a cached file with memory optimization
#[derive(Debug, Clone)]
struct FileCacheEntry {
    /// Path to the file
    path: PathBuf,

    /// SHA-256 hash of the file content
    hash: String,

    /// File size in bytes
    size: u64,

    /// Last modification time of the file
    last_modified: SystemTime,

    /// Time when this entry was last accessed
    last_accessed: SystemTime,

    /// File content (only cached for small files or frequently accessed segments)
    content: Option<Vec<u8>>,

    /// Content loading strategy based on file size and access patterns
    loading_strategy: ContentLoadingStrategy,

    /// Last time the content was loaded or access (for memory management)
    last_content_access: Instant,

    /// Number of times this entry has been accessed since content was loaded
    access_count_since_load: usize,

    /// Whether the content is currently pinned in memory (higher priority for retention)
    is_pinned: bool,

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
    /// Create a new cache entry with auto-determined loading strategy
    fn new(
        path: PathBuf,
        hash: String,
        size: u64,
        last_modified: SystemTime,
        content: Option<Vec<u8>>,
        total_lines: usize,
    ) -> Self {
        // Determine loading strategy based on file size
        let loading_strategy = if size > MAX_CACHED_FILE_SIZE * 10 {
            // Very large files, never cache
            ContentLoadingStrategy::MetadataOnly
        } else if size > MAX_CACHED_FILE_SIZE {
            // Large files, on-demand loading
            ContentLoadingStrategy::OnDemand
        } else if size > MAX_CACHED_FILE_SIZE / 2 && total_lines > 1000 {
            // Medium files with many lines, use segmented caching
            ContentLoadingStrategy::SegmentedCache
        } else {
            // Small files, always cache
            ContentLoadingStrategy::AlwaysCache
        };

        Self {
            path,
            hash,
            size,
            last_modified,
            last_accessed: SystemTime::now(),
            content,
            loading_strategy,
            last_content_access: Instant::now(),
            access_count_since_load: 0,
            is_pinned: false,
            fully_read: false,
            read_ranges: Vec::new(),
            total_lines,
            stats: FileStats::new(),
        }
    }

    /// Update the last accessed time and access counts
    fn touch(&mut self) {
        self.last_accessed = SystemTime::now();
        self.last_content_access = Instant::now();
        self.access_count_since_load += 1;
        self.stats.increment_read();
    }

    /// Record a file edit operation
    fn record_edit(&mut self) {
        self.stats.increment_edit();
        self.touch();
        // Pin content in memory during edit operations
        self.is_pinned = true;
    }

    /// Record a file write operation
    fn record_write(&mut self) {
        self.stats.increment_write();
        self.touch();
        // Pin content in memory during write operations
        self.is_pinned = true;
    }

    /// Unpin content after operation completes
    fn unpin(&mut self) {
        self.is_pinned = false;
    }

    /// Check if this entry is expired
    fn is_expired(&self) -> bool {
        // Pinned entries never expire
        if self.is_pinned {
            return false;
        }

        match self.last_accessed.elapsed() {
            Ok(elapsed) => elapsed > CACHE_EXPIRATION,
            Err(_) => false, // Time went backwards, not expired
        }
    }

    /// Calculate content memory footprint in bytes
    fn memory_usage(&self) -> u64 {
        match &self.content {
            Some(content) => {
                content.len() as u64 +
                // Approximate overhead for the entry structure
                (std::mem::size_of::<Self>() as u64)
            }
            None => std::mem::size_of::<Self>() as u64,
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

    /// Gets a retention score for memory pressure situations (higher = more likely to keep)
    fn retention_score(&self) -> f64 {
        // Base importance from file stats
        let base_score = self.stats.importance_score;

        // Pinned files get maximum retention
        if self.is_pinned {
            return f64::MAX;
        }

        // Apply recency factor based on elapsed time
        let recency_factor = {
            let elapsed = self.last_content_access.elapsed();
            let seconds = elapsed.as_secs() as f64;
            // Reduces importance logarithmically as time passes
            1.0 / (1.0 + seconds.ln().max(0.0) / 10.0)
        };

        // Apply access count factor - more accesses = higher retention
        let access_factor = (self.access_count_since_load as f64 / 10.0).min(1.0) + 0.5;

        // Apply size penalty - larger files have lower retention priority
        let size_factor = if self.size > 0 {
            1.0 / (1.0 + (self.size as f64 / 1024.0 / 1024.0).ln().max(0.0))
        } else {
            1.0
        };

        // Combine factors
        base_score * recency_factor * access_factor * size_factor
    }

    /// Determine if content should be unloaded based on memory pressure
    fn should_unload(&self, memory_pressure: bool) -> bool {
        // Don't unload pinned content
        if self.is_pinned {
            return false;
        }

        // Check if we have content loaded
        if self.content.is_none() {
            return false;
        }

        match self.loading_strategy {
            // Always retained
            ContentLoadingStrategy::AlwaysCache => false,

            // Always unload after inactivity
            ContentLoadingStrategy::OnDemand => {
                let elapsed = self.last_content_access.elapsed();
                // Unload after 30 seconds of inactivity or immediately under memory pressure
                elapsed > Duration::from_secs(30) || memory_pressure
            }

            // Never load content
            ContentLoadingStrategy::MetadataOnly => true,

            // Retain under moderate pressure, unload under high
            ContentLoadingStrategy::SegmentedCache => {
                if memory_pressure {
                    // Under memory pressure, check access patterns
                    let elapsed = self.last_content_access.elapsed();
                    elapsed > Duration::from_secs(300)
                        || (elapsed > Duration::from_secs(60) && self.access_count_since_load < 5)
                } else {
                    false
                }
            }
        }
    }
}

// Additional FileCacheEntry methods are defined below, after the main impl block

/// A thread-safe file cache with LRU eviction and memory management
#[derive(Debug, Clone)]
pub struct FileCache {
    // File cache entries with path as key
    entries: Arc<RwLock<HashMap<PathBuf, FileCacheEntry>>>,
    // LRU tracking for cache eviction (most recently used at front)
    access_order: Arc<RwLock<VecDeque<PathBuf>>>,
    // Track total memory usage of cached content
    memory_usage: Arc<RwLock<u64>>,
    // Maximum memory budget for content caching
    memory_budget: Arc<RwLock<u64>>,
    // Track when the last eviction check was performed
    last_eviction_check: Arc<RwLock<Instant>>,
    // Workspace statistics for relevance tracking
    workspace_stats: Arc<RwLock<Option<WorkspaceStats>>>,
    // Pinned files that should not be evicted during this session
    pinned_files: Arc<RwLock<HashSet<PathBuf>>>,
}

lazy_static::lazy_static! {
    // Singleton pattern: global file cache instance
    static ref GLOBAL_CACHE: FileCache = FileCache::new();
}

impl Default for FileCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FileCache {
    /// Create a new file cache with memory optimization
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            access_order: Arc::new(RwLock::new(VecDeque::new())),
            memory_usage: Arc::new(RwLock::new(0)),
            memory_budget: Arc::new(RwLock::new(DEFAULT_MEMORY_BUDGET)),
            last_eviction_check: Arc::new(RwLock::new(Instant::now())),
            workspace_stats: Arc::new(RwLock::new(None)),
            pinned_files: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Configure memory budget for content caching (in bytes)
    pub async fn set_memory_budget(&self, budget_bytes: u64) {
        let mut memory_budget = self.memory_budget.write().await;
        *memory_budget = budget_bytes;
        debug!("File cache memory budget set to {} bytes", budget_bytes);
    }

    /// Get current memory usage (in bytes)
    pub async fn get_memory_usage(&self) -> u64 {
        let memory_usage = self.memory_usage.read().await;
        *memory_usage
    }

    /// Pin a file in memory to prevent eviction
    pub async fn pin_file(&self, path: &Path) {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(path) {
            entry.is_pinned = true;
        }

        let mut pinned = self.pinned_files.write().await;
        pinned.insert(path.to_path_buf());
    }

    /// Unpin a file, allowing it to be evicted
    pub async fn unpin_file(&self, path: &Path) {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(path) {
            entry.is_pinned = false;
        }

        let mut pinned = self.pinned_files.write().await;
        pinned.remove(path);
    }

    /// Check if a file is pinned
    pub async fn is_file_pinned(&self, path: &Path) -> bool {
        let pinned = self.pinned_files.read().await;
        pinned.contains(path)
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
    pub fn clear(&self) {
        // Clear entries
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }

        // Clear access order
        if let Ok(mut access_order) = self.access_order.write() {
            access_order.clear();
        }

        // Reset memory usage
        if let Ok(mut memory_usage) = self.memory_usage.write() {
            *memory_usage = 0;
        }

        // Clear pinned files
        if let Ok(mut pinned) = self.pinned_files.write() {
            pinned.clear();
        }

        debug!("File cache cleared");
    }

    /// Remove expired entries from the cache
    pub async fn cleanup(&self) {
        // Track removed entries for memory accounting
        let mut memory_freed = 0;
        let mut removed_paths = Vec::new();

        // Remove expired entries
        if let Ok(mut entries) = self.entries.write() {
            let expired_keys: Vec<PathBuf> = entries
                .iter()
                .filter(|(_, entry)| entry.is_expired())
                .map(|(path, entry)| {
                    // Track memory usage for each expired entry
                    if let Some(content) = &entry.content {
                        memory_freed += content.len() as u64;
                    }
                    path.clone()
                })
                .collect();

            // Remove expired entries
            for key in &expired_keys {
                entries.remove(key);
                removed_paths.push(key.clone());
            }

            debug!("Removed {} expired entries from cache", expired_keys.len());
        }

        // Update memory usage
        if memory_freed > 0
            && let Ok(mut usage) = self.memory_usage.write()
        {
            *usage = usage.saturating_sub(memory_freed);
            debug!("Freed {} bytes from expired cache entries", memory_freed);
        }

        // Update access order list
        if !removed_paths.is_empty()
            && let Ok(mut access_order) = self.access_order.write()
        {
            access_order.retain(|path| !removed_paths.contains(path));
        }
    }

    /// Check memory pressure and perform eviction if needed
    fn check_memory_pressure(&self) -> bool {
        // Only check periodically to avoid lock contention
        let should_check = {
            if let Ok(last_check) = self.last_eviction_check.read() {
                last_check.elapsed() > EVICTION_CHECK_INTERVAL
            } else {
                false
            }
        };

        if !should_check {
            // Return current memory pressure status without checking
            if let (Ok(usage), Ok(budget)) = (self.memory_usage.read(), self.memory_budget.read()) {
                return *usage > (*budget * 9) / 10; // 90% of budget
            }
            return false;
        }

        // Update last check time
        if let Ok(mut last_check) = self.last_eviction_check.write() {
            *last_check = Instant::now();
        }

        // Calculate current memory usage and pressure level
        let (current_usage, budget) = {
            if let (Ok(usage), Ok(budget)) = (self.memory_usage.read(), self.memory_budget.read()) {
                (*usage, *budget)
            } else {
                return false;
            }
        };

        // Define pressure levels
        let low_pressure = budget * 7 / 10; // 70% of budget
        let medium_pressure = budget * 8 / 10; // 80% of budget
        let high_pressure = budget * 9 / 10; // 90% of budget

        let pressure_level = if current_usage > high_pressure {
            debug!(
                "High memory pressure: {} / {} bytes ({}%)",
                current_usage,
                budget,
                (current_usage * 100) / budget
            );
            3 // High pressure
        } else if current_usage > medium_pressure {
            debug!(
                "Medium memory pressure: {} / {} bytes ({}%)",
                current_usage,
                budget,
                (current_usage * 100) / budget
            );
            2 // Medium pressure
        } else if current_usage > low_pressure {
            trace!(
                "Low memory pressure: {} / {} bytes ({}%)",
                current_usage,
                budget,
                (current_usage * 100) / budget
            );
            1 // Low pressure
        } else {
            0 // No pressure
        };

        // Perform eviction based on pressure level
        if pressure_level > 0 {
            self.perform_eviction(pressure_level);
        }

        // Return whether we're under high pressure
        pressure_level >= 3
    }

    /// Perform cache eviction based on pressure level
    fn perform_eviction(&self, pressure_level: u8) {
        // Collect entries that can be evicted at this pressure level
        let mut unload_candidates = Vec::new();
        let mut unload_content = false;
        let mut memory_to_free: u64 = 0;

        // Memory pressure handling strategy:
        // 1. Level 1 (Low): Unload inactive content (MetadataOnly, OnDemand)
        // 2. Level 2 (Medium): Unload SegmentedCache content and some AlwaysCache content
        // 3. Level 3 (High): Aggressive unloading, only keep essential files

        {
            // Read lock for analyzing entries
            if let Ok(entries) = self.entries.read() {
                // If we're at high pressure, first figure out how many entries should be unloaded
                if pressure_level >= 3 {
                    unload_content = true;

                    // Set target to get below 70% of budget
                    if let Ok(budget) = self.memory_budget.read() {
                        let target_usage = (*budget * 7) / 10;
                        if let Ok(usage) = self.memory_usage.read()
                            && *usage > target_usage
                        {
                            memory_to_free = *usage - target_usage;
                        }
                    }
                } else if pressure_level == 2 {
                    unload_content = true;
                }

                // Get access order to prioritize eviction of least recently used files
                let mut access_order_copy = Vec::new();
                if let Ok(access_order) = self.access_order.read() {
                    // Copy from end (oldest) to beginning (newest)
                    for path in access_order.iter().rev() {
                        access_order_copy.push(path.clone());
                    }
                }

                // Only unload content if we're under memory pressure
                if unload_content {
                    // First pass: Collect all possible candidates
                    let mut candidates = Vec::new();

                    // Use the access order when available to prioritize least recently used files
                    if !access_order_copy.is_empty() {
                        for path in access_order_copy {
                            if let Some(entry) = entries.get(&path) {
                                // If this entry can be unloaded based on pressure
                                if entry.should_unload(pressure_level >= 3)
                                    && entry.content.is_some()
                                {
                                    candidates.push((
                                        path,
                                        entry.retention_score(),
                                        entry.memory_usage(),
                                    ));
                                }
                            }
                        }
                    } else {
                        // Fallback if access order is not available
                        for (path, entry) in entries.iter() {
                            if entry.should_unload(pressure_level >= 3) && entry.content.is_some() {
                                candidates.push((
                                    path.clone(),
                                    entry.retention_score(),
                                    entry.memory_usage(),
                                ));
                            }
                        }
                    }

                    // Sort by retention score (lowest first)
                    candidates
                        .sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                    // Level 1: Take only obvious candidates (low retention score)
                    // Level 2: Take more candidates
                    // Level 3: Take as many as needed to reach memory_to_free

                    let mut memory_freed = 0;
                    let count_to_take = if pressure_level == 1 {
                        candidates.len() / 4 // 25% of candidates
                    } else if pressure_level == 2 {
                        candidates.len() / 2 // 50% of candidates
                    } else {
                        candidates.len() // All candidates potentially available
                    };

                    // For high pressure, keep unloading until we reach the target
                    if pressure_level >= 3 && memory_to_free > 0 {
                        for (path, _, size) in &candidates {
                            memory_freed += size;
                            unload_candidates.push(path.clone());

                            if memory_freed >= memory_to_free {
                                break;
                            }
                        }
                    } else {
                        // For lower pressure, just take the designated percentage
                        for (path, _, _) in candidates.iter().take(count_to_take) {
                            unload_candidates.push(path.clone());
                        }
                    }
                }
            }
        }

        // Now perform the actual unloading with a write lock
        if unload_content && !unload_candidates.is_empty() {
            debug!(
                "Evicting {} entries due to memory pressure level {}",
                unload_candidates.len(),
                pressure_level
            );

            if let Ok(mut entries) = self.entries.write() {
                let mut total_freed = 0;

                for path in &unload_candidates {
                    if let Some(entry) = entries.get_mut(path) {
                        // Calculate memory to free
                        if let Some(content) = &entry.content {
                            total_freed += content.len() as u64;
                        }

                        // Remove content but keep metadata
                        entry.content = None;

                        // Reset access counter
                        entry.access_count_since_load = 0;
                    }
                }

                // Update memory usage
                if total_freed > 0 {
                    if let Ok(mut usage) = self.memory_usage.write() {
                        *usage = usage.saturating_sub(total_freed);
                    }

                    debug!(
                        "Freed {} bytes by unloading content from {} entries",
                        total_freed,
                        unload_candidates.len()
                    );
                }
            }
        }
    }

    /// Update memory usage tracking
    fn update_memory_usage(&self, delta: i64) {
        if let Ok(mut usage) = self.memory_usage.write() {
            if delta > 0 {
                *usage += delta as u64;
            } else {
                *usage = usage.saturating_sub((-delta) as u64);
            }
        }
    }

    /// Update LRU order when a file is accessed
    fn update_lru_order(&self, path: &Path) {
        if let Ok(mut access_order) = self.access_order.write() {
            // Remove the path if it already exists
            access_order.retain(|p| p != path);

            // Add to front (most recently used)
            access_order.push_front(path.to_path_buf());

            // Keep size bounded
            if access_order.len() > MAX_CACHE_ENTRIES * 2 {
                access_order.truncate(MAX_CACHE_ENTRIES * 2);
            }
        }
    }

    /// Get metadata for a file, checking the cache first
    #[allow(dead_code)]
    pub fn get_metadata(&self, path: &Path) -> Result<fs::Metadata> {
        // Check if we have a cached entry
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            // Check if the file has been modified
            if let Ok(metadata) = fs::metadata(path)
                && metadata.modified()? == entry.last_modified
            {
                return Ok(metadata);
            }
        }

        // Not in cache or modified, get fresh metadata
        fs::metadata(path).with_context(|| format!("Failed to get metadata for {}", path.display()))
    }

    /// Calculate the SHA-256 hash of a file's contents
    #[allow(dead_code)]
    pub fn calculate_hash(&self, path: &Path) -> Result<String> {
        // Check if we have a cached entry with content
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            // Check if the file has been modified
            if let Ok(metadata) = fs::metadata(path)
                && metadata.modified()? == entry.last_modified
            {
                return Ok(entry.hash.clone());
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
        // Check for memory pressure periodically
        let under_pressure = self.check_memory_pressure();

        // Use a two-phase approach to reduce lock contention:
        // 1. Try to get content from cache with read lock
        // 2. If cache miss or file modified, acquire write lock to update cache

        // Phase 1: Try to get from cache with read lock
        let cached_result: Result<Option<Vec<u8>>> = {
            // Scope for read lock
            match self.entries.read() {
                Ok(entries) => {
                    if let Some(entry) = entries.get(path) {
                        // Check if the file has been modified
                        if let Ok(metadata) = fs::metadata(path)
                            && let Ok(last_mod) = metadata.modified()
                            && last_mod == entry.last_modified
                        {
                            // Return cached content if available
                            if let Some(content) = &entry.content {
                                trace!("Returning cached content for {}", path.display());
                                // Update access time and LRU order in a background thread
                                let cache_ref = self.clone();
                                let path_copy = path.to_path_buf();
                                std::thread::spawn(move || {
                                    // Update LRU order first (read lock)
                                    cache_ref.update_lru_order(&path_copy);

                                    // Then update entry (write lock)
                                    if let Ok(mut entries) = cache_ref.entries.write()
                                        && let Some(entry) = entries.get_mut(&path_copy)
                                    {
                                        entry.touch();
                                    }
                                });

                                return Ok(content.clone());
                            } else if !under_pressure {
                                // Content not cached but entry exists - try loading
                                debug!(
                                    "Content not cached for {}, loading from disk",
                                    path.display()
                                );
                                // Fall through to load from disk but use existing entry
                            }
                        }
                    }
                    Ok(None)
                }
                Err(e) => Err(anyhow::anyhow!("Failed to acquire read lock: {}", e)),
            }
        };

        // Handle successful cache hit
        if let Ok(Some(content)) = cached_result {
            return Ok(content);
        }

        // Handle lock errors
        if let Err(e) = cached_result {
            debug!("Error reading from cache, continuing to file: {}", e);
        }

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
                ));
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

        // Check if we should try to cache this file based on size and memory pressure
        let should_cache_content = metadata.len() <= MAX_CACHED_FILE_SIZE
            && (!under_pressure || metadata.len() <= MAX_CACHED_FILE_SIZE / 5);

        // Update the cache with optimized approach
        if let Err(e) =
            self.update_cache_with_content(path, &content, &metadata, should_cache_content)
        {
            debug!("Cache update failed but will still return content: {}", e);
        }

        Ok(content)
    }

    /// Update cache with optimized memory management
    fn update_cache_with_content(
        &self,
        path: &Path,
        content: &[u8],
        metadata: &fs::Metadata,
        cache_content: bool,
    ) -> Result<()> {
        // Calculate hash
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(content);
            format!("{:x}", hasher.finalize())
        };

        // Count lines
        let total_lines = count_lines(content);

        // Track memory delta for updating usage counter
        let mut memory_delta: i64 = 0;

        // Attempt to update the cache with timeout to prevent long waits
        let update_result = tokio::task::block_in_place(|| {
            // Use a short timeout since this is a frequent operation
            let timeout = Duration::from_millis(200);
            let start = Instant::now();

            while start.elapsed() < timeout {
                match self.entries.try_write() {
                    Ok(mut entries) => {
                        // Check if we're updating an existing entry
                        let mut existing_memory: u64 = 0;

                        let (file_content, loading_strategy) = if cache_content {
                            // Include content in cache if requested
                            let strategy = if metadata.len() > MAX_CACHED_FILE_SIZE / 2 {
                                // For medium-sized files, use segmented caching
                                ContentLoadingStrategy::SegmentedCache
                            } else {
                                // For small files, always cache
                                ContentLoadingStrategy::AlwaysCache
                            };
                            (Some(content.to_vec()), strategy)
                        } else {
                            // For large files, don't cache content
                            let strategy = if metadata.len() > MAX_CACHED_FILE_SIZE * 10 {
                                ContentLoadingStrategy::MetadataOnly
                            } else {
                                ContentLoadingStrategy::OnDemand
                            };
                            (None, strategy)
                        };

                        if let Some(entry) = entries.get(path) {
                            // Calculate existing memory usage for delta tracking
                            if let Some(old_content) = &entry.content {
                                existing_memory = old_content.len() as u64;
                            }
                        }

                        // Calculate new memory usage
                        let new_memory = if let Some(new_content) = &file_content {
                            new_content.len() as u64
                        } else {
                            0
                        };

                        // Calculate memory delta (negative if we're freeing memory)
                        memory_delta = new_memory as i64 - existing_memory as i64;

                        // Create or update the cache entry
                        entries.insert(
                            path.to_path_buf(),
                            FileCacheEntry {
                                path: path.to_path_buf(),
                                hash,
                                size: metadata.len(),
                                last_modified: metadata
                                    .modified()
                                    .unwrap_or_else(|_| SystemTime::now()),
                                last_accessed: SystemTime::now(),
                                content: file_content,
                                loading_strategy,
                                last_content_access: Instant::now(),
                                access_count_since_load: 1,
                                is_pinned: self.is_file_pinned(path).await,
                                fully_read: false,
                                read_ranges: Vec::new(),
                                total_lines,
                                stats: FileStats::new(),
                            },
                        );

                        // Update LRU tracking independently of content
                        self.update_lru_order(path);

                        // Ensure cache doesn't grow too large (entry count)
                        if entries.len() > MAX_CACHE_ENTRIES {
                            self.evict_oldest_entries(&mut entries);
                        }

                        return Ok(());
                    }
                    Err(_) => {
                        // Couldn't get lock, sleep briefly and retry
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }

            // Timed out waiting for lock
            Err(anyhow::anyhow!(
                "Timed out waiting for write lock during cache update"
            ))
        });

        // Update memory usage counter
        if memory_delta != 0 {
            self.update_memory_usage(memory_delta);
        }

        // Log errors but don't fail the operation
        if let Err(e) = update_result {
            debug!("Failed to update cache entry for {}: {}", path.display(), e);
        }

        Ok(())
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

    /// Evict entries from the cache to maintain size limits using LRU strategy
    fn evict_oldest_entries(&self, entries: &mut HashMap<PathBuf, FileCacheEntry>) {
        // Memory tracking for freed memory
        let mut memory_freed: u64 = 0;
        let mut removed_paths = Vec::new();

        // Get access order to identify least recently used entries
        let mut lru_paths = Vec::new();

        if let Ok(access_order) = self.access_order.read() {
            // Get paths from back (least recently used) to front
            for path in access_order.iter().rev() {
                lru_paths.push(path.clone());
            }
        }

        // If we have access order data, use it for eviction
        if !lru_paths.is_empty() {
            // Calculate target entries (keep at most MAX_CACHE_ENTRIES)
            let target_count = MAX_CACHE_ENTRIES;
            let excess_count = entries.len().saturating_sub(target_count);

            if excess_count > 0 {
                debug!("Evicting {} entries to maintain size limit", excess_count);

                // Track which entries were removed
                let mut removed_count = 0;

                // Start from least recently used
                for path in lru_paths {
                    // Don't exceed target removal count
                    if removed_count >= excess_count {
                        break;
                    }

                    // Skip pinned files
                    if self.is_file_pinned(&path) {
                        continue;
                    }

                    // Get entry and decide whether to remove or unload content
                    if let Some(entry) = entries.get(&path) {
                        // For high importance files, just unload content
                        if entry.stats.importance_score > 10.0 && entry.content.is_some() {
                            // If this entry has high importance, just unload the content
                            if let Some(content) = &entry.content {
                                memory_freed += content.len() as u64;
                            }

                            if let Some(entry) = entries.get_mut(&path) {
                                entry.content = None;
                                entry.access_count_since_load = 0;
                            }
                        } else {
                            // Remove entry completely
                            if let Some(content) = &entry.content {
                                memory_freed += content.len() as u64;
                            }

                            entries.remove(&path);
                            removed_paths.push(path);
                            removed_count += 1;
                        }
                    }
                }
            }
        } else {
            // Fallback if we don't have access order data: use traditional last_accessed time
            let mut entry_data: Vec<_> = entries
                .iter()
                .map(|(path, entry)| {
                    // Create a tuple with (path, is_pinned, importance, last_accessed)
                    (
                        path.clone(),
                        entry.is_pinned,
                        entry.stats.importance_score,
                        entry.last_accessed,
                        entry.content.as_ref().map(|c| c.len() as u64).unwrap_or(0),
                    )
                })
                .collect();

            // Sort: unpinned first, then by importance (low to high), then by last_accessed (oldest first)
            entry_data.sort_by(|a, b| {
                // First criteria: pinned status (unpinned first)
                match (a.1, b.1) {
                    (true, false) => return std::cmp::Ordering::Greater,
                    (false, true) => return std::cmp::Ordering::Less,
                    _ => {}
                }

                // Second criteria: importance score (low to high)
                match a.2.partial_cmp(&b.2) {
                    Some(ord) if ord != std::cmp::Ordering::Equal => return ord,
                    _ => {}
                }

                // Third criteria: access time (oldest first)
                a.3.cmp(&b.3)
            });

            // Calculate how many entries to remove
            let target_count = MAX_CACHE_ENTRIES;
            let excess_count = entries.len().saturating_sub(target_count);

            // Remove oldest/least important entries
            for (path, is_pinned, _, _, content_size) in entry_data.iter().take(excess_count) {
                // Skip pinned files
                if *is_pinned {
                    continue;
                }

                memory_freed += *content_size;
                entries.remove(path);
                removed_paths.push(path.clone());
            }
        }

        // Update memory usage counter asynchronously if memory was freed
        if memory_freed > 0 {
            let cache_ref = self.clone();
            std::thread::spawn(move || {
                cache_ref.update_memory_usage(-(memory_freed as i64));
            });
        }

        // Update access order list to remove evicted entries
        if !removed_paths.is_empty()
            && let Ok(mut access_order) = self.access_order.write()
        {
            access_order.retain(|path| !removed_paths.contains(path));
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
        if let Ok(mut entries) = self.entries.write()
            && let Some(entry) = entries.get_mut(path)
        {
            entry.record_edit();
        }

        // Update workspace stats
        self.update_workspace_stats_file_edit(path);

        Ok(())
    }

    /// Record a file write operation
    pub fn record_file_write(&self, path: &Path) -> Result<()> {
        // Update the cache entry
        if let Ok(mut entries) = self.entries.write()
            && let Some(entry) = entries.get_mut(path)
        {
            entry.record_write();
        }

        // Update workspace stats
        self.update_workspace_stats_file_edit(path);

        Ok(())
    }

    /// Update workspace stats with file access
    fn update_workspace_stats_file_access(&self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();

        if let Ok(mut ws_guard) = self.workspace_stats.write()
            && let Some(stats) = ws_guard.as_mut()
        {
            stats.record_file_access(&path_str);
        }
    }

    /// Update workspace stats with file edit
    fn update_workspace_stats_file_edit(&self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();

        if let Ok(mut ws_guard) = self.workspace_stats.write()
            && let Some(stats) = ws_guard.as_mut()
        {
            stats.record_file_edit(&path_str);
        }
    }

    /// Get file statistics for a particular file
    pub fn get_file_stats(&self, path: &Path) -> Option<FileStats> {
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            return Some(entry.stats.clone());
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
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            return entry.fully_read;
        }

        false
    }

    /// Check if a file has been read enough (>=99%)
    #[allow(dead_code)]
    pub fn is_file_read_enough(&self, path: &Path) -> bool {
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            return entry.is_read_enough();
        }

        false
    }

    /// Get the unread ranges for a file
    pub fn get_unread_ranges(&self, path: &Path) -> Vec<(usize, usize)> {
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            return entry.get_unread_ranges();
        }

        if let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
        {
            // File exists but not in cache, consider the whole file unread
            if let Ok(content) = fs::read(path) {
                let total_lines = count_lines(&content);
                if total_lines > 0 {
                    return vec![(1, total_lines)];
                }
            }
        }

        Vec::new()
    }

    /// Check if a file has changed since it was last cached
    #[allow(dead_code)]
    pub fn has_file_changed(&self, path: &Path) -> Result<bool> {
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
            && let Ok(metadata) = fs::metadata(path)
        {
            let current_modified = metadata.modified()?;
            return Ok(current_modified != entry.last_modified);
        }

        // Not in cache or can't get metadata, consider changed
        Ok(true)
    }

    /// Get the hash for a file from the cache
    pub fn get_cached_hash(&self, path: &Path) -> Option<String> {
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(path)
        {
            return Some(entry.hash.clone());
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
        let entries = cache.entries.read().await;
        assert!(entries.contains_key(path));
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

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
        assert_eq!(entry.read_ranges, vec![(1, 2)]);
        assert!(entry.is_line_read(1));
        assert!(entry.is_line_read(2));
        assert!(!entry.is_line_read(3));

        // Read more lines
        cache.record_read_range(path, 4, 5).unwrap(); // Read lines 4-5

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
        assert_eq!(entry.read_ranges, vec![(1, 2), (4, 5)]);

        // Get unread ranges
        let unread = cache.get_unread_ranges(path);
        assert_eq!(unread, vec![(3, 3)]);

        // Read the rest
        cache.record_read_range(path, 3, 3).unwrap();

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
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

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
        assert_eq!(entry.read_percentage(), 40.0); // 2/5 = 40%

        // Read more lines
        cache.record_read_range(path, 4, 5).unwrap(); // Read lines 4-5

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
        assert_eq!(entry.read_percentage(), 80.0); // 4/5 = 80%

        // Read everything
        cache.record_read_range(path, 1, 5).unwrap();

        let entries = cache.entries.read().await;
        let entry = entries.get(path).unwrap().clone();
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
