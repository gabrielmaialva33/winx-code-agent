use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing::warn;

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
                    "Path traversal detected: '{}' escapes workspace '{}'. To allow paths outside \
                     the workspace, set WINX_ALLOW_PATHS in the winx server config (read once at \
                     startup — restart required; exporting it in a shell has no effect)",
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

/// Parse the `:`-separated `WINX_ALLOW_PATHS` value into canonical roots.
///
/// Entries must be absolute (relative to *what* would be ambiguous — the tools
/// that use this run with a per-session cwd) and must exist, so that the roots
/// are canonical and comparable with `starts_with`. Anything else is dropped
/// with a warning: a typo'd root silently widening nothing is the safe failure.
fn parse_allow_paths(raw: &str) -> Vec<PathBuf> {
    raw.split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let candidate = Path::new(entry);
            if !candidate.is_absolute() {
                warn!("WINX_ALLOW_PATHS: ignoring '{entry}' (must be an absolute path)");
                return None;
            }
            match candidate.canonicalize() {
                Ok(root) => Some(root),
                Err(e) => {
                    warn!("WINX_ALLOW_PATHS: ignoring '{entry}' ({e})");
                    None
                }
            }
        })
        .collect()
}

/// Extra roots the file tools may reach outside the workspace, from
/// `WINX_ALLOW_PATHS` (`:`-separated absolute paths, same convention as
/// `WINX_SANDBOX_RW_PATHS`).
///
/// Read from the environment ONCE, at first use. That is deliberate: the
/// containment policy is set by whoever starts the server, out of band, and is
/// not renegotiable at runtime — no tool argument and no shell command the model
/// is talked into running can widen it mid-session.
///
/// `WINX_ALLOW_PATHS=/` degenerates to "unconfined" (every canonical path on
/// unix starts with `/`), which is the intended way to turn containment off — an
/// explicit, visible config value rather than a separate kill-switch flag.
fn allowed_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let roots = crate::config::env_text("WINX_ALLOW_PATHS").map(|raw| parse_allow_paths(&raw));
        let roots = roots.unwrap_or_default();
        if !roots.is_empty() {
            let list = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
            warn!(
                "WINX_ALLOW_PATHS active: the file tools may read and write outside the \
                 workspace, under: {list}"
            );
        }
        roots
    })
}

/// Effective operator-configured roots outside the workspace. Exposed for
/// diagnostics so `doctor` reports the same once-read policy the file tools use.
pub fn configured_allowed_roots() -> &'static [PathBuf] {
    allowed_roots()
}

/// Containment predicate: a resolved path is acceptable if it is inside the
/// workspace or inside one of the operator-configured extra roots.
fn is_contained(
    canonical_path: &Path,
    canonical_workspace: &Path,
    extra_roots: &[PathBuf],
) -> bool {
    canonical_path.starts_with(canonical_workspace)
        || extra_roots.iter().any(|root| canonical_path.starts_with(root))
}

