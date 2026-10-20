//! Self-update from GitHub Releases.
//!
//! `winx-code-agent update` fetches the release metadata, downloads the bundle
//! for this platform together with the published `SHA256SUMS`, verifies the
//! digest, extracts the three sibling binaries, and swaps them in next to the
//! running executable. Downloads go through the system `curl` and extraction
//! through the system `tar` (bsdtar on Windows also reads zip archives), so the
//! server binary carries no TLS stack of its own and inherits the user's proxy
//! configuration. Running daemons keep the old executables alive until they
//! exit; `restart-daemon` picks up the new `winxd`, and guardians are replaced
//! as their sessions end.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::errors::{Result, WinxError};

/// GitHub repository that publishes the release bundles.
pub const REPOSITORY: &str = "gabrielmaialva33/winx-code-agent";
/// Checksum manifest published with every release.
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";
/// Sibling binaries every bundle must contain.
pub const BINARIES: [&str; 3] = ["winx-code-agent", "winxd", "winx-guardian"];

/// A published release and its downloadable assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

/// What `update` decided and did.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateReport {
    pub current_version: String,
    pub latest_version: String,
    pub latest_tag: String,
    pub asset: String,
    pub install_dir: PathBuf,
    pub action: UpdateAction,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub installed: Vec<PathBuf>,
    pub next_step: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    /// Nothing downloaded: `--check`, or already on the requested version.
    UpToDate,
    /// `--check` found a newer version and stopped.
    UpdateAvailable,
    /// The bundle was verified and installed.
    Installed,
}

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Report only; never download.
    pub check_only: bool,
    /// Install this tag (`v0.2.351`) instead of the latest release.
    pub tag: Option<String>,
    /// Reinstall even when the version already matches.
    pub force: bool,
    /// Directory to install into; defaults to the running executable's directory.
    pub install_dir: Option<PathBuf>,
}

/// Resolve, verify, and install a release according to `options`.
pub fn run(options: &UpdateOptions) -> Result<UpdateReport> {
    let current_version = crate::build_info::package_version().to_string();
    let asset_name = platform_asset_name()?;
    let install_dir = match &options.install_dir {
        Some(dir) => dir.clone(),
        None => default_install_dir()?,
    };
    let release = fetch_release(options.tag.as_deref())?;
    let newer = version_is_newer(&release.version, &current_version);
    let same = release.version == current_version;
    let mut report = UpdateReport {
        current_version,
        latest_version: release.version.clone(),
        latest_tag: release.tag.clone(),
        asset: asset_name.to_string(),
        install_dir: install_dir.clone(),
        action: UpdateAction::UpToDate,
        installed: Vec::new(),
        next_step: "nothing to do",
    };

    if options.check_only {
        if newer || (options.tag.is_some() && !same) {
            report.action = UpdateAction::UpdateAvailable;
            report.next_step = "run `winx-code-agent update` to install it";
        }
        return Ok(report);
    }
    if same && !options.force {
        report.next_step = "already on this version; pass --force to reinstall";
        return Ok(report);
    }

    let asset = release.assets.iter().find(|asset| asset.name == asset_name).ok_or_else(|| {
        WinxError::ConfigurationError(format!(
            "release {} publishes no {asset_name} asset",
            release.tag
        ))
    })?;
    let checksums =
        release.assets.iter().find(|asset| asset.name == CHECKSUM_ASSET).ok_or_else(|| {
            WinxError::ConfigurationError(format!(
                "release {} publishes no {CHECKSUM_ASSET} asset",
                release.tag
            ))
        })?;

    let workdir = tempfile::Builder::new().prefix("winx-update-").tempdir().map_err(|error| {
        WinxError::CommandExecutionError(format!("cannot create a download directory: {error}"))
    })?;
    let bundle = workdir.path().join(asset_name);
    download(&asset.url, &bundle)?;
    let manifest = String::from_utf8_lossy(&fetch_bytes(&checksums.url)?).into_owned();
    verify_checksum(&bundle, asset_name, &manifest)?;

    let extracted = workdir.path().join("extracted");
    std::fs::create_dir_all(&extracted).map_err(|error| {
        WinxError::CommandExecutionError(format!("cannot create {}: {error}", extracted.display()))
    })?;
    extract(&bundle, &extracted)?;
    let binaries = locate_binaries(&extracted)?;

    std::fs::create_dir_all(&install_dir).map_err(|error| {
        WinxError::CommandExecutionError(format!(
            "cannot create {}: {error}",
            install_dir.display()
        ))
    })?;
    for source in binaries {
        let file_name = source.file_name().map(OsStr::to_os_string).unwrap_or_default();
        let destination = install_dir.join(file_name);
        replace_binary(&source, &destination)?;
        report.installed.push(destination);
    }
    report.action = UpdateAction::Installed;
    report.next_step = "run `winx-code-agent restart-daemon` so winxd uses the new build; \
                        guardians switch as their sessions end";
    Ok(report)
}

