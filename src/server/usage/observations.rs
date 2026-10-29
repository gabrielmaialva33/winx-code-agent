//! Aggregate request/result observations without retaining paths or execution contents.

use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;

#[derive(Clone, Copy, Default)]
pub(super) struct ReadShapes {
    pub whole_files: u64,
    pub bounded_ranges: u64,
    pub open_ended_ranges: u64,
}

#[derive(Clone, Copy, Default)]
pub(super) struct UsageObservationRequest {
    pub batch_items: usize,
    pub observe_schema: Option<u64>,
    pub requested_items: Option<u64>,
    pub read_schema: Option<u64>,
    pub read_shapes: Option<ReadShapes>,
}

#[derive(Default)]
pub(super) struct UsageObservationResult {
    pub returned_items: Option<u64>,
    pub items_with_issues: Option<u64>,
}

impl UsageObservationRequest {
    pub fn from_request(tool: &str, arguments: Option<&JsonObject>) -> Self {
        if tool == "ReadFiles" {
            let batch_items = arguments
                .and_then(|args| args.get("file_paths"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            return Self {
                batch_items,
                read_schema: Some(1),
                read_shapes: read_shapes(arguments),
                ..Self::default()
            };
        }
        let action = arguments.and_then(|args| args.get("action"));
        if tool == "Execution"
            && action.and_then(|action| action.get("type")).and_then(Value::as_str)
                == Some("observe")
        {
            let items = action.and_then(|action| action.get("items")).and_then(Value::as_array);
            return Self {
                batch_items: items.map_or(0, Vec::len),
                observe_schema: Some(1),
                requested_items: items.and_then(|items| u64::try_from(items.len()).ok()),
                ..Self::default()
            };
        }
        Self::default()
    }

    pub fn result(self, result: Option<&CallToolResult>) -> UsageObservationResult {
        if self.observe_schema.is_none() {
            return UsageObservationResult::default();
        }
        let items = result
            .and_then(|result| result.structured_content.as_ref())
            .and_then(|envelope| envelope.get("data"))
            .and_then(|data| data.get("items"))
            .and_then(Value::as_array);
        let Some(items) = items else {
            return UsageObservationResult::default();
        };
        // Multiple errors can affect one item; incomplete error metadata stays unknown.
        let items_with_issues = items.iter().try_fold(0_u64, |count, item| {
            let errors = item.get("errors")?.as_array()?;
            Some(count.saturating_add(u64::from(!errors.is_empty())))
        });
        UsageObservationResult {
            returned_items: u64::try_from(items.len()).ok(),
            items_with_issues,
        }
    }
}

fn read_shapes(arguments: Option<&JsonObject>) -> Option<ReadShapes> {
    // Reuse the tool's exact grammar, including Windows paths and open ranges.
    // Parsing performs no filesystem IO and none of the parsed strings are retained.
    let read: crate::types::ReadFiles =
        serde_json::from_value(Value::Object(arguments?.clone())).ok()?;
    let mut shapes = ReadShapes::default();
    for (start, end) in read.start_line_nums.into_iter().zip(read.end_line_nums) {
        match (start, end) {
            (_, Some(_)) => shapes.bounded_ranges = shapes.bounded_ranges.saturating_add(1),
            (Some(_), None) => {
                shapes.open_ended_ranges = shapes.open_ended_ranges.saturating_add(1);
            }
            (None, None) => shapes.whole_files = shapes.whole_files.saturating_add(1),
        }
    }
    Some(shapes)
}
