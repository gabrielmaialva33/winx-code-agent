//! Implementation of the `ReadFiles` tool.
//!
//! Reads one or many files with line numbers and optional line ranges. Every
//! call is bounded by a response budget ([`ReadResponseBudget`]): an item over
//! budget returns its head at a line boundary plus the exact next range to
//! request, and an unchanged, still-guarded repeat in the same thread returns a
//! compact marker instead of resending identical content.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::Mutex;
use tracing::{debug, instrument};

use crate::config::{ReadResponseBudget, MIN_READ_BUDGET_BYTES};
use crate::errors::{ErrorRecovery, Result, WinxError};
use crate::state::bash_state::{BashState, ReadDelivery};
use crate::types::ReadFiles;
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{expand_user, validate_path_in_workspace};

/// Default token limits for file reading
const CODING_MAX_TOKENS: usize = 24_000;
const NONCODING_MAX_TOKENS: usize = 8_000;
const DEFAULT_READ_PARALLELISM: usize = 4;
const MAX_READ_PARALLELISM: usize = 32;
/// Repeats smaller than this are resent: the marker would save too little to
/// justify a possible `force=true` round trip.
const MIN_UNCHANGED_ELISION_BYTES: usize = 512;
/// Above this multiple of the chunk budget, the continuation also points at
/// `CodeMap`, which navigates a large file far more cheaply than paging.
const OUTLINE_HINT_CHUNKS: usize = 4;

type ReadCoverage = (Vec<(usize, usize)>, String, usize);

/// Byte limits one worker applies while rendering. `render` bounds the text it
/// formats and tokenizes; `horizon` bounds the line sizes it measures so the
/// ordered phase can size the next chunk without rendering it.
#[derive(Clone, Copy, Debug)]
struct WorkerCaps {
    render: Option<usize>,
    horizon: usize,
}

impl WorkerCaps {
    const UNBOUNDED: Self = Self { render: None, horizon: 0 };
}

#[derive(Debug)]
struct FileReadResult {
    /// Complete numbered lines starting at `first_line`.
    content: String,
    /// Cumulative rendered byte end of each requested line from `first_line`,
    /// measured up to the worker horizon (it may run past `content`).
    line_ends: Vec<usize>,
    first_line: usize,
    /// Last requested line, clamped to the file.
    last_requested_line: usize,
    /// Rendered size of the whole requested range, as if nothing were cut.
    requested_bytes: usize,
    /// `(tokens, limit)` when the per-file token budget cut `content`.
    token_limit: Option<(usize, usize)>,
    canonical_path: String,
    file_hash: String,
    total_lines: usize,
}

/// Revision receipt for one successful file read. `revision` is opaque to MCP
/// callers and may be supplied to `ApplyPatch` as a compare-and-swap guard.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileRevision {
    pub path: String,
    pub revision: String,
    pub total_lines: usize,
    pub visible_ranges: Vec<ReadLineRange>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_start_line: Option<usize>,
    /// Inclusive end of a continuation chunk that fits the per-file budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_end_line: Option<usize>,
    /// Content was elided because this thread already holds it unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unchanged: bool,
    #[serde(rename = "temporary_artifact", skip_serializing_if = "Option::is_none")]
    pub temporary_artifact: Option<crate::utils::agent_temp::release::TemporaryArtifactMetadata>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadLineRange {
    pub start_line: usize,
    pub end_line: usize,
}

/// Budget accounting for one call. Byte counts are rendered (numbered) text.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadBudgetReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_bytes: Option<usize>,
    pub returned_bytes: usize,
    pub truncated_files: usize,
    pub unchanged_files: usize,
    /// Requested bytes withheld by truncation.
    pub omitted_bytes: usize,
    /// Bytes not resent because the content was unchanged.
    pub unchanged_bytes: usize,
    /// Exact `file_paths` entries not read because the call budget ran out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<String>,
}

impl ReadBudgetReport {
    /// Whether the budget changed this response; otherwise it is not reported.
    pub fn is_notable(&self) -> bool {
        self.truncated_files > 0 || self.unchanged_files > 0 || !self.deferred.is_empty()
    }
}

