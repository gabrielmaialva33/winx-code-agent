//! Opt-in Landlock filesystem sandbox (Linux only, EXPERIMENTAL).
//!
//! Set `WINX_SANDBOX=1` before starting winx to confine the winx process and the
//! PTY shell children it spawns: writes are restricted to the workspace (the cwd
//! at startup) plus `/tmp`, and the home directory is deliberately NOT in the
//! allowlist, so a manipulated agent can neither read `~/.ssh` / `~/.aws` nor
//! modify files outside the project.
//!
//! This is coarse and best-effort. A command that needs a path outside the
//! allowlist (a compiler reading `~/.cargo`, say) will fail; extend the allowlist
//! with `WINX_SANDBOX_RO_PATHS` / `WINX_SANDBOX_RW_PATHS` (`:`-separated absolute
//! paths). On kernels without Landlock (< 5.13) it logs a warning and runs
//! unsandboxed (the Landlock default compatibility is best-effort). Off by
//! default: winx is unaffected unless you opt in.
//!
//! Known limitation: `/proc` is read-allowed because many programs need
//! `/proc/self`, and Landlock (ABI v1) cannot scope rules within procfs. That
//! also exposes other processes' `/proc/<pid>/environ`. On a single-user local
//! machine (winx's target) those are same-user processes the agent could already
//! read without the sandbox, so this isn't a regression - but don't rely on the
//! sandbox to hide env-passed secrets from co-tenant processes.

use std::path::Path;

use tracing::{info, warn};

#[cfg(target_os = "linux")]
const DEFAULT_RO_PATHS: &[&str] =
    &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/etc", "/proc", "/sys", "/run", "/opt"];

#[cfg(target_os = "linux")]
const DEFAULT_RW_PATHS: &[&str] = &["/tmp", "/dev", "/var/tmp"];

/// Apply the Landlock sandbox if `WINX_SANDBOX` is set. Infallible: any problem
/// is logged and the process continues (possibly unsandboxed). The cwd at startup
/// is taken as the workspace and granted read-write. MUST be called before the
/// async runtime is built so its worker threads inherit the Landlock domain.
pub fn apply_if_requested() {
    let enabled =
        std::env::var("WINX_SANDBOX").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !enabled {
        return;
    }

    let workspace_root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            warn!(
                "WINX_SANDBOX: cannot determine the cwd ({e}); the workspace may not be writable \
                 under the sandbox - start winx from your project directory."
            );
            std::path::PathBuf::from(".")
        }
    };

    #[cfg(not(target_os = "linux"))]
    {
        let _ = &workspace_root;
        warn!("WINX_SANDBOX is set but Landlock is Linux-only; continuing UNSANDBOXED.");
    }

    #[cfg(target_os = "linux")]
    match apply_linux(&workspace_root) {
        Ok(status) => info!(
            "WINX_SANDBOX active (Landlock {status:?}); writes confined to the workspace + /tmp, \
             home not readable. Extend via WINX_SANDBOX_RO_PATHS / WINX_SANDBOX_RW_PATHS."
        ),
        Err(e) => warn!("WINX_SANDBOX: could not apply Landlock ({e}); continuing UNSANDBOXED."),
    }
}

#[cfg(target_os = "linux")]
fn apply_linux(workspace_root: &Path) -> anyhow::Result<landlock::RulesetStatus> {
    use landlock::{Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI};

    // ABI V1 (kernel 5.13) is the baseline; read access includes Execute, so
    // binaries under the RO system paths still run.
    let abi = ABI::V1;
    let ro = AccessFs::from_read(abi);
    let rw = AccessFs::from_all(abi);

    // `handle_access` declares the full set we govern; each rule grants a subset
    // for its path. Default compatibility is BestEffort, so a kernel missing some
    // (or all) of these degrades to partial/no enforcement instead of erroring.
    let mut ruleset = Ruleset::default().handle_access(rw)?.create()?;

    for path in DEFAULT_RO_PATHS {
        ruleset = add_path(ruleset, Path::new(path), ro)?;
    }
    for path in env_paths("WINX_SANDBOX_RO_PATHS") {
        ruleset = add_path(ruleset, &path, ro)?;
    }
    for path in DEFAULT_RW_PATHS {
        ruleset = add_path(ruleset, Path::new(path), rw)?;
    }
    for path in env_paths("WINX_SANDBOX_RW_PATHS") {
        ruleset = add_path(ruleset, &path, rw)?;
    }
    // The workspace is the whole point of having write access at all.
    ruleset = add_path(ruleset, workspace_root, rw)?;

    Ok(ruleset.restrict_self()?.ruleset)
}

/// Add a `path` rule, skipping paths that don't exist (not every distro has
/// `/opt`, `/lib32`, ...) so a missing entry never aborts the whole ruleset.
#[cfg(target_os = "linux")]
fn add_path(
    ruleset: landlock::RulesetCreated,
    path: &Path,
    access: landlock::BitFlags<landlock::AccessFs>,
) -> anyhow::Result<landlock::RulesetCreated> {
    use landlock::{PathBeneath, PathFd, RulesetCreatedAttr};
    match PathFd::new(path) {
        Ok(fd) => Ok(ruleset.add_rule(PathBeneath::new(fd, access))?),
        Err(_) => Ok(ruleset),
    }
}

/// Parse a `:`-separated list of absolute paths from environment `var`.
#[cfg(target_os = "linux")]
fn env_paths(var: &str) -> Vec<std::path::PathBuf> {
    parse_path_list(&std::env::var(var).unwrap_or_default())
}

/// Split a `:`-separated path list, dropping empty segments (so `""`, `"/a::"`
/// don't yield bogus empty paths).
#[cfg(target_os = "linux")]
fn parse_path_list(value: &str) -> Vec<std::path::PathBuf> {
    value.split(':').filter(|segment| !segment.is_empty()).map(std::path::PathBuf::from).collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_path_list;
    use std::path::PathBuf;

    #[test]
    fn parse_path_list_splits_and_drops_empties() {
        assert!(parse_path_list("").is_empty());
        assert_eq!(parse_path_list("/a:/b"), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(parse_path_list("/a::/b:"), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
