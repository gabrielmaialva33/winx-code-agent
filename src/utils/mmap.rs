use memmap2::{Mmap, MmapOptions};
use rayon::prelude::*;
use std::cmp::min;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

use crate::errors::{Result, WinxError};

/// Maximum file size for direct reading (10MB)
pub const DIRECT_READ_THRESHOLD: u64 = 10_000_000;

/// Maximum file size for single memory mapping (1GB)
pub const MAX_MMAP_SIZE: u64 = 1_000_000_000;

/// Maximum file size for segmented memory mapping (4GB)
pub const MAX_SEGMENTED_MMAP_SIZE: u64 = 4_000_000_000;

/// Segment size for large file memory mapping (256MB)
pub const SEGMENT_SIZE: u64 = 256_000_000;

/// Read file contents optimally based on file size
///
/// This function chooses the optimal reading strategy based on file size:
/// - Small files: Direct read with standard File I/O
/// - Medium files: Memory-mapped reading for performance
/// - Large files: Segmented memory-mapped reading
/// - Extreme files: Windowed access with streaming
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `max_file_size` - Maximum allowed file size
///
/// # Returns
///
/// A vector containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds the size limit
pub fn read_file_optimized(path: &Path, max_file_size: u64) -> Result<Vec<u8>> {
    // Get file metadata
    let file = File::open(path).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Error opening file: {e}"),
    })?;

    let metadata = file.metadata().map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to get file metadata: {e}"),
    })?;

    // Check file size
    let file_size = metadata.len();
    if file_size > max_file_size {
        return Err(WinxError::FileTooLarge {
            path: path.to_path_buf(),
            size: file_size,
            max_size: max_file_size,
        });
    }

    // Choose reading strategy based on file size
    if file_size < DIRECT_READ_THRESHOLD {
        debug!("Using direct read for file: {}", path.display());
        read_direct(&file, file_size, path)
    } else if file_size < MAX_MMAP_SIZE {
        debug!("Using memory-mapped read for file: {}", path.display());
        read_mmap(&file, path)
    } else if file_size < MAX_SEGMENTED_MMAP_SIZE {
        debug!("Using segmented memory-mapped read for file: {}", path.display());
        read_segmented_mmap(&file, file_size, path)
    } else {
        debug!("Using streaming read for extremely large file: {}", path.display());
        read_streaming(&file, file_size, path)
    }
}

/// Read file contents directly using standard I/O
///
/// This is efficient for small files.
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `file_size` - Size of the file
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be read
fn read_direct(file: &File, file_size: u64, path: &Path) -> Result<Vec<u8>> {
    // For very small files (< 1MB), use an optimized approach
    if file_size < 1_000_000 {
        // Pre-allocate an exact-sized buffer
        let mut buffer = Vec::with_capacity(file_size as usize);

        // Create a mutable file handle and seek to the beginning
        let mut file_handle = file.try_clone().map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error cloning file handle: {e}"),
        })?;

        file_handle.seek(SeekFrom::Start(0)).map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error seeking to start of file: {e}"),
        })?;

        // Use a BufReader with an appropriate buffer size (4K-64K)
        let mut reader = BufReader::with_capacity(min(file_size as usize, 64 * 1024), file_handle);

        // Read directly to the end
        reader.read_to_end(&mut buffer).map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error reading file: {e}"),
        })?;

        return Ok(buffer);
    }

    // For larger files, use a chunked reading approach with progress tracking
    let mut buffer = Vec::with_capacity(file_size as usize);

    // Create a mutable file handle and seek to the beginning
    let mut file_handle = file.try_clone().map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Error cloning file handle: {e}"),
    })?;

    file_handle.seek(SeekFrom::Start(0)).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Error seeking to start of file: {e}"),
    })?;

    let mut reader = BufReader::with_capacity(262_144, file_handle); // 256KB buffer

    const CHUNK_SIZE: usize = 1_048_576; // 1MB chunks
    let mut chunk = vec![0; CHUNK_SIZE];
    let mut bytes_read = 0;

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                bytes_read += n as u64;

                // Log progress for larger files
                if file_size > 5_000_000 && bytes_read % 5_000_000 < CHUNK_SIZE as u64 {
                    trace!(
                        "Read progress for {}: {}MB/{}MB ({}%)",
                        path.display(),
                        bytes_read / 1_000_000,
                        file_size / 1_000_000,
                        bytes_read * 100 / file_size
                    );
                }
            }
            Err(e) => {
                return Err(WinxError::FileAccessError {
                    path: path.to_path_buf(),
                    message: format!("Error reading file chunk: {e}"),
                });
            }
        }
    }

    Ok(buffer)
}