#[derive(Clone)]
struct FileReadRequest {
    index: usize,
    requested_path: String,
    clean_path: String,
    start_line_num: Option<usize>,
    end_line_num: Option<usize>,
}

impl FileReadRequest {
    /// Explicit `start-end` (or `-end`) ranges are deliberate and exempt from
    /// the per-file cap; whole-file and open-ended reads are not.
    fn is_bounded(&self) -> bool {
        self.end_line_num.is_some()
    }

    fn spec(&self) -> String {
        format!("{}{}", self.clean_path, range_format(self.start_line_num, self.end_line_num))
    }
}

/// A complete requested range to remember for unchanged-repeat compaction.
struct DeliveredRange {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    file_hash: String,
}

/// Coverage that an elided repeat re-confirms. It is applied only while the
/// guard still holds the same hash, so it can refresh but never re-grant a
/// permit that a concurrent SEARCH conflict revoked.
struct RefreshedCoverage {
    path: String,
    range: (usize, usize),
    file_hash: String,
}

struct ReadBudgetState {
    budget: ReadResponseBudget,
    remaining: Option<usize>,
    unchanged_candidates: Vec<ReadDelivery>,
    report: ReadBudgetReport,
}

impl ReadBudgetState {
    fn new(budget: ReadResponseBudget, unchanged_candidates: Vec<ReadDelivery>) -> Self {
        Self {
            budget,
            remaining: budget.call_bytes,
            unchanged_candidates,
            report: ReadBudgetReport {
                call_bytes: budget.call_bytes,
                file_bytes: budget.file_bytes,
                ..ReadBudgetReport::default()
            },
        }
    }

    /// Size used to recommend a continuation chunk.
    fn chunk_bytes(&self) -> Option<usize> {
        self.budget.file_bytes.or(self.budget.call_bytes)
    }

    /// Worker caps for one request before any batch item has been consumed.
    fn worker_caps(&self, request: &FileReadRequest) -> WorkerCaps {
        let render = if request.is_bounded() {
            self.budget.call_bytes
        } else {
            min_option(self.budget.file_bytes, self.budget.call_bytes)
        };
        render.map_or(WorkerCaps::UNBOUNDED, |render| WorkerCaps {
            render: Some(render),
            horizon: render.saturating_add(self.chunk_bytes().unwrap_or(render)),
        })
    }

    fn unchanged_delivery(
        &self,
        request: &FileReadRequest,
        result: &FileReadResult,
    ) -> Option<SystemTime> {
        if result.requested_bytes < MIN_UNCHANGED_ELISION_BYTES {
            return None;
        }
        self.unchanged_candidates
            .iter()
            .find(|delivery| {
                delivery.path == result.canonical_path
                    && delivery.start_line == request.start_line_num
                    && delivery.end_line == request.end_line_num
                    && delivery.file_hash == result.file_hash
            })
            .map(|delivery| delivery.delivered_at)
    }
}

fn min_option(left: Option<usize>, right: Option<usize>) -> Option<usize> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (value, None) | (None, value) => value,
    }
}

struct ReadBatchState {
    message: String,
    file_ranges: HashMap<String, ReadCoverage>,
    refreshed: Vec<RefreshedCoverage>,
    delivered: Vec<DeliveredRange>,
    stats_paths: Vec<PathBuf>,
    successful_files: usize,
    revisions: Vec<ReadFileRevision>,
    errors: Vec<WinxError>,
    budget: ReadBudgetState,
}

impl ReadBatchState {
    fn new(budget: ReadBudgetState) -> Self {
        Self {
            message: String::new(),
            file_ranges: HashMap::new(),
            refreshed: Vec::new(),
            delivered: Vec::new(),
            stats_paths: Vec::new(),
            successful_files: 0,
            revisions: Vec::new(),
            errors: Vec::new(),
            budget,
        }
    }

