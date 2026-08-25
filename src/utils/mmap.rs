//! Race-safe bounded file reads.
//!
//! The module name is retained for source compatibility, but project files are
//! intentionally no longer memory-mapped. A repository file can be truncated
//! by another editor or agent at any time; copying through `Read` keeps those
//! races inside Rust's safe I/O model.

use std::cmp::min;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(unix))]
use std::time::SystemTime;
use std::{path::Path, path::PathBuf, sync::Arc};

use rayon::prelude::*;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use tracing::debug;

use crate::errors::{Result, WinxError};

/// Legacy threshold retained for callers that used the old tuning constants.
pub const DIRECT_READ_THRESHOLD: u64 = 10_000_000;
/// Historical single-map ceiling, now the maximum `ShareableMap` snapshot size.
pub const MAX_MMAP_SIZE: u64 = 1_000_000_000;
/// Legacy segmented-map ceiling retained for API compatibility.
pub const MAX_SEGMENTED_MMAP_SIZE: u64 = 4_000_000_000;
/// Legacy map segment size retained for API compatibility.
pub const SEGMENT_SIZE: u64 = 256_000_000;

const READ_CHUNK_SIZE: usize = 256 * 1024;
const MAX_PREALLOC_BYTES: usize = 64 * 1024 * 1024;
const MAX_STABILITY_ATTEMPTS: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileVersion {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(not(unix))]
    modified: Option<SystemTime>,
}

impl From<&Metadata> for FileVersion {
    fn from(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            modified_seconds: metadata.mtime(),
            #[cfg(unix)]
            modified_nanoseconds: metadata.mtime_nsec(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
            #[cfg(not(unix))]
            modified: metadata.modified().ok(),
        }
    }
}

fn access_error(path: &Path, operation: &str, error: impl std::fmt::Display) -> WinxError {
    WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("{operation}: {error}"),
    }
}

fn open_with_version(path: &Path, max_file_size: u64) -> Result<(File, FileVersion)> {
    let file = File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WinxError::FileNotFound { path: path.to_path_buf() },
        std::io::ErrorKind::PermissionDenied => {
            WinxError::FileOperationDenied { path: path.to_path_buf(), message: error.to_string() }
        }
        _ => access_error(path, "Error opening file", error),
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| access_error(path, "Failed to get file metadata", error))?;
    let version = FileVersion::from(&metadata);
    if version.len > max_file_size {
        return Err(WinxError::FileTooLarge {
            path: path.to_path_buf(),
            size: version.len,
            max_size: max_file_size,
        });
    }
    Ok((file, version))
}

fn bounded_capacity(reported_size: u64, limit: u64) -> usize {
    let bounded = min(reported_size, limit);
    usize::try_from(bounded).unwrap_or(usize::MAX).min(MAX_PREALLOC_BYTES)
}

fn read_bounded<R: Read>(
    reader: &mut R,
    reported_size: u64,
    limit: u64,
    path: &Path,
) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    result.try_reserve(bounded_capacity(reported_size, limit)).map_err(|error| {
        WinxError::ResourceAllocationError {
            message: format!(
                "Unable to reserve a bounded file buffer for {}: {error}",
                path.display()
            ),
        }
    })?;
    let mut chunk = vec![0_u8; READ_CHUNK_SIZE];

    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(access_error(path, "Error reading file", error)),
        };
        let observed = u64::try_from(result.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        if observed > limit {
            return Err(WinxError::FileTooLarge {
                path: path.to_path_buf(),
                size: observed,
                max_size: limit,
            });
        }
        result.try_reserve(count).map_err(|error| WinxError::ResourceAllocationError {
            message: format!("Unable to grow file buffer for {}: {error}", path.display()),
        })?;
        result.extend_from_slice(&chunk[..count]);
    }
    Ok(result)
}

fn snapshot_is_stable(file: &File, path: &Path, before: &FileVersion, observed: u64) -> bool {
    let descriptor_after = file.metadata().ok().as_ref().map(FileVersion::from);
    let path_after = std::fs::metadata(path).ok().as_ref().map(FileVersion::from);
    let length_matches = before.len == 0 || observed == before.len;
    descriptor_after.as_ref() == Some(before)
        && path_after.as_ref() == Some(before)
        && length_matches
}

