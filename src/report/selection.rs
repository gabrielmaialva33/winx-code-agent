//! Bounded event selection and HTTP build attribution over stable log boundaries.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _};
use std::path::{Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;

use super::{parse_event, parse_timestamp, string, UsageEvent};
use crate::errors::{Result, WinxError};

const HTTP_BATCH_SIZE: usize = 4096;

pub(super) struct Selection {
    pub(super) events: VecDeque<UsageEvent>,
    pub(super) lines_seen: u64,
    pub(super) invalid_lines: u64,
}

struct LogSnapshot {
    path: PathBuf,
    file: File,
    length: u64,
}

impl LogSnapshot {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|error| file_error(path, "opening", &error))?;
        let length = file
            .metadata()
            .map_err(|error| file_error(path, "reading metadata for", &error))?
            .len();
        Ok(Self { path: path.to_path_buf(), file, length })
    }
}

struct PositionedEvent {
    position: u64,
    event: UsageEvent,
}

#[derive(Default)]
struct ReadCounts {
    lines_seen: u64,
    invalid_lines: u64,
}

#[derive(Default)]
struct BuildEvidence {
    matching: bool,
    conflicting: bool,
}

type RequestKey = (String, String);

pub(super) fn select(
    paths: &[PathBuf],
    last_events: usize,
    cutoff: Option<OffsetDateTime>,
    build_filter: Option<&str>,
) -> Result<Selection> {
    let mut sources =
        paths.iter().map(|path| LogSnapshot::open(path)).collect::<Result<Vec<_>>>()?;
    let mut recent = VecDeque::with_capacity(last_events.min(HTTP_BATCH_SIZE));
    let mut candidates = VecDeque::with_capacity(HTTP_BATCH_SIZE);
    let counts = scan(&mut sources, |position, event| {
        if matches!(
            string(&event.fields, "event"),
            "tool_started" | "tool_phase" | "tool_phase_trace_truncated"
        ) || !within_time(&event, cutoff)
        {
            return;
        }
        let selected = build_filter.is_none_or(|filter| matches_build(&event, filter));
        let row = PositionedEvent { position, event };
        if selected {
            push_bounded(&mut recent, row, last_events);
        } else if can_inherit_build(&row.event) {
            push_bounded(&mut candidates, row, HTTP_BATCH_SIZE);
        }
    })?;
    if let Some(filter) = build_filter {
        attribute_http(&mut sources, &mut recent, candidates, last_events, cutoff, filter)?;
    }
    Ok(Selection {
        events: recent.into_iter().map(|row| row.event).collect(),
        lines_seen: counts.lines_seen,
        invalid_lines: counts.invalid_lines,
    })
}

fn attribute_http(
    sources: &mut [LogSnapshot],
    recent: &mut VecDeque<PositionedEvent>,
    mut candidates: VecDeque<PositionedEvent>,
    last_events: usize,
    cutoff: Option<OffsetDateTime>,
    filter: &str,
) -> Result<()> {
    while let Some(before) = candidates.front().map(|row| row.position) {
        let may_have_older_candidates = candidates.len() == HTTP_BATCH_SIZE;
        let oldest = window_start(recent, last_events);
        candidates.retain(|row| oldest.is_none_or(|position| row.position > position));
        if candidates.is_empty() {
            break;
        }

        // Resolve only this batch's keys. A tool outside the returned window can
        // identify a late HTTP event, but conflicting build ownership cannot.
        let mut ownership = candidates
            .iter()
            .filter_map(|row| request_key(&row.event).map(|key| (key, BuildEvidence::default())))
            .collect::<BTreeMap<_, _>>();
        scan(sources, |_, event| {
            if string(&event.fields, "event") != "tool_call" {
                return;
            }
            let Some(key) = request_key(&event) else { return };
            if let Some(evidence) = ownership.get_mut(&key) {
                let matching = matches_build(&event, filter);
                evidence.matching |= matching;
                evidence.conflicting |= !matching;
            }
        })?;
        for row in candidates.drain(..) {
            let Some(key) = request_key(&row.event) else { continue };
            if ownership
                .get(&key)
                .is_some_and(|evidence| evidence.matching && !evidence.conflicting)
            {
                recent.push_back(row);
            }
        }
        recent.make_contiguous().sort_unstable_by_key(|row| row.position);
        while recent.len() > last_events {
            recent.pop_front();
        }

        if !may_have_older_candidates
            || window_start(recent, last_events).is_some_and(|position| position >= before)
        {
            break;
        }

        // Unknown HTTP traffic may fill a batch. Continue with older candidates
        // until none can enter the final window, rather than losing sparse pairs.
        let oldest = window_start(recent, last_events);
        scan(sources, |position, event| {
            if position < before
                && oldest.is_none_or(|oldest| position > oldest)
                && within_time(&event, cutoff)
                && can_inherit_build(&event)
            {
                push_bounded(&mut candidates, PositionedEvent { position, event }, HTTP_BATCH_SIZE);
            }
        })?;
    }
    Ok(())
}