    /// Add one worker result to the ordered response. Returns `true` when the
    /// call budget is exhausted and the remaining items must be deferred.
    fn record_result(&mut self, request: &FileReadRequest, result: Result<FileReadResult>) -> bool {
        match result {
            Ok(result) => {
                if let Some(delivered_at) = self.budget.unchanged_delivery(request, &result) {
                    self.record_unchanged(request, &result, delivered_at);
                    return false;
                }
                self.record_content(request, result)
            }
            Err(error) => {
                let _ = write!(self.message, "\nError reading {}: {error}", request.requested_path);
                self.errors.push(error);
                false
            }
        }
    }

    fn record_unchanged(
        &mut self,
        request: &FileReadRequest,
        result: &FileReadResult,
        delivered_at: SystemTime,
    ) {
        let revision = revision_from_hash(&result.file_hash);
        let range = (result.first_line, result.last_requested_line);
        self.successful_files = self.successful_files.saturating_add(1);
        self.budget.report.unchanged_files = self.budget.report.unchanged_files.saturating_add(1);
        self.budget.report.unchanged_bytes =
            self.budget.report.unchanged_bytes.saturating_add(result.requested_bytes);
        self.refreshed.push(RefreshedCoverage {
            path: result.canonical_path.clone(),
            range,
            file_hash: result.file_hash.clone(),
        });
        self.revisions.push(ReadFileRevision {
            path: result.canonical_path.clone(),
            revision: revision.clone(),
            total_lines: result.total_lines,
            visible_ranges: visible_ranges(range),
            truncated: false,
            next_start_line: None,
            next_end_line: None,
            unchanged: true,
            temporary_artifact: None,
        });
        let _ = write!(
            self.message,
            "\n{}\n[unchanged: identical to the copy already returned in full at {} \
             (revision {revision}); lines {}-{} remain valid edit evidence. Re-request with \
             force=true only if that copy is no longer in your context.]",
            request.spec(),
            describe_delivery_time(delivered_at),
            range.0,
            range.1,
        );
        self.stats_paths.push(PathBuf::from(&result.canonical_path));
    }

