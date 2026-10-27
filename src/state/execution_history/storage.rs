use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ExecutionCatalog, ExecutionProcessStatus, ExecutionRecord, ExecutionScope, HistoryError,
    HistoryLimits, HistoryResult,
};

const LOCK_WAIT_LIMIT: Duration = Duration::from_millis(250);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(5);

const MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct HistoryIndex {
    pub version: u8,
    pub scope_key: String,
    pub epoch: String,
    pub sequence: u64,
    pub floor_sequence: u64,
    pub executions: Vec<ExecutionRecord>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(super) struct StoredPayload {
    pub output: String,
    pub output_start: u64,
    pub output_epoch: u64,
    pub runtime_snapshot: bool,
    pub output_activity_seq: Option<u64>,
    pub screen: String,
}

pub(super) struct HistoryTransaction {
    pub index: HistoryIndex,
    pub limits: HistoryLimits,
    pub now: u64,
    pub dirty: bool,
    directory: PathBuf,
    obsolete: Vec<PathBuf>,
    // Dropping the descriptor releases the cross-process file lock, including on errors.
    _lock: File,
}

pub(super) fn transaction<T>(
    catalog: &ExecutionCatalog,
    scope: &ExecutionScope,
    now: u64,
    operation: impl FnOnce(&mut HistoryTransaction) -> HistoryResult<T>,
) -> HistoryResult<T> {
    if scope.thread_id.is_empty()
        || scope.thread_id.len() > 4096
        || scope.thread_id.contains('\0')
        || !scope.workspace_root.is_absolute()
    {
        return Err(HistoryError::InvalidInput(
            "a bound thread and absolute workspace are required".into(),
        ));
    }
    if catalog.limits.max_records == 0
        || catalog.limits.max_records > 128
        || catalog.limits.max_output_bytes == 0
        || catalog.limits.max_output_bytes > 1024 * 1024
        || catalog.limits.max_session_output_bytes == 0
        || catalog.limits.max_session_output_bytes > 16 * 1024 * 1024
    {
        return Err(HistoryError::InvalidInput("history limits exceed supported bounds".into()));
    }
    let scope_key = scope_key(scope);
    private_directory(&catalog.root)?;
    let directory = catalog.root.join(&scope_key);
    private_directory(&directory)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join("history.lock"))?;
    acquire_lock(&lock)?;
    let index_path = directory.join("index.json");
    let (index, dirty) = if index_path.exists() {
        let index: HistoryIndex = read_json(&index_path, MAX_INDEX_BYTES)?;
        if index.version != 1 || index.scope_key != scope_key || index.executions.len() > 128 {
            return Err(HistoryError::InvalidInput(
                "unsupported or mismatched history index".into(),
            ));
        }
        (index, false)
    } else {
        (
            HistoryIndex {
                version: 1,
                scope_key,
                epoch: format!("{:032x}", rand::random::<u128>()),
                sequence: 0,
                floor_sequence: 0,
                executions: Vec::new(),
            },
            true,
        )
    };
    let mut transaction = HistoryTransaction {
        index,
        limits: catalog.limits.clone(),
        now,
        dirty,
        directory,
        obsolete: Vec::new(),
        _lock: lock,
    };
    transaction.prune_expired();
    let value = operation(&mut transaction)?;
    transaction.commit()?;
    Ok(value)
}

impl HistoryTransaction {
    pub fn find(&self, id: &str) -> Option<&ExecutionRecord> {
        self.index.executions.iter().find(|record| record.execution_id == id)
    }

    pub fn next_sequence(&mut self) -> u64 {
        self.index.sequence = self.index.sequence.saturating_add(1);
        self.dirty = true;
        self.index.sequence
    }

    pub fn payload(&self, record: &ExecutionRecord) -> HistoryResult<StoredPayload> {
        if record.output_revision == 0 {
            return Ok(StoredPayload::default());
        }
        let payload: StoredPayload = read_json(&self.payload_path(record), MAX_PAYLOAD_BYTES)?;
        if payload.output.len() > self.limits.max_output_bytes
            || payload.screen.len() > 64 * 1024
            || payload.output.len() != record.retained_bytes
            || payload.screen.len() != record.screen_retained_bytes
            || payload.output_start != record.retained_start
            || payload.output_start.saturating_add(payload.output.len() as u64)
                != record.retained_end
        {
            return Err(HistoryError::InvalidInput(
                "execution output revision is inconsistent".into(),
            ));
        }
        Ok(payload)
    }

    pub fn store_payload(
        &mut self,
        record: &mut ExecutionRecord,
        payload: &StoredPayload,
    ) -> HistoryResult<()> {
        if record.output_revision != 0 {
            self.obsolete.push(self.payload_path(record));
        }
        record.output_revision = self.index.sequence;
        record.retained_start = payload.output_start;
        record.retained_bytes = payload.output.len();
        record.screen_retained_bytes = payload.screen.len();
        record.retained_end = payload.output_start.saturating_add(payload.output.len() as u64);
        atomic_json(&self.payload_path(record), payload)?;
        self.dirty = true;
        Ok(())
    }

    pub fn put(&mut self, record: ExecutionRecord) {
        if let Some(index) =
            self.index.executions.iter().position(|entry| entry.execution_id == record.execution_id)
        {
            self.index.executions[index] = record;
        } else {
            self.index.executions.push(record);
        }
        self.dirty = true;
    }

