//! Bounded single-flight background builds and a process-local decoded index cache.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncReadExt as _;
use tokio::sync::{Mutex, Semaphore};

use super::index::{GraphIndex, MAX_INDEX_BYTES, MAX_KB_BYTES};
use super::snapshot;

const CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WORKSPACES: usize = 16;
static BUILD_SLOT: Semaphore = Semaphore::const_new(1);

#[derive(Clone)]
pub(super) struct View {
    pub index: Option<Arc<GraphIndex>>,
    pub allowed: Arc<std::collections::HashSet<PathBuf>>,
    pub status: &'static str,
    pub error_code: Option<String>,
}

impl View {
    fn memory_bytes(&self) -> usize {
        self.index.as_ref().map_or(0, |index| index.memory_bytes())
            + self.allowed.capacity() * 48
            + self.allowed.iter().map(|path| path.as_os_str().len()).sum::<usize>()
    }
}

struct Entry {
    view: View,
    checking: bool,
    checked: Option<Instant>,
    attempted: Option<Instant>,
    stamp: Option<(u64, SystemTime)>,
    touched: Instant,
}

impl Entry {
    fn new() -> Self {
        Self {
            view: View {
                index: None,
                allowed: Arc::default(),
                status: "building",
                error_code: None,
            },
            checking: false,
            checked: None,
            attempted: None,
            stamp: None,
            touched: Instant::now(),
        }
    }
}

pub(super) struct Service {
    parser: Option<PathBuf>,
    timeout: Duration,
    debounce: Duration,
    entries: Mutex<HashMap<PathBuf, Entry>>,
    builds: AtomicUsize,
}

impl Service {
    pub fn new(parser: Option<PathBuf>, timeout: Duration, debounce: Duration) -> Arc<Self> {
        Arc::new(Self {
            parser,
            timeout,
            debounce,
            entries: Mutex::new(HashMap::new()),
            builds: AtomicUsize::new(0),
        })
    }

    pub async fn request(self: &Arc<Self>, root: &Path) -> View {
        let mut entries = self.entries.lock().await;
        if !entries.contains_key(root) && entries.len() >= MAX_WORKSPACES {
            if let Some(oldest) = entries
                .iter()
                .filter(|(_, e)| !e.checking)
                .min_by_key(|(_, e)| e.touched)
                .map(|(p, _)| p.clone())
            {
                entries.remove(&oldest);
            } else {
                return View {
                    index: None,
                    allowed: Arc::default(),
                    status: "unavailable",
                    error_code: Some("index_busy".into()),
                };
            }
        }
        let entry = entries.entry(root.to_owned()).or_insert_with(|| {
            let mut entry = Entry::new();
            if self.parser.is_none() {
                entry.view.status = "unavailable";
                entry.view.error_code = Some("parser_unavailable".into());
            }
            entry
        });
        entry.touched = Instant::now();
        // Tests use zero debounce to inspect every source change immediately.
        let check_interval = self.debounce.min(Duration::from_secs(1));
        if !entry.checking && entry.checked.is_none_or(|time| time.elapsed() >= check_interval) {
            entry.checking = true;
            let service = self.clone();
            let root = root.to_owned();
            tokio::spawn(async move {
                service.refresh(root).await;
            });
        }
        entry.view.clone()
    }

    #[cfg(all(test, target_os = "linux"))]
    pub async fn checking(&self, root: &Path) -> bool {
        self.entries.lock().await.get(root).is_some_and(|e| e.checking)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub fn build_count(&self) -> usize {
        self.builds.load(Ordering::Relaxed)
    }

    async fn refresh(self: Arc<Self>, root: PathBuf) {
        let result = self.refresh_inner(&root).await;
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get_mut(&root) {
            if let Err(code) = result {
                entry.view.status = if entry.view.index.is_some() && code != "parser_unavailable" {
                    "stale"
                } else {
                    "unavailable"
                };
                entry.view.error_code = Some(code.into());
            }
            entry.checked = Some(Instant::now());
            entry.checking = false;
        }
        while entries.values().map(|entry| entry.view.memory_bytes()).sum::<usize>() > CACHE_BYTES {
            let victim = entries
                .iter()
                .filter(|(path, entry)| **path != root && !entry.checking)
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(path, _)| path.clone());
            if let Some(victim) = victim {
                entries.remove(&victim);
            } else {
                break;
            }
        }
    }

