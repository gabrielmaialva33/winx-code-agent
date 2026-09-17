//! Portable executable discovery.
//!
//! Winx used to probe for programs with `sh -c 'command -v …'`, which assumes a
//! POSIX shell on `PATH`. Native Windows has no `sh`, and its `bash.exe` may be
//! the WSL launcher in `System32` rather than a usable shell, so lookups are
//! done directly against `PATH` (honoring `PATHEXT` on Windows) and the known
//! Git for Windows install locations.

use std::path::{Path, PathBuf};

/// Locate `program` on `PATH`. A name with a path separator is checked as-is.
/// On Windows, a bare name is tried with every `PATHEXT` suffix (`.exe`,
/// `.cmd`, …) as well as verbatim.
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    let direct = Path::new(program);
    if direct.components().count() > 1 {
        return is_executable_file(direct).then(|| direct.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidate_names(program).into_iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable_file(candidate))
}

/// Whether `program` can be found on `PATH`. Best-effort: used for advisory
/// hints and shell selection, never as a security boundary.
pub fn is_available(program: &str) -> bool {
    find_on_path(program).is_some()
}

/// The `bash` executable Winx should drive. On Unix this is whatever `PATH`
/// resolves. On Windows the Git for Windows shell is preferred and
/// `System32\bash.exe` (the WSL launcher, which needs a distribution and does
/// not honor `PROMPT_COMMAND`) is never selected.
pub fn bash_executable() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_bash_executable()
    }
    #[cfg(not(windows))]
    {
        find_on_path("bash")
    }
}

#[cfg(windows)]
fn windows_bash_executable() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for root in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(base) = std::env::var_os(root) {
            candidates.push(PathBuf::from(&base).join("Git").join("bin").join("bash.exe"));
            candidates
                .push(PathBuf::from(base).join("Git").join("usr").join("bin").join("bash.exe"));
        }
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        let user_git = PathBuf::from(base).join("Programs").join("Git");
        candidates.push(user_git.join("bin").join("bash.exe"));
        candidates.push(user_git.join("usr").join("bin").join("bash.exe"));
    }
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
        .or_else(|| find_on_path("bash").filter(|found| !is_windows_system_launcher(found)))
}

/// `bash.exe` under `System32` and the Store alias under
/// `%LOCALAPPDATA%\Microsoft\WindowsApps` both launch WSL, not a shell.
#[cfg(windows)]
fn is_windows_system_launcher(path: &Path) -> bool {
    let mut launcher_dirs = Vec::new();
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        launcher_dirs.push(PathBuf::from(system_root).join("System32"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        launcher_dirs.push(PathBuf::from(local).join("Microsoft").join("WindowsApps"));
    }
    path.ancestors().any(|ancestor| {
        launcher_dirs.iter().any(|dir| ancestor.as_os_str().eq_ignore_ascii_case(dir.as_os_str()))
    })
}

#[cfg(windows)]
fn candidate_names(program: &str) -> Vec<String> {
    let has_extension = Path::new(program).extension().is_some();
    let mut names = vec![program.to_string()];
    if !has_extension {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        names.extend(
            pathext
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(|extension| format!("{program}{}", extension.to_ascii_lowercase())),
        );
    }
    names
}

#[cfg(not(windows))]
fn candidate_names(program: &str) -> Vec<String> {
    vec![program.to_string()]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{find_on_path, is_available};

    #[test]
    fn empty_and_missing_programs_are_not_found() {
        assert!(find_on_path("").is_none());
        assert!(!is_available("winx-definitely-not-a-real-program-9f8e7d"));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_a_standard_unix_tool_and_rejects_non_executables() {
        assert!(is_available("sh"));
        let temp = tempfile::tempdir().expect("temp dir");
        let plain = temp.path().join("not-executable");
        std::fs::write(&plain, b"data").expect("write");
        assert!(find_on_path(plain.to_str().expect("utf-8 path")).is_none());
    }
}
