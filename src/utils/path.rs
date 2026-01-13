use std::path::{Path, PathBuf};
use std::io;

/// Security error for path validation
#[derive(Debug)]
pub enum PathSecurityError {
    /// Path escapes the workspace root (path traversal attempt)
    PathTraversal { path: PathBuf, workspace: PathBuf },
    /// Path is a symlink pointing outside workspace
    SymlinkEscape { path: PathBuf, target: PathBuf, workspace: PathBuf },
    /// Failed to canonicalize path
    CanonicalizationFailed { path: PathBuf, error: io::Error },
}

impl std::fmt::Display for PathSecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathSecurityError::PathTraversal { path, workspace } => {
                write!(
                    f,
                    "Path traversal detected: '{}' escapes workspace '{}'",
                    path.display(),
                    workspace.display()
                )
            }
            PathSecurityError::SymlinkEscape { path, target, workspace } => {
                write!(
                    f,
                    "Symlink escape detected: '{}' points to '{}' outside workspace '{}'",
                    path.display(),
                    target.display(),
                    workspace.display()
                )
            }
            PathSecurityError::CanonicalizationFailed { path, error } => {
                write!(f, "Failed to resolve path '{}': {}", path.display(), error)
            }
        }
    }
}

impl std::error::Error for PathSecurityError {}

/// Validates that a path is within the workspace root.
/// Returns the canonicalized path if valid.
///
/// # Security
/// - Prevents path traversal attacks (../)
/// - Detects symlinks pointing outside workspace
/// - Canonicalizes path before comparison
pub fn validate_path_in_workspace(
    path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, PathSecurityError> {
    // First, check if it's a symlink and validate target
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            // Resolve the symlink target
            if let Ok(target) = std::fs::read_link(path) {
                let absolute_target = if target.is_absolute() {
                    target.clone()
                } else {
                    path.parent().unwrap_or(Path::new("/")).join(&target)
                };

                // Canonicalize target and check if it's in workspace
                if let Ok(canonical_target) = absolute_target.canonicalize() {
                    if let Ok(canonical_workspace) = workspace_root.canonicalize() {
                        if !canonical_target.starts_with(&canonical_workspace) {
                            return Err(PathSecurityError::SymlinkEscape {
                                path: path.to_path_buf(),
                                target: canonical_target,
                                workspace: canonical_workspace,
                            });
                        }
                    }
                }
            }
        }
    }

    // Canonicalize the path (resolves .., symlinks, etc.)
    let canonical_path = path.canonicalize().map_err(|e| {
        PathSecurityError::CanonicalizationFailed {
            path: path.to_path_buf(),
            error: e,
        }
    })?;

    // Canonicalize workspace root
    let canonical_workspace = workspace_root.canonicalize().map_err(|e| {
        PathSecurityError::CanonicalizationFailed {
            path: workspace_root.to_path_buf(),
            error: e,
        }
    })?;

    // Check if path is within workspace
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(PathSecurityError::PathTraversal {
            path: path.to_path_buf(),
            workspace: canonical_workspace,
        });
    }

    Ok(canonical_path)
}

/// Check if a path is a symlink without following it
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Expands a path that starts with ~ to the user's home directory
pub fn expand_user(path: &str) -> String {
    if path.starts_with('~') {
        if let Some(home_dir) = home::home_dir() {
            return path.replacen('~', home_dir.to_str().unwrap_or(""), 1);
        }
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
