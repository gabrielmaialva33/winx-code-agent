use std::io;
use std::path::{Path, PathBuf};

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
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // If path doesn't exist (creating new file), validate parent
            if let Some(parent) = path.parent() {
                // If parent exists, canonicalize it and check
                if parent.exists() {
                     let canonical_parent = parent.canonicalize().map_err(|e| {
                        PathSecurityError::CanonicalizationFailed { path: parent.to_path_buf(), error: e }
                    })?;
                    // Return the canonical parent joined with the filename
                    // This gives us a "pseudo-canonical" path for the new file
                    canonical_parent.join(path.file_name().unwrap_or_default())
                } else {
                     // If parent also doesn't exist, we rely on the workspace check of the "best effort" path
                     // This is slightly looser but allows recursive directory creation if implemented.
                     // However, standard canonicalize fails.
                     // For security, we might want to just enforce that we are inside workspace by simple string check
                     // or walk up until we find an existing directory.
                     // For now, let's just attempt to resolve relative components manually if possible,
                     // or return error.
                     // But simpler: just fallback to checking the parent recursively?
                     // A simple fallback: just assume the provided path is relative to CWD if relative,
                     // and if absolute, sanitize .. components.

                     // BETTER APPROACH: Walk up until we find an existing directory
                     let mut current = path.to_path_buf();
                     while !current.exists() {
                         if let Some(parent) = current.parent() {
                             current = parent.to_path_buf();
                         } else {
                             // Hit root and it doesn't exist? unwritable.
                             break;
                         }
                     }
                     if current.exists() {
                        let canonical_base = current.canonicalize().map_err(|e| {
                            PathSecurityError::CanonicalizationFailed { path: current.to_path_buf(), error: e }
                        })?;
                        // Reconstruct the full path
                        // This identifies the "real" location of the base
                        // We can't easily reconstruct the full canonical path without resolving the missing components' ..
                        // But if we assume no .. in the missing part, we can join.

                        // For Winx, let's keep it simple: if file doesn't exist, parent MUST exist for now?
                        // Or just allow the error to bubble up if we can't verify safety?
                        // WCGW Python allowed anything under workspace.

                        // Let's return the error for now if simple parent check fails, to match strict security.
                        return Err(PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e });
                     }
                     return Err(PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e });
                }
            } else {
                 return Err(PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e });
            }
        }
        Err(e) => return Err(PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e }),
    };

    // Canonicalize workspace root
    let canonical_workspace = workspace_root.canonicalize().map_err(|e| {
        PathSecurityError::CanonicalizationFailed { path: workspace_root.to_path_buf(), error: e }
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
    std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
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
