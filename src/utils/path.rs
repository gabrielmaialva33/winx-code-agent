use std::path::Path;

/// Expands a path that starts with ~ to the user's home directory
pub fn expand_user(path: &str) -> String {
    if path.starts_with('~')
        && let Some(home_dir) = home::home_dir()
    {
        return path.replacen('~', home_dir.to_str().unwrap_or(""), 1);
    }
    path.to_string()
}

/// Ensures a directory exists, creating it if necessary
pub fn ensure_directory_exists(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}