    #[expect(clippy::too_many_lines, reason = "one ordered budget decision per item")]
    fn record_content(&mut self, request: &FileReadRequest, result: FileReadResult) -> bool {
        let per_file = if request.is_bounded() { None } else { self.budget.budget.file_bytes };
        let allowed = min_option(per_file, self.budget.remaining);
        let call_limited = self.budget.remaining.is_some_and(|remaining| {
            per_file.is_none_or(|file| remaining < file) && result.content.len() > remaining
        });
        let (kept_lines, kept_bytes) = visible_prefix(&result.content, allowed);
        let shown_something = self.budget.report.returned_bytes > 0;
        if call_limited
            && shown_something
            && (kept_lines == 0 || allowed.is_some_and(|bytes| bytes < MIN_READ_BUDGET_BYTES))
        {
            // Only a sliver would fit after earlier items: defer the whole
            // item so the next call can return it intact.
            self.budget.report.deferred.push(request.spec());
            return true;
        }

        let FileReadResult {
            content,
            line_ends,
            first_line,
            last_requested_line,
            requested_bytes,
            token_limit,
            canonical_path,
            file_hash,
            total_lines,
            ..
        } = result;
        let next_start = first_line.saturating_add(kept_lines);
        let truncated = next_start <= last_requested_line;
        let visible = (first_line, next_start.saturating_sub(1));
        let revision = revision_from_hash(&file_hash);
        let next_end = truncated.then(|| {
            next_chunk_end(&line_ends, kept_lines, kept_bytes, self.budget.chunk_bytes())
                .map_or(last_requested_line, |line| first_line.saturating_add(line))
                .clamp(next_start, last_requested_line)
        });

        self.successful_files = self.successful_files.saturating_add(1);
        let entry = self
            .file_ranges
            .entry(canonical_path.clone())
            .or_insert_with(|| (Vec::new(), file_hash.clone(), total_lines));
        if entry.1 != file_hash || entry.2 != total_lines {
            // The same path can be requested more than once in one batch. If
            // it changed between reads, keep coverage only for the version
            // whose hash will guard the next edit.
            *entry = (Vec::new(), file_hash.clone(), total_lines);
        }
        entry.0.push(visible);
        if truncated {
            self.budget.report.truncated_files =
                self.budget.report.truncated_files.saturating_add(1);
            self.budget.report.omitted_bytes = self
                .budget
                .report
                .omitted_bytes
                .saturating_add(requested_bytes.saturating_sub(kept_bytes));
        } else {
            self.delivered.push(DeliveredRange {
                path: canonical_path.clone(),
                start_line: request.start_line_num,
                end_line: request.end_line_num,
                file_hash: file_hash.clone(),
            });
        }
        self.budget.report.returned_bytes =
            self.budget.report.returned_bytes.saturating_add(kept_bytes);
        if let Some(remaining) = self.budget.remaining.as_mut() {
            *remaining = remaining.saturating_sub(kept_bytes);
        }
        self.revisions.push(ReadFileRevision {
            path: canonical_path.clone(),
            revision: revision.clone(),
            total_lines,
            visible_ranges: visible_ranges(visible),
            truncated,
            next_start_line: truncated.then_some(next_start),
            next_end_line: next_end,
            unchanged: false,
            temporary_artifact: None,
        });

        let _ = write!(self.message, "\n{}\n```\n{}\n```", request.spec(), &content[..kept_bytes]);
        if let Some(next_end) = next_end {
            let limit = match token_limit {
                Some((tokens, max_tokens)) if kept_bytes == content.len() => {
                    format!("{tokens} tokens exceeded the {max_tokens}-token file limit")
                }
                _ => format!("{}-byte response budget", allowed.unwrap_or(kept_bytes)),
            };
            let shown = if kept_lines == 0 {
                format!("line {next_start} alone exceeds the {limit}")
            } else {
                format!("lines {}-{} of {total_lines} shown ({limit})", visible.0, visible.1)
            };
            let outline = self
                .budget
                .chunk_bytes()
                .filter(|chunk| requested_bytes > chunk.saturating_mul(OUTLINE_HINT_CHUNKS))
                .filter(|_| outline_supported(&request.clean_path))
                .map_or("", |_| " CodeMap outline maps this file's symbols more cheaply.");
            let _ = write!(
                self.message,
                "\n[truncated: {shown}; {} more bytes requested. Next: ReadFiles \"{}:{next_start}-\
                 {next_end}\" (revision {revision}).{outline}]",
                requested_bytes.saturating_sub(kept_bytes),
                request.clean_path,
            );
        }
        self.stats_paths.push(PathBuf::from(canonical_path));
        call_limited && truncated
    }
}

fn visible_ranges((start, end): (usize, usize)) -> Vec<ReadLineRange> {
    (start <= end)
        .then_some(ReadLineRange { start_line: start, end_line: end })
        .into_iter()
        .collect()
}

/// Longest prefix of complete lines that fits `allowed` bytes.
fn visible_prefix(content: &str, allowed: Option<usize>) -> (usize, usize) {
    let allowed = allowed.unwrap_or(usize::MAX);
    let mut lines = 0;
    let mut bytes = 0;
    for line in content.split_inclusive('\n') {
        if bytes + line.len() > allowed {
            break;
        }
        bytes += line.len();
        lines += 1;
    }
    (lines, bytes)
}

/// Zero-based index (relative to the first requested line) of the last line
/// of a continuation that fits `chunk` bytes. `None` means "to the end".
fn next_chunk_end(
    line_ends: &[usize],
    kept_lines: usize,
    kept_bytes: usize,
    chunk: Option<usize>,
) -> Option<usize> {
    let chunk = chunk?;
    let rest = line_ends.get(kept_lines..).filter(|rest| !rest.is_empty())?;
    let fitting = rest.partition_point(|end| end.saturating_sub(kept_bytes) <= chunk);
    // A single oversized line still advances; the bounded follow-up then
    // reports it explicitly instead of looping on an empty chunk.
    Some(kept_lines + fitting.max(1) - 1)
}

fn outline_supported(file_path: &str) -> bool {
    Path::new(file_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(crate::utils::symbols::supports)
}

/// UTC wall-clock time plus age, without enabling `time` formatting features.
fn describe_delivery_time(at: SystemTime) -> String {
    let utc = time::OffsetDateTime::from(at);
    let age = SystemTime::now().duration_since(at).unwrap_or_default().as_secs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z, {}m{:02}s ago",
        utc.year(),
        u8::from(utc.month()),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
        age / 60,
        age % 60
    )
}