    pub fn make_room(&mut self) -> HistoryResult<()> {
        while self.index.executions.len() >= self.limits.max_records {
            let victim = self
                .index
                .executions
                .iter()
                .enumerate()
                .filter(|(_, record)| record.process_status != ExecutionProcessStatus::Running)
                .min_by_key(|(_, record)| {
                    record.finished_at_unix_ms.unwrap_or(record.observed_at_unix_ms)
                })
                .map(|(index, _)| index);
            let Some(victim) = victim else {
                return Err(HistoryError::Capacity);
            };
            self.remove(victim);
        }
        Ok(())
    }

    pub fn enforce_output_budget(&mut self) -> HistoryResult<()> {
        let mut total: usize = self
            .index
            .executions
            .iter()
            .map(|record| record.retained_bytes + record.screen_retained_bytes)
            .sum();
        while total > self.limits.max_session_output_bytes {
            let victim = self
                .index
                .executions
                .iter()
                .enumerate()
                .filter(|(_, record)| record.retained_bytes > 0 || record.screen_retained_bytes > 0)
                .min_by_key(|(_, record)| {
                    (
                        record.process_status == ExecutionProcessStatus::Running,
                        record.observed_at_unix_ms,
                    )
                })
                .map(|(index, _)| index);
            let Some(victim) = victim else {
                break;
            };
            let mut record = self.index.executions[victim].clone();
            let mut payload = self.payload(&record)?;
            let reduction = (total - self.limits.max_session_output_bytes)
                .min(payload.output.len() + payload.screen.len());
            let keep = payload.output.len().saturating_sub(reduction);
            let removed = retain_tail(&mut payload.output, keep);
            payload.output_start = payload.output_start.saturating_add(removed as u64);
            let screen_keep =
                payload.screen.len().saturating_sub(reduction.saturating_sub(removed));
            let screen_removed = retain_tail(&mut payload.screen, screen_keep);
            record.sequence = self.next_sequence();
            record.output_sequence = record.sequence;
            record.output_truncated |= removed > 0;
            record.screen_truncated |= screen_removed > 0;
            self.store_payload(&mut record, &payload)?;
            self.index.executions[victim] = record;
            total = total.saturating_sub(removed + screen_removed);
        }
        Ok(())
    }

    fn payload_path(&self, record: &ExecutionRecord) -> PathBuf {
        self.directory.join(format!(
            "output_{}_{}.json",
            digest(record.execution_id.as_bytes()),
            record.output_revision
        ))
    }

    fn prune_expired(&mut self) {
        let expired = self
            .index
            .executions
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                record.process_status != ExecutionProcessStatus::Running
                    && record.expires_at_unix_ms.is_some_and(|deadline| self.now >= deadline)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in expired.into_iter().rev() {
            self.remove(index);
        }
    }

    fn remove(&mut self, index: usize) {
        let record = self.index.executions.remove(index);
        if record.output_revision != 0 {
            self.obsolete.push(self.payload_path(&record));
        }
        self.index.floor_sequence = self.next_sequence();
    }

    fn commit(&mut self) -> HistoryResult<()> {
        if self.dirty {
            atomic_json(&self.directory.join("index.json"), &self.index)?;
            for path in self.obsolete.drain(..) {
                let _ = fs::remove_file(path);
            }
            // A crash before committing the index can leave an unreferenced immutable blob.
            let live = self
                .index
                .executions
                .iter()
                .filter(|record| record.output_revision != 0)
                .map(|record| self.payload_path(record))
                .collect::<std::collections::HashSet<_>>();
            for entry in fs::read_dir(&self.directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("output_")
                    && name.ends_with(".json")
                    && !live.contains(&entry.path())
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }
}

fn acquire_lock(file: &File) -> HistoryResult<()> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = LOCK_WAIT_LIMIT.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "execution history lock acquisition exceeded its bounded deadline",
                    )
                    .into());
                }
                std::thread::sleep(remaining.min(LOCK_RETRY_INTERVAL));
            }
        }
    }
}

fn private_directory(path: &Path) -> HistoryResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HistoryError::InvalidInput(
            "history storage must be a private directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, cap: u64) -> HistoryResult<T> {
    let file = File::open(path)?;
    if file.metadata()?.len() > cap {
        return Err(HistoryError::InvalidInput("history file exceeds its size limit".into()));
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(HistoryError::InvalidInput("history file exceeds its size limit".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn atomic_json(path: &Path, value: &impl Serialize) -> HistoryResult<()> {
    let parent =
        path.parent().ok_or_else(|| HistoryError::InvalidInput("missing history parent".into()))?;
    let mut temporary = tempfile::Builder::new().prefix(".history-").tempfile_in(parent)?;
    serde_json::to_writer(&mut temporary, value)?;
    temporary.flush()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn retain_tail(text: &mut String, limit: usize) -> usize {
    let mut remove = text.len().saturating_sub(limit);
    while !text.is_char_boundary(remove) {
        remove += 1;
    }
    if remove != 0 {
        text.drain(..remove);
    }
    remove
}

pub(super) fn truncate(text: &mut String, limit: usize) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

pub(super) fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn scope_key(scope: &ExecutionScope) -> String {
    use std::fmt::Write as _;
    let mut hash = Sha256::new();
    hash.update((scope.thread_id.len() as u64).to_le_bytes());
    hash.update(scope.thread_id.as_bytes());
    hash.update(scope.workspace_root.as_os_str().as_encoded_bytes());
    hash.finalize().iter().fold(String::with_capacity(64), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}
