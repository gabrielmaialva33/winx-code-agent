use anyhow::{Context, Result};
use glob::glob;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Read a file's contents as string
pub async fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    debug!("Reading file: {}", path.display());

    fs::read_to_string(path).with_context(|| format!("Failed to read file: {}", path.display()))
}

/// Read a file's contents as string (synchronous version)
pub fn read_file_to_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    debug!("Reading file synchronously: {}", path.display());

    fs::read_to_string(path).with_context(|| format!("Failed to read file: {}", path.display()))
}

/// Write string content to a file
pub async fn write_file(path: impl AsRef<Path>, content: &str) -> Result<()> {
    let path = path.as_ref();
    debug!("Writing to file: {}", path.display());

    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    fs::write(path, content).with_context(|| format!("Failed to write to file: {}", path.display()))
}

/// Write string content to a file (synchronous version)
pub fn write_file_sync(path: impl AsRef<Path>, content: &str) -> Result<()> {
    let path = path.as_ref();
    debug!("Writing to file synchronously: {}", path.display());

    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    fs::write(path, content).with_context(|| format!("Failed to write to file: {}", path.display()))
}

/// Check if a file exists
pub fn file_exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    path.exists() && path.is_file()
}

/// Find files matching a glob pattern
pub fn find_files(pattern: &str) -> Result<Vec<PathBuf>> {
    debug!("Finding files matching pattern: {}", pattern);

    let paths = glob(pattern)
        .with_context(|| format!("Invalid glob pattern: {}", pattern))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();

    debug!("Found {} files matching pattern", paths.len());
    Ok(paths)
}

/// Create a directory and all parent directories
pub fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    debug!("Creating directory: {}", path.display());

    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory: {}", path.display()))
}

/// Expand ~ to user's home directory in path
pub fn expand_user(path: &str) -> String {
    if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            if path.len() > 1 {
                return home.join(&path[2..]).to_string_lossy().to_string();
            } else {
                return home.to_string_lossy().to_string();
            }
        }
    }
    path.to_string()
}

/// Create a temporary directory for file operations
pub fn create_temp_dir(base_dir: impl AsRef<Path>, prefix: &str) -> Result<PathBuf> {
    let base_dir = base_dir.as_ref();

    // Generate a unique timestamp
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Create a unique directory name
    let temp_dir_name = format!("{}_{}", prefix, timestamp);
    let temp_dir = base_dir.join("tmp").join(temp_dir_name);

    // Create the directory
    if !temp_dir.exists() {
        debug!("Creating temporary directory: {}", temp_dir.display());
        fs::create_dir_all(&temp_dir).with_context(|| {
            format!(
                "Failed to create temporary directory: {}",
                temp_dir.display()
            )
        })?;
    }

    Ok(temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::runtime::Runtime;

    #[test]
    fn test_file_operations() {
        let rt = Runtime::new().unwrap();

        rt.block_on(async {
            let dir = tempdir().unwrap();
            let file_path = dir.path().join("test.txt");

            // Test writing to file
            write_file(&file_path, "Hello, world!").await.unwrap();
            assert!(file_exists(&file_path));

            // Test reading from file
            let content = read_file(&file_path).await.unwrap();
            assert_eq!(content, "Hello, world!");
        });
    }

    #[test]
    fn test_find_files() {
        let dir = tempdir().unwrap();

        // Create some test files
        fs::write(dir.path().join("test1.txt"), "").unwrap();
        fs::write(dir.path().join("test2.txt"), "").unwrap();
        fs::create_dir_all(dir.path().join("subdir")).unwrap();
        fs::write(dir.path().join("subdir").join("test3.txt"), "").unwrap();

        // Test glob pattern
        let pattern = dir.path().join("*.txt").to_string_lossy().to_string();
        let files = find_files(&pattern).unwrap();
        assert_eq!(files.len(), 2);

        // Test recursive glob pattern
        let pattern = dir.path().join("**/*.txt").to_string_lossy().to_string();
        let files = find_files(&pattern).unwrap();
        assert_eq!(files.len(), 3);
    }
}