    async fn refresh_inner(&self, root: &Path) -> Result<(), &'static str> {
        // Validate existing components even when the parser is absent; never follow .winx aliases.
        let dir = storage(root, false)?;
        let file = dir.join("index.wcg");
        let disk_valid = self.load_cached(root, &file).await?;
        if !cfg!(target_os = "linux") {
            return Err("index_platform_unsupported");
        }
        let source_key = snapshot::fingerprint(root).await?;
        let parser = self.parser.as_ref().ok_or("parser_unavailable")?;
        let parser = parser.canonicalize().map_err(|_| "parser_unavailable")?;
        if parser.starts_with(root) || !parser.is_file() {
            return Err("parser_untrusted_path");
        }
        let key = parser_key(&source_key, &parser)?;
        let workspace = root.to_owned();
        let allowed = tokio::task::spawn_blocking(move || {
            crate::utils::repo::walk_workspace_files(&workspace).into_iter().collect()
        })
        .await
        .map_err(|_| "snapshot_failed")?;
        if let Some(entry) = self.entries.lock().await.get_mut(root) {
            entry.view.allowed = Arc::new(allowed);
            if entry.view.memory_bytes() > CACHE_BYTES {
                entry.view.allowed = Arc::default();
                return Err("index_memory_limit");
            }
        }