/// Complete result of one batched read. The MCP adapter keeps a batch with
/// partial successes a SUCCESS when every per-file error is just a missing
/// path (exploratory guesses are normal for remote models; the errors stay
/// visible inline and in structured `missing_files`), and returns an honest
/// `isError: true` only when nothing was read or an error goes beyond a
/// missing file (security, policy, session).
#[derive(Debug)]
pub struct ReadFilesOutcome {
    pub text: String,
    pub successful_files: usize,
    pub files: Vec<ReadFileRevision>,
    pub errors: Vec<WinxError>,
    pub budget: ReadBudgetReport,
}

/// Maximum amount of data to read from a file
const MAX_FILE_SIZE: u64 = 50_000_000;

fn range_format(start_line_num: Option<usize>, end_line_num: Option<usize>) -> String {
    let st = start_line_num.map_or(String::new(), |n| n.to_string());
    let end = end_line_num.map_or(String::new(), |n| n.to_string());

    if st.is_empty() && end.is_empty() {
        String::new()
    } else {
        format!(":{st}-{end}")
    }
}

/// Rendered size of one line exactly as `read_file` formats it.
fn rendered_line_bytes(line_number: usize, line: &str, show_line_numbers: bool) -> usize {
    let prefix = if show_line_numbers { decimal_digits(line_number) + 1 } else { 0 };
    prefix + line.len() + 1
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

#[instrument(level = "debug", skip(file_path))]
#[expect(clippy::too_many_arguments, reason = "one blocking worker call with explicit inputs")]
fn read_file(
    file_path: &str,
    max_tokens: Option<usize>,
    cwd: &Path,
    workspace_root: &Path,
    show_line_numbers: bool,
    start_line_num: Option<usize>,
    end_line_num: Option<usize>,
    caps: WorkerCaps,
) -> Result<FileReadResult> {
    let file_path_expanded = expand_user(file_path);
    let path = if Path::new(&file_path_expanded).is_absolute() {
        PathBuf::from(&file_path_expanded)
    } else {
        cwd.join(&file_path_expanded)
    };

    if !path.exists() {
        return Err(WinxError::FileNotFound { path: path.clone() });
    }

    let path = match validate_path_in_workspace(&path, workspace_root) {
        Ok(canonical) => canonical,
        Err(security_err) => {
            return Err(WinxError::PathSecurityError {
                path: path.clone(),
                message: security_err.to_string(),
            });
        }
    };

    if !path.is_file() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "Path exists but is not a file".to_string(),
        });
    }

    let content = read_file_to_string(&path, MAX_FILE_SIZE)?;
    let file_hash = hash_content(&content);
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start_idx = start_line_num.map_or(0, |n| n.saturating_sub(1).min(lines.len()));
    let end_idx = end_line_num.map_or(lines.len(), |n| n.min(lines.len()));

    if start_idx > lines.len() || start_idx > end_idx {
        return Err(ErrorRecovery::param_error(
            "line_range",
            &format!("Invalid line range for file with {} lines", lines.len()),
        ));
    }

    let first_line = start_idx + 1;
    let requested = &lines[start_idx..end_idx];
    // Render only what the budget can return, but measure every requested line
    // (cheap arithmetic) so the response reports exact omitted bytes and an
    // exact next chunk. Huge files are therefore never fully formatted or
    // tokenized just to be cut afterwards.
    let render_cap = caps.render.unwrap_or(usize::MAX);
    let mut result_content = String::new();
    let mut content_lines = 0;
    let mut line_ends = Vec::new();
    let mut requested_bytes = 0_usize;
    for (offset, line) in requested.iter().enumerate() {
        let line_number = first_line + offset;
        requested_bytes = requested_bytes.saturating_add(rendered_line_bytes(
            line_number,
            line,
            show_line_numbers,
        ));
        if requested_bytes <= caps.horizon {
            line_ends.push(requested_bytes);
        }
        if requested_bytes <= render_cap && content_lines == offset {
            if show_line_numbers {
                let _ = writeln!(result_content, "{line_number} {line}");
            } else {
                result_content.push_str(line);
                result_content.push('\n');
            }
            content_lines += 1;
        }
    }

    let max_tokens = max_tokens.unwrap_or_else(|| select_max_tokens(file_path));
    // Byte-level BPE emits at most one token per input byte. Small payloads are
    // therefore proven to fit without initializing the tokenizer; larger ones
    // still get exact counting and token-boundary truncation.
    let (tokens_count, token_ids) =
        if crate::utils::encoder::definitely_fits_token_budget(&result_content, max_tokens) {
            (crate::utils::encoder::estimate_tokens(&result_content), None)
        } else {
            let ids = crate::utils::encoder::encode_ids(&result_content);
            let count = ids
                .as_ref()
                .map_or_else(|| crate::utils::encoder::estimate_tokens(&result_content), Vec::len);
            (count, ids)
        };

    let mut token_limit = None;
    if tokens_count > max_tokens {
        truncate_to_token_budget(&mut result_content, max_tokens, token_ids);
        token_limit = Some((tokens_count, max_tokens));
    }

    Ok(FileReadResult {
        content: result_content,
        line_ends,
        first_line,
        last_requested_line: end_idx,
        requested_bytes,
        token_limit,
        canonical_path: path.to_string_lossy().to_string(),
        file_hash,
        total_lines,
    })
}

fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    })
}

pub(crate) fn revision_from_hash(hash: &str) -> String {
    format!("sha256:{hash}")
}

fn truncate_to_token_budget(content: &mut String, max_tokens: usize, ids: Option<Vec<u32>>) {
    // `ids` were already computed by the caller (the token count needs them too),
    // so reuse them instead of re-encoding the whole string a second time here.
    let Some(ids) = ids else {
        // No tokenizer available: fall back to a char-count cut.
        let byte_idx = byte_index_for_char_count(content, max_tokens);
        content.truncate(byte_idx);
        trim_to_last_line_boundary(content);
        return;
    };

    if ids.len() <= max_tokens {
        return;
    }

    if let Some(decoded) = crate::utils::encoder::decode_ids(&ids[..max_tokens]) {
        *content = decoded;
    } else {
        let byte_idx = byte_index_for_char_count(content, max_tokens);
        content.truncate(byte_idx);
    }
    trim_to_last_line_boundary(content);
}

fn trim_to_last_line_boundary(content: &mut String) {
    if let Some(last_nl) = content.rfind('\n') {
        content.truncate(last_nl + 1);
    } else {
        // A partial first line is not safe to record as read: the continuation
        // would skip its unseen suffix. Keep no content/range and resume from
        // the same line instead.
        content.clear();
    }
}

fn byte_index_for_char_count(content: &str, char_count: usize) -> usize {
    content.char_indices().nth(char_count).map_or(content.len(), |(idx, _)| idx)
}

fn select_max_tokens(file_path: &str) -> usize {
    // Budgets are overridable per deployment via env vars so large-context
    // clients can pull more of each file into context (defaults match wcgw).
    if is_source_code_file(file_path) {
        crate::utils::encoder::budget_from_env("WINX_CODING_TOKEN_BUDGET", CODING_MAX_TOKENS)
    } else {
        crate::utils::encoder::budget_from_env("WINX_NONCODING_TOKEN_BUDGET", NONCODING_MAX_TOKENS)
    }
}

pub(crate) fn configured_parallelism() -> usize {
    parse_read_parallelism(crate::config::env_text("WINX_READ_PARALLELISM").as_deref())
}

fn parse_read_parallelism(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|parallelism| *parallelism > 0)
        .unwrap_or(DEFAULT_READ_PARALLELISM)
        .min(MAX_READ_PARALLELISM)
}

fn is_source_code_file(file_path: &str) -> bool {
    let path = Path::new(file_path);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();

    matches!(file_name, "Makefile" | "Dockerfile" | "Jenkinsfile")
        || matches!(
            extension,
            "py" | "pyx"
                | "pyi"
                | "pyw"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "mjs"
                | "cjs"
                | "html"
                | "css"
                | "scss"
                | "sass"
                | "less"
                | "c"
                | "h"
                | "cpp"
                | "cxx"
                | "cc"
                | "hpp"
                | "java"
                | "kt"
                | "go"
                | "rs"
                | "rb"
                | "php"
                | "sh"
                | "bash"
                | "zsh"
                | "sql"
                | "xml"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "md"
                | "ex"
                | "exs"
        )
}

