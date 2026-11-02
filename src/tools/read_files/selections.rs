//! Aggregate resolved request shapes; never retain paths or source contents.
use serde::{Deserialize, Serialize};

use super::{FileReadRequest, ResolvedReads};
use crate::errors::{Result, WinxError};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct ReadSelectionCounts {
    pub schema: u64,
    pub whole_files: u64,
    pub bounded_ranges: u64,
    pub open_ended_ranges: u64,
    pub symbols: u64,
    pub unknown: u64,
}

impl ReadSelectionCounts {
    pub(super) fn new(requests: usize) -> Self {
        Self { schema: 2, unknown: requests as u64, ..Self::default() }
    }

    pub fn total(self) -> Option<u64> {
        self.whole_files
            .checked_add(self.bounded_ranges)?
            .checked_add(self.open_ended_ranges)?
            .checked_add(self.symbols)?
            .checked_add(self.unknown)
    }

    pub(super) fn record(&mut self, original: &FileReadRequest, result: &Result<ResolvedReads>) {
        let count = match result {
            Ok(reads) => {
                let Some((resolved, _)) = reads.first() else { return };
                if resolved.resolved_symbol {
                    &mut self.symbols
                } else {
                    self.range_count(resolved)
                }
            }
            Err(WinxError::SymbolReadRejected { .. }) => &mut self.symbols,
            // A hash may name a literal file. Failed early resolution is unknown.
            Err(_) if original.clean_path.contains('#') => return,
            Err(_) => self.range_count(original),
        };
        *count = count.saturating_add(1);
        self.unknown = self.unknown.saturating_sub(1);
    }

    fn range_count(&mut self, request: &FileReadRequest) -> &mut u64 {
        match (request.start_line_num, request.end_line_num) {
            (_, Some(_)) => &mut self.bounded_ranges,
            (Some(_), None) => &mut self.open_ended_ranges,
            (None, None) => &mut self.whole_files,
        }
    }
}