fn scan(sources: &mut [LogSnapshot], mut visit: impl FnMut(u64, UsageEvent)) -> Result<ReadCounts> {
    let mut counts = ReadCounts::default();
    for source in sources {
        source.file.rewind().map_err(|error| file_error(&source.path, "rewinding", &error))?;
        // Keep the opened inode through rotations and ignore later appends.
        let reader = BufReader::new((&mut source.file).take(source.length));
        for line in reader.lines() {
            counts.lines_seen = counts.lines_seen.saturating_add(1);
            if let Some(event) = line.ok().and_then(|line| parse_event(&line)) {
                visit(counts.lines_seen, event);
            } else {
                counts.invalid_lines = counts.invalid_lines.saturating_add(1);
            }
        }
    }
    Ok(counts)
}

fn file_error(path: &Path, action: &str, error: &std::io::Error) -> WinxError {
    WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("{action} usage log: {error}"),
    }
}

fn matches_build(event: &UsageEvent, filter: &str) -> bool {
    [string(&event.fields, "build"), string(&event.fields, "build_version")].contains(&filter)
}

fn within_time(event: &UsageEvent, cutoff: Option<OffsetDateTime>) -> bool {
    !cutoff.is_some_and(|cutoff| {
        event
            .timestamp
            .as_deref()
            .and_then(parse_timestamp)
            .is_some_and(|timestamp| timestamp < cutoff)
    })
}

fn can_inherit_build(event: &UsageEvent) -> bool {
    string(&event.fields, "event") == "http_request"
        && string(&event.fields, "build").is_empty()
        && string(&event.fields, "build_version").is_empty()
        && !string(&event.fields, "request_id").is_empty()
}

fn request_key(event: &UsageEvent) -> Option<RequestKey> {
    let request_id = string(&event.fields, "request_id");
    if request_id.is_empty() {
        return None;
    }
    let principal = match event.fields.get("principal") {
        None | Some(Value::Null) => "",
        Some(Value::String(principal)) => principal,
        _ => return None,
    };
    Some((principal.to_string(), request_id.to_string()))
}

fn window_start(events: &VecDeque<PositionedEvent>, limit: usize) -> Option<u64> {
    if events.len() == limit {
        events.front().map(|row| row.position)
    } else {
        None
    }
}

fn push_bounded<T>(events: &mut VecDeque<T>, event: T, limit: usize) {
    if events.len() == limit {
        events.pop_front();
    }
    events.push_back(event);
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    #[test]
    fn snapshot_keeps_initial_length_and_opened_file_after_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("usage.jsonl");
        let event = |request| {
            json!({"target":"winx::usage",
                "fields":{"event":"http_request","request_id":request}})
            .to_string()
                + "\n"
        };
        fs::write(&path, event("original"))?;
        let mut sources = vec![LogSnapshot::open(&path)?];
        let mut writer = OpenOptions::new().append(true).open(&path)?;
        writer.write_all(event("appended").as_bytes())?;
        drop(writer);

        for replaced in [false, true] {
            if replaced {
                fs::rename(&path, temp.path().join("rotated.jsonl"))?;
                fs::write(&path, event("replacement"))?;
            }
            let mut ids = Vec::new();
            let counts = scan(&mut sources, |_, record| {
                ids.push(string(&record.fields, "request_id").to_string());
            })?;
            assert_eq!(ids, ["original"]);
            assert_eq!(counts.lines_seen, 1);
            assert_eq!(counts.invalid_lines, 0);
        }
        Ok(())
    }
}
