//! Resolve a selector and render its spans from one immutable read snapshot.
use super::{
    hash_content, read_file, read_path, render_source, select_max_tokens, FileReadRequest,
    ReadSource, ResolvedReads, WorkerCaps,
};
use crate::errors::{Result, WinxError};
use crate::utils::path::expand_user;
use crate::utils::symbols::definitions::{self, FailureReason};
use std::path::{Path, PathBuf};

pub(super) fn read_request(
    request: &FileReadRequest,
    cwd: &Path,
    workspace: &Path,
    show_line_numbers: bool,
    caps: WorkerCaps,
) -> Result<ResolvedReads> {
    let expanded = expand_user(&request.clean_path);
    let literal = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        cwd.join(&expanded)
    };
    // Preserve literal filenames, including a # suffix followed by a numeric range.
    if !request.clean_path.contains('#') || literal.exists() {
        return read_file(
            &request.clean_path,
            Some(select_max_tokens(&request.clean_path)),
            cwd,
            workspace,
            show_line_numbers,
            request.start_line_num,
            request.end_line_num,
            caps,
        )
        .map(|result| vec![(request.clone(), result)]);
    }
    let Some((file, selector)) = request.clean_path.rsplit_once('#') else { return Ok(Vec::new()) };
    let path = read_path(file, cwd, workspace)?;
    let fail = |failure| WinxError::SymbolReadRejected {
        path: path.clone(),
        symbol: selector.to_owned(),
        failure,
    };
    if request.start_line_num.is_some() || request.end_line_num.is_some() {
        return Err(fail(FailureReason::InvalidSelector.into()));
    }
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    if !crate::utils::symbols::supports(extension) {
        return Err(fail(FailureReason::UnsupportedLanguage.into()));
    }
    let bytes = crate::utils::mmap::read_file_optimized(
        &path,
        definitions::MAX_SOURCE_BYTES.try_into().unwrap_or(u64::MAX),
    )
    .map_err(|error| {
        if matches!(error, WinxError::FileTooLarge { .. }) {
            fail(FailureReason::SourceTooLarge.into())
        } else {
            error
        }
    })?;
    let content = String::from_utf8(bytes).map_err(|_| fail(FailureReason::ParseFailed.into()))?;
    let source = ReadSource {
        file_hash: hash_content(&content),
        content,
        canonical_path: path.to_string_lossy().into_owned(),
    };
    let ranges = definitions::resolve(extension, &source.content, selector).map_err(fail)?;
    ranges
        .into_iter()
        .map(|range| {
            let mut normalized = request.clone();
            normalized.clean_path.clone_from(&source.canonical_path);
            normalized.start_line_num = Some(range.start_line);
            normalized.end_line_num = Some(range.end_line);
            normalized.resolved_symbol = true;
            render_source(
                &source,
                Some(select_max_tokens(file)),
                file,
                show_line_numbers,
                (normalized.start_line_num, normalized.end_line_num),
                caps,
            )
            .map(|result| (normalized, result))
        })
        .collect()
}