fn changed_during_read(path: &Path) -> WinxError {
    WinxError::ConcurrentFileModification {
        path: path.to_path_buf(),
        attempts: MAX_STABILITY_ATTEMPTS,
    }
}

fn read_file_optimized_with_hook<F>(
    path: &Path,
    max_file_size: u64,
    mut after_read: F,
) -> Result<Vec<u8>>
where
    F: FnMut(usize, &Path),
{
    for attempt in 0..MAX_STABILITY_ATTEMPTS {
        let (mut file, before) = open_with_version(path, max_file_size)?;
        let bytes = read_bounded(&mut file, before.len, max_file_size, path)?;
        after_read(attempt, path);
        let observed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if snapshot_is_stable(&file, path, &before, observed) {
            return Ok(bytes);
        }
        debug!(
            path = %path.display(),
            attempt = attempt + 1,
            "file changed during bounded read; retrying from a fresh descriptor"
        );
    }
    Err(changed_during_read(path))
}

/// Read a complete file through bounded, race-detecting safe I/O.
pub fn read_file_optimized(path: &Path, max_file_size: u64) -> Result<Vec<u8>> {
    read_file_optimized_with_hook(path, max_file_size, |_, _| {})
}

/// Read a specific byte segment through bounded, race-detecting safe I/O.
pub fn read_file_segment(
    path: &Path,
    offset: u64,
    length: u64,
    max_file_size: u64,
) -> Result<Vec<u8>> {
    for attempt in 0..MAX_STABILITY_ATTEMPTS {
        let (mut file, before) = open_with_version(path, max_file_size)?;
        if offset >= before.len {
            return Err(WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Offset {offset} exceeds file size {}", before.len),
            });
        }
        let requested = min(length, before.len - offset);
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| access_error(path, "Failed to seek to requested offset", error))?;
        let bytes = {
            let mut limited = (&mut file).take(requested);
            read_bounded(&mut limited, requested, requested, path)?
        };
        let observed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if observed == requested && snapshot_is_stable(&file, path, &before, before.len) {
            return Ok(bytes);
        }
        debug!(
            path = %path.display(),
            attempt = attempt + 1,
            "file changed during segment read; retrying from a fresh descriptor"
        );
    }
    Err(changed_during_read(path))
}

/// Read a UTF-8 file through the bounded snapshot reader.
pub fn read_file_to_string(path: &Path, max_file_size: u64) -> Result<String> {
    let bytes = read_file_optimized(path, max_file_size)?;
    String::from_utf8(bytes)
        .map_err(|error| access_error(path, "Failed to decode file as UTF-8", error))
}

/// Process UTF-8 lines in parallel once the immutable snapshot exceeds 1 MB.
pub fn process_text_file_parallel<F>(
    path: &Path,
    max_file_size: u64,
    line_processor: F,
) -> Result<()>
where
    F: Fn(&str) + Sync,
{
    let content = read_file_to_string(path, max_file_size)?;
    if content.len() > 1_000_000 {
        content.par_lines().for_each(&line_processor);
    } else {
        content.lines().for_each(line_processor);
    }
    Ok(())
}

/// Read one UTF-8 byte segment through the bounded snapshot reader.
pub fn read_file_segment_to_string(
    path: &Path,
    offset: u64,
    length: u64,
    max_file_size: u64,
) -> Result<String> {
    let bytes = read_file_segment(path, offset, length, max_file_size)?;
    String::from_utf8(bytes)
        .map_err(|error| access_error(path, "Failed to decode file segment as UTF-8", error))
}

/// Cloneable immutable file snapshot retained under the historical API name.
#[derive(Clone)]
pub struct ShareableMap {
    data: Arc<[u8]>,
    path: PathBuf,
}