        {
            let mut entries = self.entries.lock().await;
            let entry = entries.get_mut(root).ok_or("index_busy")?;
            if disk_valid && entry.view.index.as_ref().is_some_and(|index| index.snapshot == key) {
                entry.view.status = "fresh";
                entry.view.error_code = None;
                return Ok(());
            }
            entry.view.status = if entry.view.index.is_some() { "stale" } else { "building" };
            if entry.attempted.is_some_and(|time| time.elapsed() < self.debounce) {
                return Ok(());
            }
            entry.attempted = Some(Instant::now());
        }
        let _slot = BUILD_SLOT.acquire().await.map_err(|_| "index_busy")?;
        let build_directory = storage(root, true)?;
        #[cfg(target_os = "linux")]
        if !crate::os::unix::try_lock_exclusive(&build_directory.pin)
            .map_err(|_| "index_storage_unsafe")?
        {
            return Err("index_busy");
        }
        {
            let mut entries = self.entries.lock().await;
            let entry = entries.get_mut(root).ok_or("index_busy")?;
            entry.view.status = "building";
        }
        let (mut graph, version) = self.build(root, &build_directory, &parser).await?;
        if snapshot::fingerprint(root).await? != source_key {
            return Err("snapshot_changed");
        }
        graph.snapshot = key;
        graph.parser_version = version;
        graph.built_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let inventory_bytes = self.entries.lock().await.get(root).map_or(0, |entry| {
            entry
                .view
                .memory_bytes()
                .saturating_sub(entry.view.index.as_ref().map_or(0, |index| index.memory_bytes()))
        });
        if graph.memory_bytes().saturating_add(inventory_bytes) > CACHE_BYTES {
            return Err("index_memory_limit");
        }
        let graph = Arc::new(graph);
        let saving = graph.clone();
        let destination = storage(root, true)?;
        tokio::task::spawn_blocking(move || {
            let bytes = saving.encode().map_err(|_| "index_oversize")?;
            let mut temp = tempfile::NamedTempFile::new_in(&destination)
                .map_err(|_| "index_storage_unsafe")?;
            temp.write_all(&bytes).map_err(|_| "index_write_failed")?;
            temp.as_file().sync_all().map_err(|_| "index_write_failed")?;
            temp.persist(destination.join("index.wcg")).map_err(|_| "index_write_failed")?;
            Ok::<_, &'static str>(())
        })
        .await
        .map_err(|_| "index_write_failed")??;
        let mut entries = self.entries.lock().await;
        let entry = entries.get_mut(root).ok_or("index_busy")?;
        entry.view = View {
            index: Some(graph),
            allowed: entry.view.allowed.clone(),
            status: "fresh",
            error_code: None,
        };
        entry.stamp = stamp(&file)?;
        Ok(())
    }
    async fn load_cached(&self, root: &Path, file: &Path) -> Result<bool, &'static str> {
        let current_stamp = stamp(file)?;
        let reload = {
            let entries = self.entries.lock().await;
            let entry = entries.get(root).ok_or("index_busy")?;
            entry.stamp != current_stamp || entry.view.index.is_none()
        };
        if reload && current_stamp.is_some() {
            let path = file.to_owned();
            let result = tokio::task::spawn_blocking(move || load(&path))
                .await
                .map_err(|_| "index_corrupt")?;
            let graph = match result {
                Ok(graph) => graph,
                Err(_) if self.parser.is_some() => return Ok(false),
                Err(code) => return Err(code),
            };
            if graph.memory_bytes() > CACHE_BYTES {
                return Err("index_memory_limit");
            }
            let mut entries = self.entries.lock().await;
            let entry = entries.get_mut(root).ok_or("index_busy")?;
            entry.view.index = Some(Arc::new(graph));
            entry.stamp = current_stamp;
        }
        Ok(current_stamp.is_some())
    }

    async fn build(
        &self,
        root: &Path,
        dir: &Path,
        parser: &Path,
    ) -> Result<(GraphIndex, String), &'static str> {
        let version = version(parser).await?;
        let staging = tempfile::Builder::new()
            .prefix("build-")
            .tempdir_in(dir)
            .map_err(|_| "index_storage_unsafe")?;
        let output = staging.path().join("kb.json");
        // The parser disables gitignore. At minimum its own outputs and Winx artifacts must be excluded.
        let ignore = staging.path().join("parser.euignore");
        std::fs::write(&ignore, "**/.winx/\n**/.git/\n").map_err(|_| "index_storage_unsafe")?;
        let mut command = snapshot::command(parser);
        command
            .arg("--root")
            .arg(root)
            .arg("--output")
            .arg(&output)
            .args(["--threads", "4", "--prism", "2", "--euignore"])
            .arg(ignore)
            .env("RAYON_NUM_THREADS", "4")
            .current_dir(staging.path())
            .stdout(std::process::Stdio::null());
        #[cfg(target_os = "linux")]
        crate::os::bound_worker_cpus(command.as_std_mut(), 4)
            .map_err(|_| "parser_cpu_limit_failed")?;
        let mut child = command.spawn().map_err(|_| "parser_unavailable")?;
        self.builds.fetch_add(1, Ordering::Relaxed);
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            tokio::select! {
                result = child.wait() => { if !result.map_err(|_| "parser_failed")?.success() { return Err("parser_failed"); } break; }
                () = tokio::time::sleep_until(deadline) => { snapshot::terminate(&mut child).await; return Err("parser_timeout"); }
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    if directory_bytes(staging.path())? > MAX_KB_BYTES * 2 { snapshot::terminate(&mut child).await; return Err("index_oversize"); }
                }
            }
        }
        if directory_bytes(staging.path())? > MAX_KB_BYTES * 2 {
            return Err("index_oversize");
        }
        let sidecar = staging.path().join("kb_call_graph.json");
        let graph = tokio::task::spawn_blocking(move || {
            let kb = bounded_file(&output, MAX_KB_BYTES)?;
            if sidecar.exists() {
                GraphIndex::from_readers(kb, bounded_file(&sidecar, MAX_KB_BYTES)?)
                    .map_err(|_| "index_corrupt")
            } else {
                GraphIndex::from_reader(kb).map_err(|_| "index_corrupt")
            }
        })
        .await
        .map_err(|_| "index_corrupt")??;

        Ok((graph, version))
    }
}

struct Storage {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    pin: std::fs::File,
}
impl std::ops::Deref for Storage {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}
impl AsRef<Path> for Storage {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "linux")]
fn storage(root: &Path, create: bool) -> Result<Storage, &'static str> {
    let mut pin = crate::os::temporary::directory(root).map_err(|_| "index_storage_unsafe")?;
    for part in [".winx", "index", "eulix"] {
        let child = if create {
            crate::os::temporary::ensure_directory_child(&pin, std::ffi::OsStr::new(part))
        } else {
            crate::os::temporary::directory_child(&pin, std::ffi::OsStr::new(part))
        };
        match child {
            Ok(child) => pin = child,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Storage { path: root.join(".winx/index/eulix"), pin });
            }
            Err(_) => return Err("index_storage_unsafe"),
        }
    }
    Ok(Storage { path: crate::os::temporary::directory_alias(&pin), pin })
}

