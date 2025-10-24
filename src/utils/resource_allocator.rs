//! Smart resource allocation for file reading operations
//!
//! This module provides intelligent resource management for reading files,
//! including memory allocation, concurrent operation limits, and prioritization
//! based on file characteristics.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info}; // Replace std::sync::Mutex

use crate::errors::{Result, WinxError};

/// Maximum total memory allocation for file reading operations (100MB)
const MAX_TOTAL_MEMORY: usize = 100 * 1024 * 1024;

/// Maximum concurrent file reading operations
const MAX_CONCURRENT_READS: usize = 5;

/// Memory allocation per file size category
const SMALL_FILE_MEMORY: usize = 1024 * 1024; // 1MB
const MEDIUM_FILE_MEMORY: usize = 5 * 1024 * 1024; // 5MB
const LARGE_FILE_MEMORY: usize = 20 * 1024 * 1024; // 20MB

/// File size thresholds for categorization
const SMALL_FILE_THRESHOLD: u64 = 100_000; // 100KB
const MEDIUM_FILE_THRESHOLD: u64 = 5_000_000; // 5MB

/// Cache entry TTL
const CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// File reading priority levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

/// File size category for resource allocation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileCategory {
    Small,
    Medium,
    Large,
}

impl FileCategory {
    /// Determine category based on file size
    pub fn from_size(size: u64) -> Self {
        if size <= SMALL_FILE_THRESHOLD {
            Self::Small
        } else if size <= MEDIUM_FILE_THRESHOLD {
            Self::Medium
        } else {
            Self::Large
        }
    }

    /// Get memory allocation for this category
    pub fn memory_allocation(&self) -> usize {
        match self {
            Self::Small => SMALL_FILE_MEMORY,
            Self::Medium => MEDIUM_FILE_MEMORY,
            Self::Large => LARGE_FILE_MEMORY,
        }
    }

    /// Get timeout for this category
    pub fn timeout(&self) -> Duration {
        match self {
            Self::Small => Duration::from_secs(10),
            Self::Medium => Duration::from_secs(30),
            Self::Large => Duration::from_secs(60),
        }
    }
}

/// Resource allocation request
#[derive(Debug)]
pub struct AllocationRequest {
    pub file_path: PathBuf,
    pub file_size: u64,
    pub priority: ReadPriority,
    pub max_memory: Option<usize>,
    pub timeout: Option<Duration>,
}

/// Resource allocation result
#[derive(Debug, Clone)]
pub struct Allocation {
    pub allocated_memory: usize,
    pub category: FileCategory,
    pub timeout: Duration,
    pub should_use_streaming: bool,
    pub chunk_size: Option<usize>,
}

/// Resource usage statistics
#[derive(Debug)]
pub struct ResourceStats {
    pub total_allocated: AtomicUsize,
    pub active_reads: AtomicUsize,
    pub pending_requests: AtomicUsize,
    pub successful_reads: AtomicU64,
    pub failed_reads: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
}

/// Cache entry for allocation decisions
#[derive(Debug)]
struct CacheEntry {
    allocation: Allocation,
    created_at: Instant,
    access_count: AtomicU32,
}

/// Smart resource allocator for file reading operations
#[derive(Debug)]
pub struct ResourceAllocator {
    /// Semaphore for limiting concurrent operations
    read_semaphore: Arc<Semaphore>,
    /// Current memory allocations by file path
    allocations: Arc<RwLock<HashMap<PathBuf, usize>>>,
    /// Allocation cache for repeated requests
    cache: Arc<RwLock<HashMap<PathBuf, CacheEntry>>>,
    /// Priority queue for pending requests
    pending_queue: Arc<Mutex<VecDeque<AllocationRequest>>>,
    /// Resource usage statistics
    stats: Arc<ResourceStats>,
    /// Configuration
    max_total_memory: usize,
    max_concurrent_reads: usize,
}