/// Read file contents using memory mapping
///
/// This is efficient for larger files as it avoids loading the entire
/// file into memory at once.
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be mapped
fn read_mmap(file: &File, path: &Path) -> Result<Vec<u8>> {
    // Check for empty file to avoid mmap error
    if file.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Ok(Vec::new());
    }

    // SAFETY: Memory mapping a file is inherently unsafe because:
    // - The file could be modified by another process during access
    // - The file could be truncated, causing access to invalid memory
    // We mitigate these risks by:
    // - Using the mapped data immediately and converting to Vec<u8>
    // - Not holding the mmap across async boundaries
    // - File size was verified before this call
    let mmap = unsafe { MmapOptions::new().map(file) }.map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to memory-map file: {e}"),
    })?;

    // Use Rayon for parallel processing if the file is large enough
    if mmap.len() > 10_000_000 {
        // 10MB threshold for parallel processing
        debug!("Using parallel processing for large mmap file: {}", path.display());

        // Process in parallel chunks
        const CHUNK_SIZE: usize = 1_048_576; // 1MB chunks
        let chunk_count = mmap.len().div_ceil(CHUNK_SIZE);
        let mut result = vec![0; mmap.len()];

        // Process in parallel with Rayon - use collect for parallel map
        let chunks: Vec<_> = (0..chunk_count)
            .into_par_iter()
            .map(|i| {
                let start = i * CHUNK_SIZE;
                let end = min((i + 1) * CHUNK_SIZE, mmap.len());

                if start < mmap.len() {
                    // Extract chunk from mmap
                    let src = &mmap[start..end];
                    (start, end, src.to_vec())
                } else {
                    (start, start, Vec::new())
                }
            })
            .collect();

        // Now apply all chunks to the result sequentially
        for (start, end, chunk) in chunks {
            if start < end {
                result[start..end].copy_from_slice(&chunk);
            }
        }

        Ok(result)
    } else {
        // For smaller files, just copy the entire map to a Vec
        Ok(mmap.to_vec())
    }
}

/// Read large file with segmented memory mapping
///
/// This function reads a large file using multiple memory mapped segments,
/// which allows handling files larger than the maximum mapping size.
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `file_size` - Size of the file
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be read or mapped
fn read_segmented_mmap(_file: &File, file_size: u64, path: &Path) -> Result<Vec<u8>> {
    // Calculate number of segments needed
    let segment_count = file_size.div_ceil(SEGMENT_SIZE);
    debug!(
        "Reading file {} in {} segments of {}MB each",
        path.display(),
        segment_count,
        SEGMENT_SIZE / 1_000_000
    );

    // Pre-allocate result vector
    let mut result = Vec::with_capacity(file_size as usize);

    // Process each segment
    for i in 0..segment_count {
        let segment_start = i * SEGMENT_SIZE;
        let segment_size = min(SEGMENT_SIZE, file_size - segment_start);

        info!(
            "Processing segment {}/{} of file {} ({:.1}%)",
            i + 1,
            segment_count,
            path.display(),
            (segment_start as f64 / file_size as f64) * 100.0
        );

        // Open a new file handle for each segment to avoid position conflicts
        let segment_file = File::open(path).map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error opening file for segment {i}: {e}"),
        })?;

        // Seek to segment start
        let mut segment_file = segment_file;
        segment_file.seek(SeekFrom::Start(segment_start)).map_err(|e| {
            WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Error seeking to segment start: {e}"),
            }
        })?;

        // SAFETY: Memory mapping a segment is safe here because:
        // - File handle is freshly opened and seeked to correct position
        // - Segment bounds are calculated from verified file size
        // - Data is immediately copied to Vec, not held across boundaries
        let segment_mmap = unsafe {
            MmapOptions::new().offset(segment_start).len(segment_size as usize).map(&segment_file)
        }
        .map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Failed to memory-map file segment {i}: {e}"),
        })?;

        // Append segment data to result
        result.extend_from_slice(&segment_mmap);
    }

    Ok(result)
}

