//! Exact observed-generation release of session-owned helpers. Runtime owners
//! call `release` only while holding exclusive authoritative session admission.
use crate::errors::{Result, WinxError};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TemporaryReleaseTarget {
    pub file_path: String,
    pub expected_revision: String,
    pub artifact_generation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TemporaryArtifactMetadata {
    pub managed: bool,
    pub artifact_generation: String,
    /// `ReadFiles` does not acquire the runtime owner's exclusive admission gate.
    pub in_use: Option<bool>,
    pub releasable: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalIdentity {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub changed_seconds: i64,
    pub changed_nanos: i64,
    pub modified_seconds: i64,
    pub modified_nanos: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreparedTemporaryTarget {
    pub target: TemporaryReleaseTarget,
    pub identity: PhysicalIdentity,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TemporaryReleaseRequest {
    pub thread_id: String,
    pub workspace_root: PathBuf,
    pub files: Vec<PreparedTemporaryTarget>,
    pub expected_guardian_epoch: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TemporaryReleaseFileResult {
    pub file_path: String,
    pub artifact_generation: String,
    pub status: String,
    pub reclaimed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TemporaryReleaseResult {
    pub files: Vec<TemporaryReleaseFileResult>,
    pub reclaimed_bytes: u64,
    pub partial: bool,
}

pub fn validate_target(workspace: &Path, thread: &str, path: &Path) -> Result<()> {
    let info = super::session_info(workspace, thread);
    if !path.is_absolute()
        || path.components().any(|part| {
            !matches!(
                part,
                std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::Normal(_)
            )
        })
    {
        return Err(failure(path, "unsafe_path: an exact absolute helper path is required"));
    }
    super::validate_session_relative_path(path, &info)?;
    if path
        .components()
        .any(|part| part.as_os_str().to_string_lossy().starts_with(".winx-release-"))
    {
        return Err(failure(path, "unsafe_path: quarantined artifacts require recovery"));
    }
    Ok(())
}

fn failure(path: &Path, message: &str) -> WinxError {
    WinxError::FileOperationDenied { path: path.to_path_buf(), message: message.into() }
}

#[cfg(target_os = "linux")]
fn identity(file: &std::fs::File) -> std::io::Result<PhysicalIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > super::MAX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only bounded regular helpers with one link may be released",
        ));
    }
    Ok(PhysicalIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        changed_seconds: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
    })
}

#[cfg(target_os = "linux")]
fn content(file: &mut std::fs::File) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(super::MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::MAX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "helper exceeds size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn revision(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_digest(Sha256::digest(bytes).as_slice()))
}

#[cfg(target_os = "linux")]
fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

#[cfg(target_os = "linux")]
fn generation(
    workspace: &Path,
    thread: &str,
    path: &Path,
    observed: &PhysicalIdentity,
    revision: &str,
) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    static SECRET: std::sync::LazyLock<[u8; 32]> = std::sync::LazyLock::new(rand::random);
    // HMAC-SHA256 with explicit length framing. The process secret makes a
    // pathname/metadata guess insufficient, and intentionally expires on restart.
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in SECRET.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    let physical = [
        observed.device,
        observed.inode,
        observed.bytes,
        observed.changed_seconds.cast_unsigned(),
        observed.changed_nanos.cast_unsigned(),
        observed.modified_seconds.cast_unsigned(),
        observed.modified_nanos.cast_unsigned(),
    ]
    .into_iter()
    .flat_map(u64::to_le_bytes)
    .collect::<Vec<_>>();
    for part in [
        b"WinxTemporaryRead/v1".as_slice(),
        workspace.as_os_str().as_bytes(),
        thread.as_bytes(),
        path.as_os_str().as_bytes(),
        physical.as_slice(),
        revision.as_bytes(),
    ] {
        inner.update((part.len() as u64).to_le_bytes());
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    format!("temporary-v1:{}", hex_digest(outer.finalize().as_slice()))
}

/// A second bounded read for managed helpers ties content and generation to one
/// no-follow file descriptor. Ordinary source reads retain their existing path.
#[cfg(target_os = "linux")]
pub fn read_managed(
    workspace: &Path,
    thread: &str,
    path: &Path,
) -> Result<Option<(String, TemporaryArtifactMetadata)>> {
    if !path.starts_with(super::session_info(workspace, thread).directory) {
        return Ok(None);
    }
    validate_target(workspace, thread, path)?;
    let mut opened = crate::os::temporary::open(path).map_err(|_| {
        failure(path, "unsafe_path: cannot open managed helper without following links")
    })?;
    let before = identity(&opened.file).map_err(|_| {
        failure(path, "unsafe_path: helper is not a supported single-link regular file")
    })?;
    let bytes = content(&mut opened.file)
        .map_err(|_| failure(path, "io_error: cannot read managed helper"))?;
    if identity(&opened.file).ok().as_ref() != Some(&before) {
        return Err(failure(path, "generation_conflict: helper changed during read"));
    }
    let artifact_generation = generation(workspace, thread, path, &before, &revision(&bytes));
    let text =
        String::from_utf8(bytes).map_err(|_| failure(path, "managed helper is not UTF-8"))?;
    Ok(Some((
        text,
        TemporaryArtifactMetadata {
            managed: true,
            artifact_generation,
            in_use: None,
            releasable: None,
        },
    )))
}

#[cfg(not(target_os = "linux"))]
pub fn read_managed(
    _workspace: &Path,
    _thread: &str,
    _path: &Path,
) -> Result<Option<(String, TemporaryArtifactMetadata)>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
pub fn prepare(
    workspace: &Path,
    thread: &str,
    targets: &[TemporaryReleaseTarget],
) -> Result<TemporaryReleaseRequest> {
    let mut files = Vec::with_capacity(targets.len());
    for target in targets {
        let path = Path::new(&target.file_path);
        validate_target(workspace, thread, path)?;
        let mut opened = crate::os::temporary::open(path)
            .map_err(|_| failure(path, "unsafe_path: helper cannot be opened safely"))?;
        let observed = identity(&opened.file)
            .map_err(|_| failure(path, "unsafe_path: helper physical identity is unavailable"))?;
        let bytes = content(&mut opened.file)
            .map_err(|_| failure(path, "io_error: helper cannot be read"))?;
        if identity(&opened.file).ok().as_ref() != Some(&observed) {
            return Err(failure(path, "generation_conflict: helper changed during validation"));
        }
        if revision(&bytes) != target.expected_revision {
            return Err(failure(path, "revision_conflict: re-read the helper"));
        }
        let current = generation(workspace, thread, path, &observed, &target.expected_revision);
        if current.len() != target.artifact_generation.len()
            || current
                .bytes()
                .zip(target.artifact_generation.bytes())
                .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
                != 0
        {
            return Err(failure(
                path,
                "generation_conflict: a fresh ReadFiles generation is required",
            ));
        }
        files.push(PreparedTemporaryTarget { target: target.clone(), identity: observed });
    }
    Ok(TemporaryReleaseRequest {
        thread_id: thread.into(),
        workspace_root: workspace.into(),
        files,
        expected_guardian_epoch: None,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn prepare(
    workspace: &Path,
    _thread: &str,
    _targets: &[TemporaryReleaseTarget],
) -> Result<TemporaryReleaseRequest> {
    Err(failure(workspace, "unsupported: safe temporary release is unavailable on this platform"))
}

#[cfg(target_os = "linux")]
pub fn release(request: &TemporaryReleaseRequest) -> Result<TemporaryReleaseResult> {
    release_with_hook(request, || {})
}

#[cfg(target_os = "linux")]
fn release_with_hook(
    request: &TemporaryReleaseRequest,
    before_claim: impl FnOnce(),
) -> Result<TemporaryReleaseResult> {
    if request.files.is_empty() || request.files.len() > 100 {
        return Err(failure(&request.workspace_root, "release requires 1–100 exact targets"));
    }
    protect_backing_roots(request)?;
    let mut plans = Vec::with_capacity(request.files.len());
    let mut seen = std::collections::HashSet::new();
    // Keep every inspected inode open through commit; reuse cannot impersonate
    // an observed object between preflight and its atomic claim.
    for expected in &request.files {
        let path = Path::new(&expected.target.file_path);
        validate_target(&request.workspace_root, &request.thread_id, path)?;
        if !seen.insert(path) {
            return Err(failure(path, "duplicate release target"));
        }
        let mut opened = crate::os::temporary::open(path)
            .map_err(|_| failure(path, "unsafe_path: cannot open release target"))?;
        if identity(&opened.file).ok().as_ref() != Some(&expected.identity) {
            return Err(failure(path, "generation_conflict: target was replaced or changed"));
        }
        let bytes = content(&mut opened.file)
            .map_err(|_| failure(path, "io_error: cannot inspect release target"))?;
        if revision(&bytes) != expected.target.expected_revision
            || identity(&opened.file).ok().as_ref() != Some(&expected.identity)
        {
            return Err(failure(path, "revision_conflict: target changed before release"));
        }
        plans.push(opened);
    }
    before_claim();
    let mut result = TemporaryReleaseResult::default();
    let mut stopped = false;
    for (expected, opened) in request.files.iter().zip(plans) {
        let mut row = TemporaryReleaseFileResult {
            file_path: expected.target.file_path.clone(),
            artifact_generation: expected.target.artifact_generation.clone(),
            status: "not_attempted".into(),
            reclaimed_bytes: 0,
            recovery_path: None,
        };
        if !stopped {
            if let Ok(claim) = opened.claim() {
                let recovery = Path::new(&row.file_path).parent().map(|parent| {
                    parent.join(claim.recovery_name()).to_string_lossy().into_owned()
                });
                let valid = claim
                    .open_captured()
                    .and_then(|mut file| {
                        let current = identity(&file)?;
                        // Rename changes ctime itself. Dev/inode stay bound to the
                        // pinned preflight handle; bytes are checked again.
                        Ok(current.device == expected.identity.device
                            && current.inode == expected.identity.inode
                            && revision(&content(&mut file)?) == expected.target.expected_revision)
                    })
                    .unwrap_or(false);
                if !valid {
                    drop(claim);
                    row.status = "generation_conflict".into();
                    row.recovery_path =
                        recovery.filter(|path| std::fs::symlink_metadata(path).is_ok());
                    stopped = true;
                } else if claim.remove().is_ok() {
                    row.status = "released".into();
                    row.reclaimed_bytes = expected.identity.bytes;
                    result.reclaimed_bytes =
                        result.reclaimed_bytes.saturating_add(row.reclaimed_bytes);
                } else {
                    row.status = "io_error".into();
                    row.recovery_path =
                        recovery.filter(|path| std::fs::symlink_metadata(path).is_ok());
                    stopped = true;
                }
            } else {
                row.status = "io_error".into();
                stopped = true;
            }
        }
        result.files.push(row);
    }
    result.partial = stopped && result.files.iter().any(|row| row.status == "released");
    Ok(result)
}

#[cfg(not(target_os = "linux"))]
pub fn release(request: &TemporaryReleaseRequest) -> Result<TemporaryReleaseResult> {
    Err(failure(
        &request.workspace_root,
        "unsupported: safe temporary release is unavailable on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn protect_backing_roots(request: &TemporaryReleaseRequest) -> Result<()> {
    let managed = super::session_info(&request.workspace_root, &request.thread_id).directory;
    let home = home::home_dir().ok_or_else(|| {
        failure(&managed, "unresolved_reference: private history root is unavailable")
    })?;
    let mut remaining = super::MAX_SCAN_ENTRIES;
    for backing in
        [request.workspace_root.join(".winx/scratch"), home.join(".local/share/winx/executions")]
    {
        let directory = match crate::os::temporary::directory(&backing) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(failure(
                &managed,
                "unresolved_reference: output backing hierarchy cannot be opened without aliases",
            )),
        };
        // The no-follow hierarchy proves this canonicalization contains no
        // symlink components under the supported runtime/file-tool boundary.
        if backing.canonicalize().map_or(true, |actual| actual.starts_with(&managed)) {
            return Err(failure(
                &managed,
                "referenced: output backing root aliases managed helpers",
            ));
        }
        inspect_backing_directory(&directory, request, 0, &mut remaining)
            .map_err(|_| failure(&managed,
                "unresolved_reference: retained output inspection found an alias, unsupported entry, inaccessible path, or scan limit"))?;
    }
    Ok(())
}

/// Depth-first traversal retains only a bounded stack of open directories.
/// Every leaf is inspected without following links; unknown protection retains
/// all requested helpers. The guardian admission gate excludes managed shell
/// topology mutations, and supported file tools cannot create symlink aliases.
#[cfg(target_os = "linux")]
fn inspect_backing_directory(
    directory: &std::fs::File,
    request: &TemporaryReleaseRequest,
    depth: usize,
    remaining: &mut usize,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let unknown = || std::io::Error::other("retained backing protection is unknown");
    if depth > super::MAX_SCAN_DEPTH || *remaining == 0 {
        return Err(unknown());
    }
    *remaining -= 1;
    for entry in crate::os::temporary::directory_entries(directory)? {
        if *remaining == 0 {
            return Err(unknown());
        }
        *remaining -= 1;
        let entry = entry?;
        let metadata = entry.metadata()?; // DirEntry::metadata never follows a leaf symlink.
        if metadata.is_dir() {
            let child = crate::os::temporary::directory_child(directory, &entry.file_name())?;
            inspect_backing_directory(&child, request, depth + 1, remaining)?;
        } else if !metadata.is_file()
            || request.files.iter().any(|expected| {
                expected.identity.device == metadata.dev()
                    && expected.identity.inode == metadata.ino()
            })
        {
            return Err(unknown());
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::fs;

    fn fixture() -> anyhow::Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let directory = tempfile::tempdir()?;
        let workspace = directory.path().canonicalize()?;
        let helper = super::super::session_info(&workspace, "one").directory.join("helper.txt");
        fs::create_dir_all(helper.parent().unwrap())?;
        fs::write(&helper, "original\n")?;
        Ok((directory, workspace, helper))
    }

    fn target(workspace: &Path, path: &Path) -> anyhow::Result<TemporaryReleaseTarget> {
        let (text, metadata) = read_managed(workspace, "one", path)?
            .ok_or_else(|| anyhow::anyhow!("managed metadata missing"))?;
        Ok(TemporaryReleaseTarget {
            file_path: path.to_string_lossy().into_owned(),
            expected_revision: revision(text.as_bytes()),
            artifact_generation: metadata.artifact_generation,
        })
    }

    #[test]
    fn release_frees_exact_young_helpers_at_the_file_cap() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let parent = helper.parent().unwrap();
        for index in 1..super::super::MAX_SESSION_FILES {
            fs::write(parent.join(format!("keep-{index}")), "keep")?;
        }
        let request = prepare(&workspace, "one", &[target(&workspace, &helper)?])?;
        let result = release(&request)?;
        assert_eq!(result.files[0].status, "released");
        assert_eq!(result.reclaimed_bytes, 9);
        assert!(!helper.exists());
        assert_eq!(fs::read_dir(parent)?.count(), 127);
        fs::write(parent.join("new-helper"), "new")?;
        assert_eq!(fs::read_dir(parent)?.count(), 128);
        Ok(())
    }

    #[test]
    fn same_content_replacement_invalidates_the_observed_generation() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let observed = target(&workspace, &helper)?;
        let replacement = helper.with_file_name("replacement");
        fs::write(&replacement, "original\n")?;
        fs::rename(&replacement, &helper)?;
        assert!(prepare(&workspace, "one", &[observed]).is_err());
        assert_eq!(fs::read_to_string(&helper)?, "original\n");
        Ok(())
    }

    #[test]
    fn release_preflights_the_entire_batch_before_mutation() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let second = helper.with_file_name("second");
        fs::write(&second, "second")?;
        let request = prepare(
            &workspace,
            "one",
            &[target(&workspace, &helper)?, target(&workspace, &second)?],
        )?;
        fs::write(&second, "changed")?;
        assert!(release(&request).is_err());
        assert_eq!(fs::read_to_string(&helper)?, "original\n");
        assert_eq!(fs::read_to_string(&second)?, "changed");
        Ok(())
    }

    #[test]
    fn replacement_installed_between_preflight_and_claim_is_preserved() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let request = prepare(&workspace, "one", &[target(&workspace, &helper)?])?;
        let replacement = helper.with_file_name("replacement");
        fs::write(&replacement, "original\n")?;
        let result = release_with_hook(&request, || fs::rename(&replacement, &helper).unwrap())?;
        assert_eq!(result.files[0].status, "generation_conflict");
        assert_eq!(result.reclaimed_bytes, 0);
        assert_eq!(fs::read_to_string(&helper)?, "original\n");
        Ok(())
    }

    #[test]
    fn release_rejects_other_scope_source_symlink_and_hardlink() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let observed = target(&workspace, &helper)?;
        assert!(prepare(&workspace, "two", std::slice::from_ref(&observed)).is_err());
        let source = workspace.join("source.rs");
        fs::write(&source, "source")?;
        let mut source_target = observed.clone();
        source_target.file_path = source.to_string_lossy().into_owned();
        assert!(prepare(&workspace, "one", &[source_target]).is_err());
        let linked = helper.with_file_name("linked");
        fs::hard_link(&helper, &linked)?;
        assert!(prepare(&workspace, "one", std::slice::from_ref(&observed)).is_err());
        fs::remove_file(&linked)?;
        fs::remove_file(&helper)?;
        std::os::unix::fs::symlink(&source, &helper)?;
        assert!(prepare(&workspace, "one", &[observed]).is_err());
        assert_eq!(fs::read_to_string(source)?, "source");
        Ok(())
    }

    #[test]
    fn retained_output_namespace_is_preserved_and_aliases_block_cleanup() -> anyhow::Result<()> {
        let (_dir, workspace, helper) = fixture()?;
        let scratch = workspace.join(".winx/scratch");
        fs::create_dir_all(&scratch)?;
        let output = scratch.join("bash-output.txt");
        fs::write(&output, "retained output")?;
        release(&prepare(&workspace, "one", &[target(&workspace, &helper)?])?)?;
        assert_eq!(fs::read_to_string(&output)?, "retained output");
        fs::write(&helper, "again")?;
        fs::remove_file(&output)?;
        fs::remove_dir(&scratch)?;
        std::os::unix::fs::symlink(helper.parent().unwrap(), &scratch)?;
        let request = prepare(&workspace, "one", &[target(&workspace, &helper)?])?;
        assert!(release(&request).is_err());
        assert_eq!(fs::read_to_string(helper)?, "again");
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod retained_alias_tests {
    use super::*;

    #[test]
    fn retained_output_leaf_alias_prevents_helper_release() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().canonicalize()?;
        let helper = super::super::session_info(&workspace, "retained").directory.join("helper");
        std::fs::create_dir_all(
            helper.parent().ok_or_else(|| anyhow::anyhow!("helper parent missing"))?,
        )?;
        std::fs::write(&helper, "retained bytes")?;
        let (text, metadata) = read_managed(&workspace, "retained", &helper)?
            .ok_or_else(|| anyhow::anyhow!("read metadata missing"))?;
        let observed = TemporaryReleaseTarget {
            file_path: helper.to_string_lossy().into_owned(),
            expected_revision: revision(text.as_bytes()),
            artifact_generation: metadata.artifact_generation,
        };
        let scratch = workspace.join(".winx/scratch");
        std::fs::create_dir_all(&scratch)?;
        let retained = scratch.join("bash-output-old.txt");
        std::os::unix::fs::symlink(&helper, &retained)?;
        let request = prepare(&workspace, "retained", &[observed])?;
        assert!(
            release(&request).is_err(),
            "a real retained output path still promises these bytes"
        );
        assert_eq!(std::fs::read_to_string(retained)?, "retained bytes");
        Ok(())
    }
}