async fn execute_read_requests(
    requests: &[FileReadRequest],
    cwd: &Path,
    workspace_root: &Path,
    show_line_numbers: bool,
    budget: ReadBudgetState,
) -> ReadBatchState {
    let mut state = ReadBatchState::new(budget);

    // Files are independent, so perform blocking filesystem/tokenizer work in a
    // bounded pool. Results are still consumed in request order, preserving the
    // stable response and the rule that an exhausted call budget defers the rest.
    'batches: for batch in requests.chunks(configured_parallelism()) {
        let mut tasks = Vec::with_capacity(batch.len());
        for request in batch.iter().cloned() {
            let worker_path = request.clean_path.clone();
            let worker_cwd = cwd.to_path_buf();
            let worker_root = workspace_root.to_path_buf();
            let start_line_num = request.start_line_num;
            let end_line_num = request.end_line_num;
            let caps = state.budget.worker_caps(&request);
            let task = tokio::task::spawn_blocking(move || {
                read_file(
                    &worker_path,
                    Some(select_max_tokens(&worker_path)),
                    &worker_cwd,
                    &worker_root,
                    show_line_numbers,
                    start_line_num,
                    end_line_num,
                    caps,
                )
            });
            tasks.push((request, task));
        }

        let mut tasks = tasks.into_iter();
        while let Some((request, task)) = tasks.next() {
            let result = task.await.unwrap_or_else(|error| {
                Err(WinxError::CommandExecutionError(format!(
                    "ReadFiles worker failed for {}: {error}",
                    request.clean_path
                )))
            });
            if state.record_result(&request, result) {
                // Running blocking jobs cannot be cancelled, but abort prevents
                // queued work from starting. Only returned content is guarded.
                for (_, pending) in tasks {
                    pending.abort();
                }
                let deferred = &mut state.budget.report.deferred;
                deferred.extend(requests[request.index + 1..].iter().map(FileReadRequest::spec));
                break 'batches;
            }
        }
    }
    if !state.budget.report.deferred.is_empty() {
        let quoted = state
            .budget
            .report
            .deferred
            .iter()
            .map(|spec| format!("\"{spec}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            state.message,
            "\n\n[response budget reached ({} bytes); not read: {quoted}. Call ReadFiles again \
             for exactly these.]",
            state.budget.report.call_bytes.unwrap_or(state.budget.report.returned_bytes),
        );
    }
    state
}

async fn persist_read_stats(workspace_root: &Path, stats_paths: Vec<PathBuf>) {
    if stats_paths.is_empty() {
        return;
    }
    let stats_root = workspace_root.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        crate::utils::workspace_stats::record_reads(&stats_root, &stats_paths)
    })
    .await;
    match result {
        Ok(Err(error)) => debug!("failed to record read stats: {error}"),
        Err(error) => debug!("read stats worker failed: {error}"),
        Ok(Ok(())) => {}
    }
}

/// Text-only library API. Direct callers always receive content: unchanged
/// repeats are compacted only for MCP callers through
/// [`handle_tool_call_detailed`].
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    mut read_files: ReadFiles,
) -> Result<String> {
    read_files.force = true;
    let outcome = handle_tool_call_detailed(bash_state_arc, read_files).await?;
    if outcome.successful_files == 0 {
        if let Some(error) = outcome.errors.into_iter().next() {
            return Err(error);
        }
    }
    Ok(outcome.text)
}

pub async fn handle_tool_call_detailed(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_files: ReadFiles,
) -> Result<ReadFilesOutcome> {
    handle_tool_call_with_budget(bash_state_arc, read_files, ReadResponseBudget::from_env()).await
}