/// Read extremely large file with streaming
///
/// This function reads an extremely large file using a streaming approach,
/// which minimizes memory usage by processing the file in small chunks.
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `file_size` - Size of the file
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be read
fn read_streaming(file: &File, file_size: u64, path: &Path) -> Result<Vec<u8>> {
    warn!(
        "Reading extremely large file ({}GB) with streaming approach: {}",
        file_size / 1_000_000_000,
        path.display()
    );

    // For extreme files, pre-allocate a reasonably sized vector and grow as needed
    let initial_capacity = min(file_size as usize, 1_000_000_000); // 1GB initial max
    let mut buffer = Vec::with_capacity(initial_capacity);

    let mut reader = BufReader::with_capacity(4_194_304, file); // 4MB buffer
    const CHUNK_SIZE: usize = 4_194_304; // 4MB chunks
    let mut chunk = vec![0; CHUNK_SIZE];
    let mut bytes_read = 0;

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                bytes_read += n as u64;

                // Log progress every 100MB
                if bytes_read % 100_000_000 < CHUNK_SIZE as u64 {
                    info!(
                        "Read progress for large file {}: {:.2}GB/{:.2}GB ({:.1}%)",
                        path.display(),
                        bytes_read as f64 / 1_000_000_000.0,
                        file_size as f64 / 1_000_000_000.0,
                        bytes_read as f64 * 100.0 / file_size as f64
                    );
                }
            }
            Err(e) => {
                return Err(WinxError::FileAccessError {
                    path: path.to_path_buf(),
                    message: format!("Error reading file chunk at position {bytes_read}: {e}"),
                });
            }
        }
    }

    Ok(buffer)
}

/// Read a specific segment of a file
///
/// This function reads a specific segment of a file using memory mapping
/// or direct I/O, depending on the segment size.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `offset` - Starting offset in bytes
/// * `length` - Length of segment to read in bytes
/// * `max_file_size` - Maximum allowed file size
///
/// # Returns
///
/// A vector containing the file segment contents
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds the size limit
pub fn read_file_segment(
    path: &Path,
    offset: u64,
    length: u64,
    max_file_size: u64,
) -> Result<Vec<u8>> {
    // Get file metadata
    let file = File::open(path).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Error opening file: {e}"),
    })?;

    let metadata = file.metadata().map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to get file metadata: {e}"),
    })?;

    // Check file size
    let file_size = metadata.len();
    if file_size > max_file_size {
        return Err(WinxError::FileTooLarge {
            path: path.to_path_buf(),
            size: file_size,
            max_size: max_file_size,
        });
    }

    // Validate offset and length
    if offset >= file_size {
        return Err(WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Offset {offset} exceeds file size {file_size}"),
        });
    }

    // Adjust length if needed to stay within file bounds
    let length = min(length, file_size - offset);

    // Choose reading strategy based on segment size
    if length < DIRECT_READ_THRESHOLD {
        debug!("Using direct read for file segment: {}", path.display());
        read_segment_direct(&file, offset, length, path)
    } else {
        debug!("Using memory-mapped read for file segment: {}", path.display());
        read_segment_mmap(&file, offset, length, path)
    }
}

/// Read a file segment directly using standard I/O
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `offset` - Starting offset in bytes
/// * `length` - Length of segment to read in bytes
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file segment contents
///
/// # Errors
///
/// Returns an error if the file segment cannot be read
fn read_segment_direct(file: &File, offset: u64, length: u64, path: &Path) -> Result<Vec<u8>> {
    // Create a new file object that can be seeked
    let mut seekable_file = file.try_clone().map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to clone file handle: {e}"),
    })?;

    // Seek to the specified offset
    seekable_file.seek(SeekFrom::Start(offset)).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to seek to offset {offset}: {e}"),
    })?;

    // Read the specified length
    let mut buffer = Vec::with_capacity(length as usize);
    let reader = BufReader::with_capacity(min(length as usize, 64 * 1024), seekable_file);

    // Use take to limit the read to the specified length
    reader.take(length).read_to_end(&mut buffer).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Error reading file segment: {e}"),
    })?;

    Ok(buffer)
}

