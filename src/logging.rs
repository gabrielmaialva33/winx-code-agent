use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use time::OffsetDateTime;
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer as _;

use crate::errors::{Result, WinxError};

/// Keeps the non-blocking usage-log worker alive for the lifetime of the process.
pub struct LoggingGuard {
    _usage: Option<WorkerGuard>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsageRotation {
    Never,
    Hourly,
    Daily,
}

impl UsageRotation {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("daily").trim().to_ascii_lowercase().as_str() {
            "never" => Ok(Self::Never),
            "hourly" => Ok(Self::Hourly),
            "daily" | "" => Ok(Self::Daily),
            other => Err(WinxError::ConfigurationError(format!(
                "invalid WINX_USAGE_LOG_ROTATION={other:?}; expected `never`, `hourly`, or `daily`"
            ))),
        }
    }

    fn period(self, now: OffsetDateTime) -> Option<String> {
        let month = u8::from(now.month());
        match self {
            Self::Never => None,
            Self::Daily => Some(format!("{:04}-{month:02}-{:02}", now.year(), now.day())),
            Self::Hourly => {
                Some(format!("{:04}-{month:02}-{:02}-{:02}", now.year(), now.day(), now.hour()))
            }
        }
    }

    fn max_files(self, keep_days: usize) -> Option<usize> {
        if keep_days == 0 || self == Self::Never {
            return None;
        }
        let files_per_day = if self == Self::Hourly { 24 } else { 1 };
        // Retain one extra file so rotation never dips below the requested
        // completed-history window while the active file is counted.
        Some(keep_days.saturating_mul(files_per_day).saturating_add(1))
    }

    fn valid_period(self, suffix: &str) -> bool {
        let expected_len = match self {
            Self::Never => return false,
            Self::Daily => 10,
            Self::Hourly => 13,
        };
        suffix.len() == expected_len
            && suffix.bytes().enumerate().all(|(index, byte)| {
                let dash = index == 4 || index == 7 || (self == Self::Hourly && index == 10);
                if dash {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
    }
}

/// Minimal secure rolling writer used by the non-blocking tracing worker.
/// Every file is opened with `O_NOFOLLOW` and mode `0600` on Unix, including
/// files created after daily/hourly rotation. This avoids relying on the process
/// umask and closes the short world-readable window a post-open chmod would have.
struct SecureUsageWriter {
    base_path: PathBuf,
    rotation: UsageRotation,
    keep_days: usize,
    active_period: Option<String>,
    active_path: PathBuf,
    file: File,
}

impl SecureUsageWriter {
    fn new(base_path: PathBuf, rotation: UsageRotation, keep_days: usize) -> io::Result<Self> {
        let parent = log_parent(&base_path);
        create_log_directory(parent)?;
        validate_log_file_name(&base_path)?;

        let active_period = rotation.period(OffsetDateTime::now_utc());
        let active_path = rotated_path(&base_path, active_period.as_deref())?;
        let file = open_secure_log_file(&active_path)?;
        let writer = Self { base_path, rotation, keep_days, active_period, active_path, file };
        writer.prune_old_logs()?;
        Ok(writer)
    }

    fn rotate_if_needed(&mut self) -> io::Result<()> {
        let period = self.rotation.period(OffsetDateTime::now_utc());
        if period == self.active_period {
            return Ok(());
        }

        let path = rotated_path(&self.base_path, period.as_deref())?;
        let file = open_secure_log_file(&path)?;
        self.file = file;
        self.active_path = path;
        self.active_period = period;
        self.prune_old_logs()
    }

    fn prune_old_logs(&self) -> io::Result<()> {
        let Some(max_files) = self.rotation.max_files(self.keep_days) else {
            return Ok(());
        };
        let parent = log_parent(&self.base_path);
        let base_name = validate_log_file_name(&self.base_path)?;
        let prefix = format!("{base_name}.");
        let mut candidates = Vec::new();

        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToString::to_string) else {
                continue;
            };
            let Some(period) = name.strip_prefix(&prefix) else {
                continue;
            };
            if self.rotation.valid_period(period) {
                candidates.push((name, entry.path()));
            }
        }

        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        let remove_count = candidates.len().saturating_sub(max_files);
        for (_, path) in candidates.into_iter().take(remove_count) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Write for SecureUsageWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.rotate_if_needed()?;
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Install the stderr subscriber plus an optional JSONL usage-event sink.
///
/// `WINX_USAGE_LOG` enables a non-blocking file writer filtered to the
/// `winx::usage` target. The sink never receives command text or file contents.
/// Call this only after the optional Landlock domain has been applied so the
/// writer thread inherits the same filesystem restrictions as every other child.
pub fn initialize(verbose: bool, debug: bool) -> Result<LoggingGuard> {
    let level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };
    let filter = tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into());
    let filter = match "winx::usage=info".parse() {
        Ok(directive) => filter.add_directive(directive),
        Err(_) => filter,
    };
    let stderr_json = crate::config::env_text("WINX_LOG_FORMAT")
        .is_some_and(|format| format.eq_ignore_ascii_case("json"));
    let usage = usage_log_writer()?;

    let guard = match (stderr_json, usage) {
        (true, Some((writer, guard))) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_ansi(false)
                        .with_writer(std::io::stderr)
                        .with_filter(filter),
                )
                .with(usage_json_layer(writer))
                .init();
            Some(guard)
        }
        (false, Some((writer, guard))) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(true)
                        .with_writer(std::io::stderr)
                        .with_filter(filter),
                )
                .with(usage_json_layer(writer))
                .init();
            Some(guard)
        }
        (true, None) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_ansi(false)
                        .with_writer(std::io::stderr)
                        .with_filter(filter),
                )
                .init();
            None
        }
        (false, None) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(true)
                        .with_writer(std::io::stderr)
                        .with_filter(filter),
                )
                .init();
            None
        }
    };
    Ok(LoggingGuard { _usage: guard })
}