/// Release asset built for this operating system and architecture.
pub fn platform_asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("winx-linux-amd64.tar.gz"),
        ("macos", "aarch64") => Ok("winx-macos-arm64.tar.gz"),
        ("windows", "x86_64") => Ok("winx-windows-amd64.zip"),
        (os, arch) => Err(WinxError::ConfigurationError(format!(
            "no prebuilt release bundle for {os}/{arch}; build from source with `cargo install \
             winx-code-agent`"
        ))),
    }
}

/// Directory that holds the running executable, with symlinks resolved so the
/// swap happens where the files really live.
pub fn default_install_dir() -> Result<PathBuf> {
    let executable = std::env::current_exe().map_err(|error| {
        WinxError::ConfigurationError(format!("cannot locate the current executable: {error}"))
    })?;
    let executable =
        executable.canonicalize().map(crate::utils::path::simplified).map_err(|error| {
            WinxError::ConfigurationError(format!(
                "cannot resolve {}: {error}",
                executable.display()
            ))
        })?;
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        WinxError::ConfigurationError(format!("{} has no parent directory", executable.display()))
    })
}

/// Fetch release metadata for `tag`, or the latest release when `None`.
pub fn fetch_release(tag: Option<&str>) -> Result<Release> {
    let url = match tag {
        Some(tag) => format!("https://api.github.com/repos/{REPOSITORY}/releases/tags/{tag}"),
        None => format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
    };
    let body = run_curl(&["-H", "Accept: application/vnd.github+json", &url], None)?;
    parse_release(&String::from_utf8_lossy(&body))
}

/// Parse the GitHub release API response.
pub fn parse_release(json: &str) -> Result<Release> {
    #[derive(Deserialize)]
    struct ApiAsset {
        name: String,
        browser_download_url: String,
    }
    #[derive(Deserialize)]
    struct ApiRelease {
        tag_name: String,
        #[serde(default)]
        assets: Vec<ApiAsset>,
    }
    let release: ApiRelease = serde_json::from_str(json).map_err(|error| {
        WinxError::SerializationError(format!("unexpected release metadata: {error}"))
    })?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    Ok(Release {
        tag: release.tag_name,
        version,
        assets: release
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset { name: asset.name, url: asset.browser_download_url })
            .collect(),
    })
}

/// `true` when `candidate` is a strictly newer `MAJOR.MINOR.PATCH` than `current`.
/// Unparseable versions never count as newer.
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['+', '-']).next()?;
    let mut parts = core.split('.').map(str::parse::<u64>);
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next()?.ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Parse a `sha256sum`-style manifest into `file name -> lowercase hex digest`.
pub fn parse_checksums(manifest: &str) -> HashMap<String, String> {
    manifest
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (name.to_string(), digest.to_ascii_lowercase()))
        })
        .collect()
}

/// Compare the digest of `path` against the manifest entry for `asset_name`.
pub fn verify_checksum(path: &Path, asset_name: &str, manifest: &str) -> Result<()> {
    let expected = parse_checksums(manifest).remove(asset_name).ok_or_else(|| {
        WinxError::ConfigurationError(format!("{CHECKSUM_ASSET} has no entry for {asset_name}"))
    })?;
    let data = std::fs::read(path).map_err(|error| {
        WinxError::CommandExecutionError(format!("cannot read {}: {error}", path.display()))
    })?;
    let actual = Sha256::digest(&data).iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    if actual != expected {
        return Err(WinxError::ConfigurationError(format!(
            "checksum mismatch for {asset_name}: expected {expected}, downloaded {actual}; \
             the download was discarded"
        )));
    }
    Ok(())
}