impl Default for ResourceAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceAllocator {
    /// Create a new resource allocator
    pub fn new() -> Self {
        Self {
            read_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_READS)),
            allocations: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(HashMap::new())),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            stats: Arc::new(ResourceStats {
                total_allocated: AtomicUsize::new(0),
                active_reads: AtomicUsize::new(0),
                pending_requests: AtomicUsize::new(0),
                successful_reads: AtomicU64::new(0),
                failed_reads: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                cache_misses: AtomicU64::new(0),
            }),
            max_total_memory: MAX_TOTAL_MEMORY,
            max_concurrent_reads: MAX_CONCURRENT_READS,
        }
    }

    /// Request resource allocation for file reading
    pub async fn request_allocation(&self, request: AllocationRequest) -> Result<Allocation> {
        debug!("Requesting allocation for: {:?}", request.file_path);

        // Check cache first
        if let Some(cached) = self.check_cache(&request.file_path).await {
            self.update_cache_hit().await;
            debug!("Cache hit for: {:?}", request.file_path);
            return Ok(cached);
        }

        self.update_cache_miss().await;

        // Calculate allocation based on file characteristics
        let category = FileCategory::from_size(request.file_size);
        let mut allocation = self.calculate_allocation(&request, category).await?;

        // Check if we need to use streaming for large files
        if request.file_size > LARGE_FILE_MEMORY as u64 {
            allocation.should_use_streaming = true;
            allocation.chunk_size = Some(1024 * 1024); // 1MB chunks
            // Safely use the chunk size if present; otherwise fall back to doubling current allocation.
            allocation.allocated_memory = if let Some(chunk) = allocation.chunk_size {
                chunk.saturating_mul(2) // Double buffer, using saturating to avoid overflow
            } else {
                allocation.allocated_memory.saturating_mul(2)
            };
        }

        // Try to acquire resources
        if self.can_allocate(allocation.allocated_memory).await {
            self.allocate_resources(&request.file_path, allocation.allocated_memory)
                .await;
            self.cache_allocation(&request.file_path, allocation.clone())
                .await;
            Ok(allocation)
        } else {
            // Add to pending queue if resources not available
            self.queue_request(request).await;
            Err(WinxError::ResourceAllocationError {
                message: Arc::new("Insufficient resources available, request queued".to_string()),
            })
        }
    }

    /// Calculate optimal allocation for the request
    async fn calculate_allocation(
        &self,
        request: &AllocationRequest,
        category: FileCategory,
    ) -> Result<Allocation> {
        let base_memory = category.memory_allocation();
        let timeout = request.timeout.unwrap_or_else(|| category.timeout());

        // Adjust based on priority
        let memory_multiplier = match request.priority {
            ReadPriority::Critical => 1.5,
            ReadPriority::High => 1.2,
            ReadPriority::Normal => 1.0,
            ReadPriority::Low => 0.8,
        };

        let allocated_memory = if let Some(max_mem) = request.max_memory {
            max_mem.min((base_memory as f64 * memory_multiplier) as usize)
        } else {
            (base_memory as f64 * memory_multiplier) as usize
        };

        // Determine if streaming is beneficial
        let should_use_streaming = request.file_size > allocated_memory as u64 * 2;

        Ok(Allocation {
            allocated_memory,
            category,
            timeout,
            should_use_streaming,
            chunk_size: if should_use_streaming {
                Some(allocated_memory / 4) // Use quarter of allocation as chunk size
            } else {
                None
            },
        })
    }

    /// Check if we can allocate the requested memory
    async fn can_allocate(&self, requested_memory: usize) -> bool {
        if let Ok(allocations_guard) = self.allocations.read().await {
            let current_total: usize = allocations_guard.values().sum();
            current_total + requested_memory <= self.max_total_memory
        } else {
            false
        }
    }

    /// Allocate resources for a file read
    async fn allocate_resources(&self, file_path: &Path, memory: usize) {
        let mut allocations = self.allocations.write().await;
        allocations.insert(file_path.to_path_buf(), memory);

        self.stats
            .total_allocated
            .fetch_add(memory, Ordering::Relaxed);
        self.stats.active_reads.fetch_add(1, Ordering::Relaxed);

        tracing::debug!("Allocated {} bytes for: {:?}", memory, file_path);
    }

    /// Release resources after file read completion
    pub async fn release_allocation(&self, file_path: &Path) -> Result<()> {
        let mut allocations = self.allocations.write().await;
        let memory_released = allocations.remove(file_path);

        if let Some(memory) = memory_released {
            self.stats
                .total_allocated
                .fetch_sub(memory, Ordering::Relaxed);
            self.stats.active_reads.fetch_sub(1, Ordering::Relaxed);

            tracing::debug!("Released {} bytes for: {:?}", memory, file_path);
        }

        self.process_pending_queue().await;

        Ok(())
    }

    /// Check cache for allocation
    async fn check_cache(&self, file_path: &Path) -> Option<Allocation> {
        let cache = self.cache.read().await;
        if let Some(entry) = cache.get(file_path) {
            if entry.created_at.elapsed() < CACHE_TTL {
                entry.access_count.fetch_add(1, Ordering::Relaxed);
                return Some(entry.allocation.clone());
            }
        }
        None
    }

    /// Cache an allocation decision
    async fn cache_allocation(&self, file_path: &Path, allocation: Allocation) {
        let mut cache = self.cache.write().await;
        cache.insert(
            file_path.to_path_buf(),
            CacheEntry {
                allocation,
                created_at: Instant::now(),
                access_count: AtomicU32::new(1),
            },
        );

        // Cleanup old entries if cache is getting large
        if cache.len() > 1000 {
            self.cleanup_cache(&mut cache);
        }
    }

    /// Clean up old cache entries
    fn cleanup_cache(&self, cache: &mut HashMap<PathBuf, CacheEntry>) {
        let now = Instant::now();
        cache.retain(|_, entry| now.duration_since(entry.created_at) < CACHE_TTL);
    }

    /// Add request to pending queue
    async fn queue_request(&self, request: AllocationRequest) {
        let mut queue = self.pending_queue.lock().await;
        let insert_pos = queue
            .iter()
            .position(|req| req.priority < request.priority)
            .unwrap_or(queue.len());

        queue.insert(insert_pos, request);

        self.stats.pending_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Process pending allocation requests
    async fn process_pending_queue(&self) {
        let mut processed = Vec::new();

        loop {
            let mut queue = self.pending_queue.lock().await;
            let request = queue.pop_front();

            if let Some(request) = request {
                let category = FileCategory::from_size(request.file_size);
                if let Ok(allocation) = self.calculate_allocation(&request, category).await {
                    if self.can_allocate(allocation.allocated_memory).await {
                        self.allocate_resources(&request.file_path, allocation.allocated_memory)
                            .await;
                        self.cache_allocation(&request.file_path, allocation).await;
                        processed.push(request.file_path);
                    } else {
                        queue.push_front(request);
                        break;
                    }
                }
            } else {
                break; // No more requests
            }
        }

        if !processed.is_empty() {
            tracing::info!("Processed {} pending allocation requests", processed.len());
        }
    }

    /// Acquire read permit (for concurrent operation limiting)
    pub async fn acquire_read_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>> {
        self.read_semaphore
            .acquire()
            .await
            .map_err(|e| WinxError::ResourceAllocationError {
                message: Arc::new(format!("Failed to acquire read permit: {}", e)),
            })
    }

    /// Mark read operation as successful
    pub async fn mark_read_success(&self) {
        self.stats.successful_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark read operation as failed
    pub async fn mark_read_failure(&self) {
        self.stats.failed_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Update cache hit statistics
    async fn update_cache_hit(&self) {
        self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Update cache miss statistics
    async fn update_cache_miss(&self) {
        self.stats.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current resource usage statistics
    pub async fn get_stats(&self) -> ResourceStats {
        ResourceStats {
            total_allocated: AtomicUsize::new(self.stats.total_allocated.load(Ordering::Relaxed)),
            active_reads: AtomicUsize::new(self.stats.active_reads.load(Ordering::Relaxed)),
            pending_requests: AtomicUsize::new(self.stats.pending_requests.load(Ordering::Relaxed)),
            successful_reads: AtomicU64::new(self.stats.successful_reads.load(Ordering::Relaxed)),
            failed_reads: AtomicU64::new(self.stats.failed_reads.load(Ordering::Relaxed)),
            cache_hits: AtomicU64::new(self.stats.cache_hits.load(Ordering::Relaxed)),
            cache_misses: AtomicU64::new(self.stats.cache_misses.load(Ordering::Relaxed)),
        }
    }

    /// Get memory usage percentage
    pub async fn get_memory_usage_percent(&self) -> f64 {
        let allocations = self.allocations.read().await;
        let current_total: usize = allocations.values().sum();
        (current_total as f64 / self.max_total_memory as f64) * 100.0
    }

    /// Check if system is under memory pressure
    pub async fn is_under_memory_pressure(&self) -> bool {
        self.get_memory_usage_percent().await > 80.0
    }

    /// Force cleanup of unused allocations
    pub async fn cleanup_unused_allocations(&self) {
        if let Ok(mut cache) = self.cache.write() {
            self.cleanup_cache(&mut cache);

            // Also cleanup any stale allocations
            if let Ok(mut allocations) = self.allocations.write() {
                allocations.retain(|path, _| cache.contains_key(path));
            }

            info!("Performed cleanup of unused allocations");
        }
    }

    /// Cleanup all allocations (for testing and debugging)
    async fn cleanup_allocations(&self) {
        let mut allocations = self.allocations.write().await;
        allocations.clear();

        let mut cache = self.cache.write().await;
        self.cleanup_cache(&mut cache);
    }
}

/// Global resource allocator instance
static GLOBAL_ALLOCATOR: std::sync::OnceLock<ResourceAllocator> = std::sync::OnceLock::new();

/// Get the global resource allocator instance
pub fn get_global_allocator() -> &'static ResourceAllocator {
    GLOBAL_ALLOCATOR.get_or_init(ResourceAllocator::new)
}

/// Convenience function to request allocation with default priority
pub async fn request_file_allocation(file_path: &Path, file_size: u64) -> Result<Allocation> {
    let allocator = get_global_allocator();
    allocator
        .request_allocation(AllocationRequest {
            file_path: file_path.to_path_buf(),
            file_size,
            priority: ReadPriority::Normal,
            max_memory: None,
            timeout: None,
        })
        .await
}

/// Convenience function to release allocation
pub async fn release_file_allocation(file_path: &Path) -> Result<()> {
    let allocator = get_global_allocator();
    allocator.release_allocation(file_path).await
}

/// Smart resource allocation guard that automatically releases resources
pub struct AllocationGuard<'a> {
    file_path: &'a Path,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

impl<'a> AllocationGuard<'a> {
    /// Create a new allocation guard
    pub async fn new(file_path: &'a Path, _allocation: Allocation) -> Result<Self> {
        let allocator = get_global_allocator();
        let permit = allocator.acquire_read_permit().await?;
        Ok(Self {
            file_path,
            _permit: permit,
        })
    }
}

impl<'a> Drop for AllocationGuard<'a> {
    fn drop(&mut self) {
        let file_path = self.file_path.to_path_buf(); // Clone the file path to ensure ownership
        tokio::spawn(async move {
            let _ = release_file_allocation(&file_path).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_file_category_classification() {
        assert_eq!(FileCategory::from_size(50_000), FileCategory::Small);
        assert_eq!(FileCategory::from_size(1_000_000), FileCategory::Medium);
        assert_eq!(FileCategory::from_size(10_000_000), FileCategory::Large);
    }

    #[tokio::test]
    async fn test_resource_allocation() {
        let allocator = ResourceAllocator::new();

        let request = AllocationRequest {
            file_path: PathBuf::from("/test/file.txt"),
            file_size: 1_000_000,
            priority: ReadPriority::Normal,
            max_memory: None,
            timeout: None,
        };

        let allocation = allocator.request_allocation(request).await.unwrap();
        assert_eq!(allocation.category as u8, FileCategory::Medium as u8);
        assert!(!allocation.should_use_streaming);
    }

    #[tokio::test]
    async fn test_streaming_for_large_files() {
        let allocator = ResourceAllocator::new();

        let request = AllocationRequest {
            file_path: PathBuf::from("/test/large_file.txt"),
            file_size: 50_000_000, // 50MB
            priority: ReadPriority::Normal,
            max_memory: None,
            timeout: None,
        };

        let allocation = allocator.request_allocation(request).await.unwrap();
        assert!(allocation.should_use_streaming);
        assert!(allocation.chunk_size.is_some());
    }

    #[tokio::test]
    async fn test_memory_pressure_detection() {
        let allocator = ResourceAllocator::new();

        // Initially should not be under pressure
        assert!(!allocator.is_under_memory_pressure().await);

        // Simulate high memory usage by checking percentage
        let usage = allocator.get_memory_usage_percent().await;
        assert!(usage < 80.0); // Should be low initially
    }
}