fn usage_json_layer<S>(writer: NonBlocking) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_current_span(false)
        .with_span_list(false)
        .with_writer(writer)
        .with_filter(filter_fn(|metadata| metadata.target() == "winx::usage"))
}

fn usage_log_writer() -> Result<Option<(NonBlocking, WorkerGuard)>> {
    let Some(path) = crate::config::env_text("WINX_USAGE_LOG").map(PathBuf::from) else {
        return Ok(None);
    };
    let rotation =
        UsageRotation::parse(crate::config::env_text("WINX_USAGE_LOG_ROTATION").as_deref())?;
    let keep_days = parse_keep_days()?;
    let writer = SecureUsageWriter::new(path.clone(), rotation, keep_days).map_err(|error| {
        WinxError::ConfigurationError(format!(
            "cannot open WINX_USAGE_LOG {} securely: {error}",
            path.display()
        ))
    })?;
    Ok(Some(NonBlockingBuilder::default().thread_name("winx-usage-log").finish(writer)))
}

fn log_parent(path: &Path) -> &Path {
    path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn validate_log_file_name(path: &Path) -> io::Result<&str> {
    path.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("usage log path must name a UTF-8 file: {}", path.display()),
        )
    })
}

fn rotated_path(base_path: &Path, period: Option<&str>) -> io::Result<PathBuf> {
    let Some(period) = period else {
        return Ok(base_path.to_path_buf());
    };
    let name = validate_log_file_name(base_path)?;
    Ok(base_path.with_file_name(format!("{name}.{period}")))
}

fn create_log_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

fn open_secure_log_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("usage log is not a regular file: {}", path.display()),
            ));
        }
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let file = OpenOptions::new().append(true).create(true).open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("usage log is not a regular file: {}", path.display()),
            ));
        }
        Ok(file)
    }
}

fn parse_keep_days() -> Result<usize> {
    let Some(raw) = crate::config::env_text("WINX_USAGE_LOG_KEEP_DAYS") else {
        return Ok(7);
    };
    let days = raw.parse::<usize>().map_err(|error| {
        WinxError::ConfigurationError(format!("invalid WINX_USAGE_LOG_KEEP_DAYS={raw:?}: {error}"))
    })?;
    if days > 3_650 {
        return Err(WinxError::ConfigurationError(
            "WINX_USAGE_LOG_KEEP_DAYS must be between 0 and 3650".to_string(),
        ));
    }
    Ok(days)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{SecureUsageWriter, UsageRotation};
    use std::io::Write as _;

    #[test]
    fn usage_rotation_retention_is_bounded_and_explicit() {
        assert!(matches!(UsageRotation::parse(None), Ok(UsageRotation::Daily)));
        assert_eq!(UsageRotation::Daily.max_files(7), Some(8));
        assert_eq!(UsageRotation::Hourly.max_files(2), Some(49));
        assert_eq!(UsageRotation::Never.max_files(7), None);
        assert_eq!(UsageRotation::Daily.max_files(0), None);
    }

    #[cfg(unix)]
    #[test]
    fn usage_files_are_private_for_initial_and_rotated_names() {
        use std::os::unix::fs::PermissionsExt as _;

        for rotation in [UsageRotation::Never, UsageRotation::Daily, UsageRotation::Hourly] {
            let root = tempfile::tempdir().unwrap();
            let nested = root.path().join("private/state");
            let base = nested.join("usage.jsonl");
            let mut writer = SecureUsageWriter::new(base, rotation, 1).unwrap();
            writer.write_all(b"{}\n").unwrap();
            writer.flush().unwrap();

            let mode = writer.active_path.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", writer.active_path.display());
            let directory_mode = nested.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(directory_mode, 0o700, "{}", nested.display());

            if rotation != UsageRotation::Never {
                let rotated_path = writer.active_path.clone();
                std::fs::remove_file(&rotated_path).unwrap();
                writer.active_period = Some("1970-01-01-00".to_string());
                writer.write_all(b"rotated\n").unwrap();
                writer.flush().unwrap();
                let rotated_mode = rotated_path.metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(rotated_mode, 0o600, "{}", rotated_path.display());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_usage_file_is_tightened_and_symlink_is_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("existing.jsonl");
        std::fs::write(&existing, b"old\n").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
        let writer = SecureUsageWriter::new(existing.clone(), UsageRotation::Never, 0).unwrap();
        let mode = writer.active_path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(writer);

        let target = root.path().join("target.jsonl");
        std::fs::write(&target, b"target\n").unwrap();
        let link = root.path().join("link.jsonl");
        symlink(&target, &link).unwrap();
        assert!(SecureUsageWriter::new(link, UsageRotation::Never, 0).is_err());
    }
}