#[cfg(not(target_os = "linux"))]
fn storage(root: &Path, create: bool) -> Result<Storage, &'static str> {
    if create {
        return Err("index_platform_unsupported");
    }
    let mut directory = root.to_owned();
    for part in [".winx", "index", "eulix"] {
        directory.push(part);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("index_storage_unsafe"),
        }
    }
    Ok(Storage { path: directory })
}

fn stamp(path: &Path) -> Result<Option<(u64, SystemTime)>, &'static str> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Ok(Some((metadata.len(), metadata.modified().map_err(|_| "index_corrupt")?)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err("index_storage_unsafe"),
    }
}
fn bounded_file(path: &Path, max: u64) -> Result<std::fs::File, &'static str> {
    let file = crate::os::open_read_nofollow(path).map_err(|_| "index_storage_unsafe")?;
    if file.metadata().map_err(|_| "index_corrupt")?.len() > max {
        return Err("index_oversize");
    }
    Ok(file)
}
fn load(path: &Path) -> Result<GraphIndex, &'static str> {
    let file = bounded_file(path, u64::try_from(MAX_INDEX_BYTES).unwrap_or(u64::MAX))?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_INDEX_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|_| "index_corrupt")?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err("index_oversize");
    }
    GraphIndex::decode(&bytes).map_err(|_| "index_corrupt")
}
fn directory_bytes(path: &Path) -> Result<u64, &'static str> {
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path).map_err(|_| "index_storage_unsafe")? {
        let metadata = entry
            .map_err(|_| "index_storage_unsafe")?
            .metadata()
            .map_err(|_| "index_storage_unsafe")?;
        bytes = bytes.saturating_add(metadata.len());
    }
    Ok(bytes)
}
async fn version(parser: &Path) -> Result<String, &'static str> {
    let mut child = snapshot::command(parser)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| "parser_unavailable")?;
    let stdout = child.stdout.take().ok_or("parser_version_failed")?;
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        stdout.take(513).read_to_end(&mut bytes).await.map_err(|_| "parser_version_failed")?;
        if bytes.len() > 512 || !child.wait().await.map_err(|_| "parser_version_failed")?.success()
        {
            return Err("parser_version_failed");
        }
        let text = String::from_utf8(bytes).map_err(|_| "parser_version_failed")?;
        if !text.starts_with("eulix_parser ") {
            return Err("parser_version_failed");
        }
        Ok(text.trim().to_owned())
    })
    .await;
    if let Ok(result) = result {
        result
    } else {
        snapshot::terminate(&mut child).await;
        Err("parser_version_timeout")
    }
}

pub(super) fn global() -> &'static Arc<Service> {
    static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
    SERVICE
        .get_or_init(|| Service::new(discover(), Duration::from_secs(120), Duration::from_secs(2)))
}
fn discover() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WINX_EULIX_PARSER") {
        let path = PathBuf::from(path);
        return path.is_absolute().then_some(path);
    }
    let name = if cfg!(windows) { "eulix_parser.exe" } else { "eulix_parser" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|path| path.is_absolute())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

#[cfg(all(test, target_os = "linux"))]
mod storage_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn directory_handle_pins_writes_across_symlink_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("workspace");
        let outside = temp.path().canonicalize().unwrap().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let directory = storage(&root, true).unwrap();
        std::fs::rename(root.join(".winx"), root.join("retired")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".winx")).unwrap();
        std::fs::write(directory.join("probe"), "fixture").unwrap();
        assert!(!outside.join("index/eulix/probe").exists());
        assert!(root.join("retired/index/eulix/probe").exists());
    }
}

fn parser_key(source_key: &str, parser: &Path) -> Result<String, &'static str> {
    use std::hash::{Hash as _, Hasher as _};
    let mut path_hash = std::collections::hash_map::DefaultHasher::new();
    parser.hash(&mut path_hash);
    let identity = std::fs::metadata(parser).map_err(|_| "parser_unavailable")?;
    Ok(format!(
        "{source_key}:{:x}:{:x}:{:x}",
        path_hash.finish(),
        identity.len(),
        identity
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default()
            .as_nanos()
    ))
}