/// Validates that a path is within the workspace root (or an extra root allowed
/// via `WINX_ALLOW_PATHS`). Returns the canonicalized path if valid.
///
/// # Security
/// - Prevents path traversal attacks (../)
/// - Detects symlinks pointing outside the allowed roots
/// - Canonicalizes path before comparison
pub fn validate_path_in_workspace(
    path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, PathSecurityError> {
    validate_path_with_roots(path, workspace_root, allowed_roots())
}

/// The real implementation, with the allowed roots injected so tests can drive
/// every case without touching process-global environment state.
pub(crate) fn validate_path_with_roots(
    path: &Path,
    workspace_root: &Path,
    extra_roots: &[PathBuf],
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
            if !is_contained(&canonical_target, &canonical_workspace, extra_roots) {
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
            if is_contained(&canonical_path, &canonical_workspace, extra_roots) {
                Ok(canonical_path)
            } else {
                Err(PathSecurityError::PathTraversal {
                    path: path.to_path_buf(),
                    workspace: canonical_workspace,
                })
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            resolve_new_path(path, &canonical_workspace, extra_roots)
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
fn resolve_new_path(
    path: &Path,
    canonical_workspace: &Path,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, PathSecurityError> {
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

    if is_contained(&resolved, canonical_workspace, extra_roots) {
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
    resolve_in_workspace_with_roots(path, cwd, workspace_root, allowed_roots())
}

/// Deterministic variant used by security tests and helpers that need to inject
/// the exact containment policy instead of inheriting process-global overrides.
pub(crate) fn resolve_in_workspace_with_roots(
    path: &str,
    cwd: &Path,
    workspace_root: &Path,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, PathSecurityError> {
    if path.trim().is_empty() {
        return validate_path_with_roots(workspace_root, workspace_root, extra_roots);
    }
    let expanded = expand_user(path);
    let candidate = if Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        cwd.join(expanded)
    };
    validate_path_with_roots(&candidate, workspace_root, extra_roots)
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
            if let Ok(resolved) = resolve_in_workspace_with_roots(&rel, &ws, &ws, &[]) {
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
            if let Ok(p) = validate_path_with_roots(Path::new(&s), &ws, &[]) {
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
        let v = validate_path_with_roots(&f, ws.path(), &[]).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
    }

    #[test]
    fn allows_new_file_in_nested_nonexistent_dir() {
        // mkdir -p semantics: a deep, not-yet-existing path resolves and stays
        // contained (validation happens before the dirs are created).
        let ws = TempDir::new().unwrap();
        let f = ws.path().join("new/deep/dir/file.txt");
        let v = validate_path_with_roots(&f, ws.path(), &[]).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
        assert!(v.ends_with("new/deep/dir/file.txt"));
    }

    #[test]
    fn rejects_traversal_via_dotdot_in_new_path() {
        let ws = TempDir::new().unwrap();
        let f = ws.path().join("nope/../../etc/passwd");
        assert!(matches!(
            validate_path_with_roots(&f, ws.path(), &[]),
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
            validate_path_with_roots(&f, ws.path(), &[]),
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
            validate_path_with_roots(&link, ws.path(), &[]),
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
        assert!(validate_path_with_roots(&f, ws.path(), &[]).is_err());
    }

    #[test]
    fn parse_allow_paths_keeps_absolute_existing_roots_only() {
        let existing = TempDir::new().unwrap();
        let existing_str = existing.path().to_string_lossy().to_string();
        let raw = format!("{existing_str}: :relative/dir:/definitely/not/here/xyz");
        let roots = parse_allow_paths(&raw);
        assert_eq!(roots, vec![existing.path().canonicalize().unwrap()]);
    }

    #[test]
    fn allowed_root_permits_existing_path_outside_workspace() {
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let f = outside.path().join("note.md");
        fs::write(&f, "x").unwrap();
        let roots = vec![outside.path().canonicalize().unwrap()];

        // Without the allowlist it is a traversal; with it, it resolves.
        assert!(validate_path_with_roots(&f, ws.path(), &[]).is_err());
        let v = validate_path_with_roots(&f, ws.path(), &roots).unwrap();
        assert!(v.starts_with(outside.path().canonicalize().unwrap()));
    }

    #[test]
    fn allowed_root_permits_new_file_outside_workspace() {
        // The `mkdir -p` path (resolve_new_path) must honour the extra roots too.
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let f = outside.path().join("new/deep/file.txt");
        let roots = vec![outside.path().canonicalize().unwrap()];

        assert!(validate_path_with_roots(&f, ws.path(), &[]).is_err());
        let v = validate_path_with_roots(&f, ws.path(), &roots).unwrap();
        assert!(v.ends_with("new/deep/file.txt"));
    }

    #[test]
    fn allowed_root_does_not_widen_to_its_siblings() {
        // Allowing one root must not allow everything above or beside it.
        let ws = TempDir::new().unwrap();
        let allowed = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let f = other.path().join("secret.txt");
        fs::write(&f, "s").unwrap();
        let roots = vec![allowed.path().canonicalize().unwrap()];
        assert!(matches!(
            validate_path_with_roots(&f, ws.path(), &roots),
            Err(PathSecurityError::PathTraversal { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn root_slash_allow_path_disables_containment() {
        // The documented way to turn containment off: every canonical unix path
        // starts with `/`, so `WINX_ALLOW_PATHS=/` accepts anything that resolves.
        // Asserted against a temp dir rather than a system path, and compared to
        // the CANONICAL form: validation always returns a canonicalized path, and
        // on macOS the system prefixes are symlinks (`/etc` -> `/private/etc`,
        // `/var` -> `/private/var`), so a literal comparison would be wrong there.
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let f = outside.path().join("anywhere.txt");
        fs::write(&f, "x").unwrap();
        let roots = parse_allow_paths("/");
        assert_eq!(roots, vec![PathBuf::from("/")]);

        assert!(validate_path_with_roots(&f, ws.path(), &[]).is_err());
        let v = validate_path_with_roots(&f, ws.path(), &roots).unwrap();
        assert_eq!(v, f.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_into_allowed_root_is_accepted() {
        // A symlink out of the workspace is an escape only if the target is
        // outside every allowed root.
        use std::os::unix::fs::symlink;
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("data.txt");
        fs::write(&target, "d").unwrap();
        let link = ws.path().join("link.txt");
        symlink(&target, &link).unwrap();
        let roots = vec![outside.path().canonicalize().unwrap()];

        assert!(matches!(
            validate_path_with_roots(&link, ws.path(), &[]),
            Err(PathSecurityError::SymlinkEscape { .. })
        ));
        assert!(validate_path_with_roots(&link, ws.path(), &roots).is_ok());
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
        let v = validate_path_with_roots(&f, ws.path(), &[]).unwrap();
        assert!(v.starts_with(ws.path().canonicalize().unwrap()));
    }
}