/// [`handle_tool_call_detailed`] with an explicit budget instead of the
/// environment, for embedding and deterministic tests.
#[expect(clippy::too_many_lines, reason = "snapshot, read and commit share one guard lifecycle")]
pub async fn handle_tool_call_with_budget(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_files: ReadFiles,
    budget: ReadResponseBudget,
) -> Result<ReadFilesOutcome> {
    // Compaction needs an explicit thread: an anonymous call may resolve to
    // whichever session was last active, which is not proof of what this
    // conversation already holds.
    let compact_repeats =
        !read_files.force && !crate::types::normalize_thread_id(&read_files.thread_id).is_empty();
    let (cwd, workspace_root, temporary_thread, unchanged_candidates) = {
        let bash_state_guard = bash_state_arc.lock().await;
        let bash_state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let candidates = if compact_repeats {
            bash_state.unchanged_read_candidates(budget.unchanged_ttl, Instant::now())
        } else {
            Vec::new()
        };
        (
            bash_state.cwd.clone(),
            bash_state.workspace_root.clone(),
            bash_state.current_thread_id.clone(),
            candidates,
        )
    };

    let requests = read_files
        .file_paths
        .iter()
        .enumerate()
        .map(|(index, file_path)| FileReadRequest {
            index,
            requested_path: file_path.clone(),
            clean_path: read_files.get_clean_path(index),
            start_line_num: read_files.start_line_nums.get(index).copied().flatten(),
            end_line_num: read_files.end_line_nums.get(index).copied().flatten(),
        })
        .collect::<Vec<_>>();
    let batch = execute_read_requests(
        &requests,
        &cwd,
        &workspace_root,
        read_files.show_line_numbers(),
        ReadBudgetState::new(budget, unchanged_candidates),
    )
    .await;
    let ReadBatchState {
        message,
        file_ranges,
        refreshed,
        delivered,
        stats_paths,
        successful_files,
        mut revisions,
        errors,
        budget,
    } = batch;
    let temporary_workspace = workspace_root.clone();
    revisions = tokio::task::spawn_blocking(move || {
        for receipt in &mut revisions {
            if let Ok(Some((text, metadata))) = crate::utils::agent_temp::release::read_managed(
                &temporary_workspace,
                &temporary_thread,
                Path::new(&receipt.path),
            ) {
                // The descriptor used for generation observed exactly the bytes
                // whose revision this response exposed. A concurrent change
                // removes eligibility rather than issuing a mismatched token.
                if revision_from_hash(&hash_content(&text)) == receipt.revision {
                    receipt.temporary_artifact = Some(metadata);
                }
            }
        }
        revisions
    })
    .await
    .map_err(|error| {
        WinxError::CommandExecutionError(format!("temporary read metadata worker failed: {error}"))
    })?;
    persist_read_stats(&workspace_root, stats_paths).await;

    let mut bash_state_guard = bash_state_arc.lock().await;
    if let Some(bash_state) = bash_state_guard.as_mut() {
        for (path, (ranges, file_hash, total_lines)) in file_ranges {
            bash_state.record_read_coverage(&path, ranges, file_hash, total_lines);
        }
        for refresh in refreshed {
            let guarded_lines = bash_state
                .whitelist_for_overwrite
                .get(&refresh.path)
                .filter(|guard| guard.file_hash == refresh.file_hash)
                .map(|guard| guard.total_lines);
            if let Some(total_lines) = guarded_lines {
                bash_state.record_read_coverage(
                    &refresh.path,
                    [refresh.range],
                    refresh.file_hash,
                    total_lines,
                );
            }
        }
        for delivery in delivered {
            // Remember only content whose hash is the one now guarding edits;
            // a same-batch re-read of a changed file supersedes older copies.
            let guarded = bash_state
                .whitelist_for_overwrite
                .get(&delivery.path)
                .is_some_and(|guard| guard.file_hash == delivery.file_hash);
            if guarded {
                bash_state.record_read_delivery(
                    &delivery.path,
                    delivery.start_line,
                    delivery.end_line,
                    &delivery.file_hash,
                );
            }
        }
    }

    Ok(ReadFilesOutcome {
        text: message,
        successful_files,
        files: revisions,
        errors,
        budget: budget.report,
    })
}

#[cfg(test)]
mod tests;