/// Find the three sibling binaries under an extracted bundle.
pub fn locate_binaries(extracted: &Path) -> Result<Vec<PathBuf>> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let mut found = Vec::new();
    for name in BINARIES {
        let file_name = format!("{name}{suffix}");
        let direct = extracted.join(&file_name);
        let path =
            if direct.is_file() { Some(direct) } else { find_file(extracted, &file_name, 3) };
        match path {
            Some(path) => found.push(path),
            None => {
                return Err(WinxError::ConfigurationError(format!(
                    "the bundle does not contain {file_name}"
                )))
            }
        }
    }
    Ok(found)
}

fn find_file(root: &Path, file_name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().is_some_and(|name| name == file_name) {
            return Some(path);
        }
        if path.is_dir() {
            directories.push(path);
        }
    }
    directories.into_iter().find_map(|dir| find_file(&dir, file_name, depth - 1))
}

/// Swap `source` into `destination` without ever leaving a half-written
/// executable in place. The new file is staged beside the target and renamed
/// over it; on Windows a running executable cannot be overwritten but can be
/// renamed, so the old one moves aside first.
pub fn replace_binary(source: &Path, destination: &Path) -> Result<()> {
    let staged = sibling_with_suffix(destination, ".new");
    std::fs::copy(source, &staged).map_err(|error| {
        WinxError::CommandExecutionError(format!("cannot stage {}: {error}", staged.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(
            |error| {
                WinxError::CommandExecutionError(format!(
                    "cannot mark {} executable: {error}",
                    staged.display()
                ))
            },
        )?;
    }
    if cfg!(windows) && destination.exists() {
        let retired = sibling_with_suffix(destination, ".old");
        let _ = std::fs::remove_file(&retired);
        std::fs::rename(destination, &retired).map_err(|error| {
            WinxError::CommandExecutionError(format!(
                "cannot move the running {} aside: {error}",
                destination.display()
            ))
        })?;
    }
    std::fs::rename(&staged, destination).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        WinxError::CommandExecutionError(format!(
            "cannot install {}: {error}",
            destination.display()
        ))
    })?;
    if cfg!(windows) {
        // Fails while the old executable is still running; the next update
        // removes it.
        let _ = std::fs::remove_file(sibling_with_suffix(destination, ".old"));
    }
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsStr::to_os_string).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let destination_text = destination.to_string_lossy().into_owned();
    run_curl(&["-o", &destination_text, url], None)?;
    Ok(())
}

fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    run_curl(&[url], None)
}