impl ShareableMap {
    /// Snapshot a non-empty file without retaining an alias to the live inode.
    pub fn new(path: &Path) -> Result<Self> {
        let data = read_file_optimized(path, MAX_MMAP_SIZE)?;
        if data.is_empty() {
            return Err(WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: "Cannot create a shared snapshot of an empty file".to_string(),
            });
        }
        Ok(Self { data: Arc::from(data), path: path.to_path_buf() })
    }

    /// Snapshot a non-empty file segment without retaining a live mapping.
    pub fn new_segment(path: &Path, offset: u64, length: u64) -> Result<Self> {
        if length == 0 {
            return Err(WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: "Cannot create a shared snapshot of a zero-length segment".to_string(),
            });
        }
        let file_size = std::fs::metadata(path)
            .map_err(|error| access_error(path, "Failed to get file metadata", error))?
            .len();
        let data = read_file_segment(path, offset, length, file_size)?;
        Ok(Self { data: Arc::from(data), path: path.to_path_buf() })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation)]

    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::NamedTempFile;

    use super::*;

    fn create_test_file(size: usize) -> Result<(NamedTempFile, Vec<u8>)> {
        let mut file = NamedTempFile::new()?;
        let data = (0..size).map(|index| (index % 256) as u8).collect::<Vec<_>>();
        file.write_all(&data)?;
        file.flush()?;
        Ok((file, data))
    }

    #[test]
    fn bounded_reader_preserves_small_and_large_files() -> Result<()> {
        for size in [10 * 1024, 11 * 1024 * 1024] {
            let (file, expected) = create_test_file(size)?;
            assert_eq!(read_file_optimized(file.path(), size as u64)?, expected);
        }
        Ok(())
    }

    #[test]
    fn concurrent_replacement_retries_from_a_fresh_descriptor() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        file.write_all(b"first revision")?;
        file.flush()?;
        let replaced = AtomicBool::new(false);

        let result = read_file_optimized_with_hook(file.path(), 1_024, |attempt, path| {
            if attempt == 0 && !replaced.swap(true, Ordering::AcqRel) {
                let replaced = std::fs::write(path, b"second revision is current");
                assert!(replaced.is_ok(), "failed to replace test file: {replaced:?}");
            }
        })?;

        assert_eq!(result, b"second revision is current");
        Ok(())
    }

    #[test]
    fn file_size_limit_is_enforced_before_allocating_the_payload() -> Result<()> {
        let (file, _) = create_test_file(4_096)?;
        assert!(matches!(
            read_file_optimized(file.path(), 1_024),
            Err(WinxError::FileTooLarge { .. })
        ));
        Ok(())
    }

    #[test]
    fn file_segment_read_uses_the_same_stable_snapshot_contract() -> Result<()> {
        let (file, data) = create_test_file(1024 * 1024)?;
        let offset = 100 * 1024;
        let length = 200 * 1024;
        let expected = &data[offset..offset + length];
        let result =
            read_file_segment(file.path(), offset as u64, length as u64, data.len() as u64)?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn shareable_map_is_an_owned_snapshot() -> Result<()> {
        let (file, data) = create_test_file(100 * 1024)?;
        let snapshot = ShareableMap::new(file.path())?;
        std::fs::write(file.path(), b"changed after snapshot")?;
        assert_eq!(snapshot.as_slice(), &data);

        let segment = ShareableMap::new_segment(snapshot.path(), 0, 7)?;
        assert_eq!(segment.as_slice(), b"changed");
        Ok(())
    }

    #[test]
    fn parallel_processing_observes_every_line() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        let expected = (0..1_000).map(|index| format!("Line {index}")).collect::<Vec<_>>();
        for line in &expected {
            writeln!(file, "{line}")?;
        }
        file.flush()?;

        let processed = std::sync::Mutex::new(Vec::new());
        process_text_file_parallel(file.path(), 1_000_000, |line| {
            if let Ok(mut entries) = processed.lock() {
                entries.push(line.to_string());
            }
        })?;
        let result = processed.lock().map_err(|error| WinxError::ResourceAllocationError {
            message: format!("Failed to lock processed lines: {error}"),
        })?;
        assert_eq!(result.len(), expected.len());
        assert!(expected.iter().all(|line| result.contains(line)));
        Ok(())
    }
}
