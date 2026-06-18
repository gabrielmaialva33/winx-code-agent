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
    // Resolve the workspace boundary once, up front; everything is checked
    // against this. Fail closed if the workspace itself can't be canonicalized.
    let canonical_workspace = workspace_root.canonicalize().map_err(|e| {
        PathSecurityError::CanonicalizationFailed { path: workspace_root.to_path_buf(), error: e }
    })?;

    // If `path` itself is a symlink, resolve its target and reject if it escapes.
    // Fail CLOSED: a target we can't resolve (e.g. dangling) is refused, not
    // waved through (the old code silently passed on any resolution failure).
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(path).map_err(|e| {
                PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e }
            })?;
            let absolute_target = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or(Path::new("/")).join(&target)
            };
            let canonical_target = absolute_target.canonicalize().map_err(|e| {
                PathSecurityError::CanonicalizationFailed {
                    path: absolute_target.clone(),
                    error: e,
                }
            })?;
            if !canonical_target.starts_with(&canonical_workspace) {
                return Err(PathSecurityError::SymlinkEscape {
                    path: path.to_path_buf(),
                    target: canonical_target,
                    workspace: canonical_workspace,
                });
            }
        }
    }

    // Resolve `path`. If it exists, canonicalize() collapses `..`, resolves
    // symlinks, etc. If it doesn't (creating a new file/dir), fall back to a
    // lexical resolution of the not-yet-existing tail.
    match path.canonicalize() {
        Ok(canonical_path) => {
            if canonical_path.starts_with(&canonical_workspace) {
                Ok(canonical_path)
            } else {
                Err(PathSecurityError::PathTraversal {
                    path: path.to_path_buf(),
                    workspace: canonical_workspace,
                })
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            resolve_new_path(path, &canonical_workspace)
        }
        Err(e) => {
            Err(PathSecurityError::CanonicalizationFailed { path: path.to_path_buf(), error: e })
        }
    }
}

/// Resolve a not-yet-existing path for containment checking (supports creating
/// files in directories that don't exist yet — i.e. `mkdir -p` semantics).
///
/// Strategy: walk up to the first ancestor that exists *as a filesystem entry*,
/// canonicalize it (resolving any real symlinks in the existing prefix), then
/// re-apply the remaining (non-existent) components purely lexically — `..`
/// pops, `.` is skipped — and verify the result is inside `canonical_workspace`.
///
/// Two subtleties make this safe:
/// - We stop the walk-up on `symlink_metadata` (entry exists), NOT `exists()`
///   (which follows symlinks). A dangling symlink in an intermediate component
///   therefore becomes the canonicalize target and fails closed, instead of
///   being treated as a fresh lexical component that a later `create_dir_all`
///   would follow out of the workspace.
/// - The lexical pass resolves `..` before the containment check, so
///   `workspace/new/../../etc/passwd` is rejected.
fn resolve_new_path(path: &Path, canonical_workspace: &Path) -> Result<PathBuf, PathSecurityError> {
    let traversal = || PathSecurityError::PathTraversal {
        path: path.to_path_buf(),
        workspace: canonical_workspace.to_path_buf(),
    };

    // Walk up to the first ancestor that exists as a filesystem entry.
    let mut existing = path;
    loop {
        if std::fs::symlink_metadata(existing).is_ok() {
            break;
        }
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }

    // The deepest existing ancestor must resolve (a dangling symlink here fails
    // closed) and anchor the resolution.
    let canonical_base = existing.canonicalize().map_err(|e| {
        PathSecurityError::CanonicalizationFailed { path: existing.to_path_buf(), error: e }
    })?;

    // Apply the components after `existing` lexically (they don't exist yet, so
    // there are no symlinks among them to follow).
    let remainder = path.strip_prefix(existing).map_err(|_| traversal())?;
    let mut resolved = canonical_base;
    for component in remainder.components() {
        match component {
            std::path::Component::Normal(c) => resolved.push(c),
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            // RootDir / Prefix must not appear in a relative remainder.
            _ => return Err(traversal()),
        }
    }

    if resolved.starts_with(canonical_workspace) {
        Ok(resolved)
    } else {
        Err(traversal())
    }
}