/// Run the system `curl` with fail-closed defaults and return its stdout.
fn run_curl(args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
    let user_agent = format!("winx-code-agent/{}", crate::build_info::package_version());
    let mut command = Command::new("curl");
    command
        .args(["--fail", "--silent", "--show-error", "--location", "--retry", "3"])
        .args(["--user-agent", &user_agent])
        .args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            WinxError::ConfigurationError(
                "`curl` is required for `update`; install it or download the release bundle \
                 manually"
                    .to_string(),
            )
        } else {
            WinxError::CommandExecutionError(format!("cannot run curl: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(WinxError::CommandExecutionError(format!(
            "curl failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Extract a `.tar.gz` or `.zip` bundle with the system `tar`.
fn extract(bundle: &Path, into: &Path) -> Result<()> {
    let output = Command::new("tar").arg("-xf").arg(bundle).arg("-C").arg(into).output().map_err(
        |error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                WinxError::ConfigurationError(
                    "`tar` is required for `update`; install it or extract the bundle manually"
                        .to_string(),
                )
            } else {
                WinxError::CommandExecutionError(format!("cannot run tar: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(WinxError::CommandExecutionError(format!(
            "tar failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    const RELEASE_JSON: &str = r#"{
        "tag_name": "v0.2.351",
        "name": "v0.2.351",
        "assets": [
            {"name": "SHA256SUMS", "browser_download_url": "https://example.invalid/SHA256SUMS"},
            {"name": "winx-linux-amd64.tar.gz", "browser_download_url": "https://example.invalid/linux"},
            {"name": "winx-windows-amd64.zip", "browser_download_url": "https://example.invalid/win"}
        ]
    }"#;

    #[test]
    fn release_metadata_parses_tag_version_and_assets() {
        let release = parse_release(RELEASE_JSON).expect("release");
        assert_eq!(release.tag, "v0.2.351");
        assert_eq!(release.version, "0.2.351");
        assert_eq!(release.assets.len(), 3);
        assert_eq!(release.assets[1].name, "winx-linux-amd64.tar.gz");
        assert_eq!(release.assets[1].url, "https://example.invalid/linux");
    }

    #[test]
    fn release_metadata_without_assets_is_still_a_release() {
        let release = parse_release(r#"{"tag_name":"v9.9.9"}"#).expect("release");
        assert!(release.assets.is_empty(), "no assets expected");
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn version_comparison_is_numeric_and_ignores_build_metadata() {
        assert!(version_is_newer("0.2.352", "0.2.351"));
        assert!(version_is_newer("v0.3.0", "0.2.999"));
        assert!(version_is_newer("1.0.0+gabc", "0.9.9"));
        assert!(!version_is_newer("0.2.351", "0.2.351"));
        assert!(version_is_newer("0.2.10", "0.2.9+dirty"));
        assert!(version_is_newer("0.2.10", "0.2.9"));
        assert!(!version_is_newer("0.2.9", "0.2.10"));
        assert!(!version_is_newer("garbage", "0.2.351"));
        assert!(!version_is_newer("0.2", "0.1.0"));
    }

    #[test]
    fn checksum_manifest_parses_sha256sum_output() {
        let manifest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  winx-linux-amd64.tar.gz\n\
                        ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789 *winx-windows-amd64.zip\n\
                        not-a-digest  junk\n";
        let sums = parse_checksums(manifest);
        assert_eq!(sums.len(), 2);
        assert_eq!(
            sums["winx-linux-amd64.tar.gz"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            sums["winx-windows-amd64.zip"],
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
    }

    #[test]
    fn checksum_verification_accepts_matching_and_rejects_tampered_bundles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle = dir.path().join("bundle.tar.gz");
        std::fs::write(&bundle, b"hello").expect("write");
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let manifest = format!("{digest}  bundle.tar.gz\n");
        verify_checksum(&bundle, "bundle.tar.gz", &manifest).expect("digest matches");
        std::fs::write(&bundle, b"hello!").expect("write");
        let error = verify_checksum(&bundle, "bundle.tar.gz", &manifest).expect_err("mismatch");
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        let error = verify_checksum(&bundle, "other", &manifest).expect_err("missing entry");
        assert!(error.to_string().contains("no entry"), "{error}");
    }

    #[test]
    fn binaries_are_located_at_the_root_or_one_level_down() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("stage");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let suffix = std::env::consts::EXE_SUFFIX;
        for name in BINARIES {
            std::fs::write(nested.join(format!("{name}{suffix}")), b"x").expect("write");
        }
        let found = locate_binaries(dir.path()).expect("binaries");
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|path| path.starts_with(&nested)));
        std::fs::remove_file(nested.join(format!("winxd{suffix}"))).expect("remove");
        let error = locate_binaries(dir.path()).expect_err("missing winxd");
        assert!(error.to_string().contains("winxd"), "{error}");
    }

    #[test]
    fn replace_binary_swaps_the_file_and_leaves_no_staging_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("src.bin");
        let destination = dir.path().join("bin").join("winxd");
        std::fs::create_dir_all(destination.parent().expect("parent")).expect("mkdir");
        std::fs::write(&destination, b"old").expect("write old");
        std::fs::write(&source, b"new").expect("write new");
        replace_binary(&source, &destination).expect("replace");
        assert_eq!(std::fs::read(&destination).expect("read"), b"new");
        let leftovers: Vec<String> = std::fs::read_dir(destination.parent().expect("parent"))
            .expect("read dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "winxd")
            .collect();
        assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&destination).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "installed binary must be executable");
        }
    }

    #[test]
    fn replace_binary_installs_into_an_empty_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("src.bin");
        std::fs::write(&source, b"new").expect("write new");
        let destination = dir.path().join("fresh");
        replace_binary(&source, &destination).expect("replace");
        assert_eq!(std::fs::read(&destination).expect("read"), b"new");
    }

    #[test]
    fn platform_asset_is_known_for_supported_targets() {
        let name = platform_asset_name();
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(name.expect("asset"), "winx-linux-amd64.tar.gz");
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert_eq!(name.expect("asset"), "winx-windows-amd64.zip");
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(name.expect("asset"), "winx-macos-arm64.tar.gz");
        }
    }

    #[test]
    fn default_install_dir_is_the_running_executables_directory() {
        let dir = default_install_dir().expect("install dir");
        assert!(dir.is_dir(), "{}", dir.display());
    }
}
