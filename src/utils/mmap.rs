use memmap2::MmapOptions;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use tracing::{debug, warn};

use crate::errors::{Result, WinxError};

/// Maximum file size for direct reading (10MB)
pub const DIRECT_READ_THRESHOLD: u64 = 10_000_000;

/// Maximum file size for memory mapping (1GB)
pub const MAX_MMAP_SIZE: u64 = 1_000_000_000;

/// Read file contents optimally based on file size
///
/// This function chooses the optimal reading strategy based on file size:
/// - Small files: Direct read with standard File I/O
/// - Medium files: Memory-mapped reading for performance
/// - Large files: Returns an error (exceeds maximum allowed size)
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
        message: format!("Error opening file: {}", e),
    })?;

    let metadata = file.metadata().map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to get file metadata: {}", e),
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
    } else {
        // This shouldn't happen due to the max_file_size check above,
        // but we include it for completeness
        warn!("File too large for memory mapping: {}", path.display());
        Err(WinxError::FileTooLarge {
            path: path.to_path_buf(),
            size: file_size,
            max_size: MAX_MMAP_SIZE,
        })
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
    let mut reader = BufReader::with_capacity(std::cmp::min(file_size as usize, 64 * 1024), file);
    let mut buffer = Vec::with_capacity(file_size as usize);

    reader
        .read_to_end(&mut buffer)
        .map_err(|e| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("Error reading file: {}", e),
        })?;

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
    // Safety: We've already checked the file size and permissions
    let mmap = unsafe { MmapOptions::new().map(file) }.map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to memory-map file: {}", e),
    })?;

    // Copy the mapped data to a Vec<u8>
    Ok(mmap.to_vec())
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
        message: format!("Failed to decode file as UTF-8: {}", e),
    })
}