/// Read a file segment using memory mapping
///
/// # Arguments
///
/// * `file` - Open file handle
/// * `offset` - Starting offset in bytes
/// * `length` - Length of segment to read in bytes
/// * `path` - Path to the file (for error reporting)
///
/// # Returns
///
/// A vector containing the file segment contents
///
/// # Errors
///
/// Returns an error if the file segment cannot be mapped
fn read_segment_mmap(file: &File, offset: u64, length: u64, path: &Path) -> Result<Vec<u8>> {
    // SAFETY: Memory mapping is safe here because:
    // - Offset and length were validated against file size by caller
    // - Data is immediately copied to Vec<u8>, not held
    // - File handle remains valid for duration of the map operation
    let segment_mmap = unsafe { MmapOptions::new().offset(offset).len(length as usize).map(file) }
        .map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Failed to memory-map file segment: {e}"),
        })?;

    // Copy segment data to result
    Ok(segment_mmap.to_vec())
}

/// Read a text file as a string using the optimal reading strategy
///
/// This function reads a file as text, using the most efficient strategy
/// based on the file size.
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `max_file_size` - Maximum allowed file size
///
/// # Returns
///
/// A string containing the file contents
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds the size limit
pub fn read_file_to_string(path: &Path, max_file_size: u64) -> Result<String> {
    let bytes = read_file_optimized(path, max_file_size)?;

    String::from_utf8(bytes).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to decode file as UTF-8: {e}"),
    })
}

/// Read a text file in a parallel, line-by-line fashion
///
/// This processes lines in parallel using Rayon for faster processing
/// of large text files.
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `max_file_size` - Maximum allowed file size
/// * `line_processor` - Function to process each line
///
/// # Returns
///
/// Result indicating success or failure
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds the size limit
pub fn process_text_file_parallel<F>(
    path: &Path,
    max_file_size: u64,
    line_processor: F,
) -> Result<()>
where
    F: Fn(&str) + Sync,
{
    let content = read_file_to_string(path, max_file_size)?;

    // For larger files, use parallel processing
    if content.len() > 1_000_000 {
        // 1MB
        content.par_lines().for_each(|line| {
            line_processor(line);
        });
    } else {
        // For smaller files, process sequentially
        content.lines().for_each(|line| {
            line_processor(line);
        });
    }

    Ok(())
}

/// Read a text file segment as a string
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `offset` - Starting offset in bytes
/// * `length` - Length of segment to read in bytes
/// * `max_file_size` - Maximum allowed file size
///
/// # Returns
///
/// A string containing the file segment contents
///
/// # Errors
///
/// Returns an error if the file segment cannot be read
pub fn read_file_segment_to_string(
    path: &Path,
    offset: u64,
    length: u64,
    max_file_size: u64,
) -> Result<String> {
    let bytes = read_file_segment(path, offset, length, max_file_size)?;

    String::from_utf8(bytes).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to decode file segment as UTF-8: {e}"),
    })
}

/// `ShareableMap` provides a thread-safe memory-mapped file access
///
/// This is useful for providing read-only access to multiple threads
/// without copying the data, especially for large files.
#[derive(Clone)]
pub struct ShareableMap {
    /// The memory-mapped file data
    data: Arc<Mmap>,
    /// The path to the mapped file
    path: PathBuf,
}