/// Check if a path is a symlink without following it
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Expands a path that starts with ~ to the user's home directory
pub fn expand_user(path: &str) -> String {
    if path.starts_with('~') {
        // Only expand when home is known AND valid UTF-8. The old `to_str()
        // .unwrap_or("")` mapped `~/x` to `/x` (the filesystem root!) on a
        // non-UTF-8 $HOME — silently pointing at the wrong place. Leaving the
        // literal `~` is the safe failure.
        if let Some(home_str) = home::home_dir().as_deref().and_then(Path::to_str) {
            return path.replacen('~', home_str, 1);
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

/// Resolve a user-supplied path string (possibly empty, relative, or `~`) into a
/// workspace-confined absolute path. Empty input resolves to the workspace root.
/// Used by the read-only search/glob tools to scope and confine their root.
pub fn resolve_in_workspace(
    path: &str,
    cwd: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, PathSecurityError> {
    if path.trim().is_empty() {
        return validate_path_in_workspace(workspace_root, workspace_root);
    }
    let expanded = expand_user(path);
    let candidate = if Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        cwd.join(expanded)
    };
    validate_path_in_workspace(&candidate, workspace_root)
}

/// Match a glob against a workspace-relative path. `*`/`?` do NOT cross `/`
/// (so `src/*.ts` matches only direct children — `**` is the recursive form,
/// matching `find`/`bash` semantics); a bare pattern (e.g. `*.rs`) also matches
/// the file name at any depth, giving the intuitive "all .rs files".
pub fn glob_matches(pattern: &glob::Pattern, relative: &Path) -> bool {
    let opts = glob::MatchOptions { require_literal_separator: true, ..glob::MatchOptions::new() };
    if pattern.matches_path_with(relative, opts) {
        return true;
    }
    relative
        .file_name()
        .is_some_and(|name| pattern.matches_with(name.to_string_lossy().as_ref(), opts))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::TempDir;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The containment invariant the whole file-safety story rests on: no
        /// relative path string — however many `..`/`.`/odd segments — can make
        /// resolve_in_workspace return Ok pointing OUTSIDE the workspace. Either it
        /// errors, or the result is contained. (Uses the real /tmp as a stand-in
        /// workspace so each case is read-only and cheap.)
        #[test]
        fn resolve_in_workspace_ok_implies_contained(
            segments in prop::collection::vec(
                prop_oneof![Just("..".to_string()), Just(".".to_string()), "[a-zA-Z0-9_]{1,5}"],
                0..10,
            )
        ) {
            let ws = std::env::temp_dir().canonicalize().unwrap();
            let rel = segments.join("/");
            if let Ok(resolved) = resolve_in_workspace(&rel, &ws, &ws) {
                prop_assert!(
                    resolved.starts_with(&ws),
                    "resolve_in_workspace({rel:?}) escaped to {resolved:?}"
                );
            }
        }

        /// Same invariant at the lower layer, for ANY input string (incl. absolute
        /// paths, traversal, junk): validate_path_in_workspace must never accept a
        /// path that escapes, and must never panic.
        #[test]
        fn validate_ok_implies_contained_any_input(s in ".*") {
            let ws = std::env::temp_dir().canonicalize().unwrap();
            if let Ok(p) = validate_path_in_workspace(Path::new(&s), &ws) {
                prop_assert!(p.starts_with(&ws), "accepted escaping path {p:?} from input {s:?}");
            }
        }
    }

    #[test]
    fn expand_user_leaves_non_tilde_paths_untouched() {
        assert_eq!(expand_user("/abs/path"), "/abs/path");
        assert_eq!(expand_user("rel/path"), "rel/path");
    }

    #[test]
    fn expand_user_never_maps_tilde_to_root() {
        // Regression: a non-UTF-8 (or unknown) $HOME used to turn `~/sub` into
        // `/sub` (the filesystem root). It must expand to a home-prefixed path or
        // stay literal — never collapse to root.
        let out = expand_user("~/sub");
        assert_ne!(out, "/sub", "~/sub must not become the filesystem root");
        if let Some(home) = home::home_dir().and_then(|h| h.to_str().map(String::from)) {
            assert_eq!(out, format!("{home}/sub"));
        }
    }

    #[test]
    fn allows_existing_file_in_workspace() {
        let ws = TempDir::new().unwrap();
        let f = ws.path().join("a.txt");
        fs::write(&f, "x").unwrap();
        let v = validate_path_in_workspace(&f, ws.path()).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
    }

    #[test]
    fn allows_new_file_in_nested_nonexistent_dir() {
        // mkdir -p semantics: a deep, not-yet-existing path resolves and stays
        // contained (validation happens before the dirs are created).
        let ws = TempDir::new().unwrap();
        let f = ws.path().join("new/deep/dir/file.txt");
        let v = validate_path_in_workspace(&f, ws.path()).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
        assert!(v.ends_with("new/deep/dir/file.txt"));
    }

    #[test]
    fn rejects_traversal_via_dotdot_in_new_path() {
        let ws = TempDir::new().unwrap();
        let f = ws.path().join("nope/../../etc/passwd");
        assert!(matches!(
            validate_path_in_workspace(&f, ws.path()),
            Err(PathSecurityError::PathTraversal { .. })
        ));
    }

    #[test]
    fn rejects_existing_path_outside_workspace() {
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let f = outside.path().join("secret.txt");
        fs::write(&f, "s").unwrap();
        assert!(matches!(
            validate_path_in_workspace(&f, ws.path()),
            Err(PathSecurityError::PathTraversal { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escaping_workspace() {
        use std::os::unix::fs::symlink;
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "s").unwrap();
        let link = ws.path().join("link.txt");
        symlink(&secret, &link).unwrap();
        assert!(matches!(
            validate_path_in_workspace(&link, ws.path()),
            Err(PathSecurityError::SymlinkEscape { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_new_file_through_dangling_symlink_dir() {
        // An intermediate component is a dangling symlink pointing outside the
        // workspace. The walk-up must stop on it (symlink_metadata) and fail
        // closed, NOT treat it as a fresh lexical component that a later
        // create_dir_all would follow out of the workspace.
        use std::os::unix::fs::symlink;
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let link = ws.path().join("evil");
        symlink(outside.path().join("nonexistent"), &link).unwrap();
        let f = link.join("file.txt");
        assert!(validate_path_in_workspace(&f, ws.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn allows_new_file_through_internal_symlink_dir() {
        // A symlink to a directory INSIDE the workspace is fine; the resolved
        // path stays contained.
        use std::os::unix::fs::symlink;
        let ws = TempDir::new().unwrap();
        let real = ws.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = ws.path().join("link");
        symlink(&real, &link).unwrap();
        let f = link.join("file.txt");
        let v = validate_path_in_workspace(&f, ws.path()).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
    }
}
