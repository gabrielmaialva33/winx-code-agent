use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// One socket location that may contain a Winx control daemon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonSocketCandidate {
    pub path: PathBuf,
    pub sources: Vec<&'static str>,
    pub selected: bool,
}

fn socket_under(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("winx/winxd.sock")
}

fn fallback_socket(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/winx-{uid}/winxd.sock"))
}

fn canonical_user_runtime_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}"))
}

/// Only trust an implicit runtime directory owned exclusively by this user.
fn usable_runtime_dir(path: &Path, uid: u32) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    let mode = metadata.permissions().mode();
    metadata.is_dir() && metadata.uid() == uid && mode.trailing_zeros() >= 6
}

fn select_socket_path(
    explicit: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
    canonical_runtime_dir: Option<PathBuf>,
    uid: u32,
) -> PathBuf {
    explicit
        .or_else(|| xdg_runtime_dir.map(|path| socket_under(&path)))
        .or_else(|| canonical_runtime_dir.map(|path| socket_under(&path)))
        .unwrap_or_else(|| fallback_socket(uid))
}

/// Resolve the canonical control-daemon socket without creating filesystem state.
///
/// Launchers that do not inherit `XDG_RUNTIME_DIR` still converge on
/// `/run/user/<uid>` when the private user runtime directory is available. This
/// prevents one installation from silently creating independent daemons under
/// both `/run/user` and `/tmp`.
pub fn default_socket_path() -> PathBuf {
    let uid = crate::os::unix::effective_uid();
    let explicit =
        std::env::var_os("WINX_SOCKET").filter(|value| !value.is_empty()).map(PathBuf::from);
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| usable_runtime_dir(path, uid));
    let canonical_runtime_dir = canonical_user_runtime_dir(uid);
    let canonical_runtime_dir =
        usable_runtime_dir(&canonical_runtime_dir, uid).then_some(canonical_runtime_dir);
    select_socket_path(explicit, xdg_runtime_dir, canonical_runtime_dir, uid)
}

fn push_candidate(
    candidates: &mut Vec<DaemonSocketCandidate>,
    path: PathBuf,
    source: &'static str,
    selected: &Path,
) {
    if let Some(existing) = candidates.iter_mut().find(|candidate| candidate.path == path) {
        if !existing.sources.contains(&source) {
            existing.sources.push(source);
        }
        existing.selected |= existing.path == selected;
        return;
    }
    candidates.push(DaemonSocketCandidate {
        selected: path == selected,
        path,
        sources: vec![source],
    });
}

/// Enumerate canonical and legacy socket locations for diagnostics.
pub fn socket_candidates() -> Vec<DaemonSocketCandidate> {
    let uid = crate::os::unix::effective_uid();
    let selected = default_socket_path();
    let mut candidates = Vec::with_capacity(4);

    if let Some(path) = std::env::var_os("WINX_SOCKET").filter(|value| !value.is_empty()) {
        push_candidate(&mut candidates, PathBuf::from(path), "WINX_SOCKET", &selected);
    }
    if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        push_candidate(
            &mut candidates,
            socket_under(&PathBuf::from(path)),
            "XDG_RUNTIME_DIR",
            &selected,
        );
    }

    push_candidate(
        &mut candidates,
        socket_under(&canonical_user_runtime_dir(uid)),
        "canonical_user_runtime",
        &selected,
    );
    push_candidate(&mut candidates, fallback_socket(uid), "legacy_tmp_fallback", &selected);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_socket_always_wins() {
        let selected = select_socket_path(
            Some(PathBuf::from("/custom/winx.sock")),
            Some(PathBuf::from("/xdg")),
            Some(PathBuf::from("/run/user/42")),
            42,
        );
        assert_eq!(selected, PathBuf::from("/custom/winx.sock"));
    }

    #[test]
    fn missing_xdg_converges_on_canonical_user_runtime() {
        let selected = select_socket_path(None, None, Some(PathBuf::from("/run/user/42")), 42);
        assert_eq!(selected, PathBuf::from("/run/user/42/winx/winxd.sock"));
    }

    #[test]
    fn tmp_is_only_the_last_resort() {
        assert_eq!(
            select_socket_path(None, None, None, 42),
            PathBuf::from("/tmp/winx-42/winxd.sock")
        );
    }

    #[test]
    fn duplicate_candidate_paths_merge_their_sources() {
        let selected = Path::new("/run/user/42/winx/winxd.sock");
        let mut candidates = Vec::new();
        push_candidate(&mut candidates, selected.to_path_buf(), "XDG_RUNTIME_DIR", selected);
        push_candidate(&mut candidates, selected.to_path_buf(), "canonical_user_runtime", selected);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].selected);
        assert_eq!(candidates[0].sources.len(), 2);
    }
}