impl ShareableMap {
    /// Create a new `ShareableMap` from a file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file to map
    ///
    /// # Returns
    ///
    /// A Result containing the `ShareableMap` or an error
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be mapped
    pub fn new(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error opening file: {e}"),
        })?;

        // Check for empty file
        if file.metadata().map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Failed to get metadata: {e}"),
        })?.len() == 0 {
            return Err(WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: "Cannot memory map empty file".to_string(),
            });
        }

        // SAFETY: ShareableMap wraps the Mmap in Arc for thread-safe sharing.
        // The mapped data is read-only and the Arc ensures the Mmap outlives
        // all references. Users must be aware the underlying file should not
        // be modified while ShareableMap is in use.
        let mmap =
            unsafe { MmapOptions::new().map(&file) }.map_err(|e| WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Failed to memory-map file: {e}"),
            })?;

        Ok(Self { data: Arc::new(mmap), path: path.to_path_buf() })
    }

    /// Create a new `ShareableMap` for a segment of a file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file to map
    /// * `offset` - Starting offset in bytes
    /// * `length` - Length of segment to map in bytes
    ///
    /// # Returns
    ///
    /// A Result containing the `ShareableMap` or an error
    ///
    /// # Errors
    ///
    /// Returns an error if the file segment cannot be mapped
    pub fn new_segment(path: &Path, offset: u64, length: u64) -> Result<Self> {
        if length == 0 {
            return Err(WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: "Cannot memory map segment of length 0".to_string(),
            });
        }

        let file = File::open(path).map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error opening file: {e}"),
        })?;

        // SAFETY: Same as ShareableMap::new, plus:
        // - Caller is responsible for ensuring offset+length is within file bounds
        // - The segment mapping is wrapped in Arc for safe sharing
        let mmap = unsafe { MmapOptions::new().offset(offset).len(length as usize).map(&file) }
            .map_err(|e| WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Failed to memory-map file segment: {e}"),
            })?;

        Ok(Self { data: Arc::new(mmap), path: path.to_path_buf() })
    }

    /// Get the data as a byte slice
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Get the path to the mapped file
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the size of the mapped data
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the mapped data is empty
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(size: usize) -> (NamedTempFile, Vec<u8>) {
        let mut file = NamedTempFile::new().unwrap();
        let mut data = Vec::with_capacity(size);

        // Fill with pattern data (more realistic than zeros)
        for i in 0..size {
            data.push((i % 256) as u8);
        }

        file.write_all(&data).unwrap();
        file.flush().unwrap();

        (file, data)
    }

    #[test]
    fn test_direct_read_small_file() {
        let size = 10 * 1024; // 10KB
        let (file, expected_data) = create_test_file(size);

        let result = read_direct(file.as_file(), size as u64, file.path()).unwrap();
        assert_eq!(result, expected_data);
    }

    #[test]
    fn test_mmap_read() {
        let size = 1 * 1024 * 1024; // 1MB
        let (file, expected_data) = create_test_file(size);

        let result = read_mmap(file.as_file(), file.path()).unwrap();
        assert_eq!(result, expected_data);
    }

    #[test]
    fn test_file_segment_read() {
        let size = 1 * 1024 * 1024; // 1MB
        let (file, data) = create_test_file(size);

        // Read a segment from the middle
        let offset = 100 * 1024; // 100KB
        let length = 200 * 1024; // 200KB
        let expected_segment = &data[offset as usize..(offset + length) as usize];

        let result = read_segment_direct(file.as_file(), offset, length, file.path()).unwrap();
        assert_eq!(result, expected_segment);

        let result = read_segment_mmap(file.as_file(), offset, length, file.path()).unwrap();
        assert_eq!(result, expected_segment);
    }

    #[test]
    fn test_shareable_map() {
        let size = 100 * 1024; // 100KB
        let (file, data) = create_test_file(size);

        let map = ShareableMap::new(file.path()).unwrap();
        assert_eq!(map.as_slice(), &data);

        // Test segment
        let offset = 10 * 1024; // 10KB
        let length = 20 * 1024; // 20KB
        let segment_map = ShareableMap::new_segment(file.path(), offset, length).unwrap();
        assert_eq!(segment_map.as_slice(), &data[offset as usize..(offset + length) as usize]);
    }

    #[test]
    fn test_parallel_processing() {
        // Create a test file with lines
        let mut file = NamedTempFile::new().unwrap();
        let mut lines = Vec::new();

        for i in 0..1000 {
            let line = format!("Line {}\n", i);
            file.write_all(line.as_bytes()).unwrap();
            lines.push(format!("Line {}", i));
        }
        file.flush().unwrap();

        // Test parallel processing
        let processed_lines = std::sync::Mutex::new(Vec::new());

        process_text_file_parallel(file.path(), 1_000_000, |line| {
            processed_lines.lock().unwrap().push(line.to_string());
        })
        .unwrap();

        // Verify results (order may differ due to parallel processing)
        let result = processed_lines.lock().unwrap();
        assert_eq!(result.len(), lines.len());

        // Check that all lines are present
        for line in &lines {
            assert!(result.contains(line));
        }
    }
}
