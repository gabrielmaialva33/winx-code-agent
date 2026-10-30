//! Client-specific delivery of tool-result text.
//!
//! Every finalized result carries its text twice: in the `content` text blocks
//! and in `structuredContent.output`. Both can reach the model. For the `ChatGPT`
//! profile, Winx conservatively replaces exactly mirrored text with a pointer;
//! other clients keep both copies and full mirroring remains configurable.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value};

use super::client_profile::ClientProfile;

/// `auto` (default) or `full`; see [`ResultTextDelivery::resolve`].
pub(crate) const RESULT_TEXT_MIRROR_ENV: &str = "WINX_RESULT_TEXT_MIRROR";

/// Cap for an error message repeated in the pointer line, in bytes.
const MAX_POINTER_MESSAGE_BYTES: usize = 512;

/// Where the delivered text of a tool result lives on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResultTextDelivery {
    /// Text blocks and `structuredContent.output` both carry the text.
    Mirrored,
    /// Only `structuredContent` carries it; text blocks become one pointer line.
    StructuredOnly,
}

impl ResultTextDelivery {
    pub(super) fn for_client(profile: ClientProfile) -> Self {
        Self::resolve(profile, crate::config::env_text(RESULT_TEXT_MIRROR_ENV).as_deref())
    }

    /// Unset or `auto` selects the `ChatGPT` optimization; `full`, and any
    /// unrecognized value, keeps both copies.
    fn resolve(profile: ClientProfile, setting: Option<&str>) -> Self {
        let auto = setting.is_none_or(|value| value.eq_ignore_ascii_case("auto"));
        if auto && profile == ClientProfile::ChatGpt {
            Self::StructuredOnly
        } else {
            Self::Mirrored
        }
    }

    /// Run after `finalize_tool_result` and any later content rewrite (policy
    /// banners), and before unscoping, so the pointer is unscoped like the rest.
    /// Images and other non-text blocks are kept in place. A result whose text
    /// is not exactly mirrored in `structuredContent` is left untouched, so this
    /// never drops information a structured-only client could not see.
    pub(super) fn apply(self, result: &mut CallToolResult) {
        if self == Self::Mirrored {
            return;
        }
        let Some(Value::Object(envelope)) = result.structured_content.as_ref() else { return };
        if !text_is_mirrored(&result.content, envelope) {
            return;
        }
        let mut pointer =
            Some(ContentBlock::text(pointer_line(envelope, result.is_error == Some(true))));
        result.content = std::mem::take(&mut result.content)
            .into_iter()
            .filter_map(
                |block| if block.as_text().is_some() { pointer.take() } else { Some(block) },
            )
            .collect();
    }
}

/// Whether `structuredContent` already holds everything the text blocks say:
/// `output` equals their joined text (or its redaction, when a secret straddled
/// two blocks and only the joined pass caught it), or a `CodeMap` envelope
/// carries the symbols the text renders. `false` when there is no text block.
fn text_is_mirrored(content: &[ContentBlock], envelope: &Map<String, Value>) -> bool {
    let blocks: Vec<&str> =
        content.iter().filter_map(|block| block.as_text()).map(|t| t.text.as_str()).collect();
    if blocks.is_empty() {
        return false;
    }
    match envelope.get("output") {
        Some(Value::String(output)) => {
            joined_equals(output, &blocks)
                || (blocks.len() > 1
                    && crate::utils::redact::redact(&blocks.join("\n")) == output.as_str())
        }
        None => {
            let tool = envelope.get("tool").and_then(Value::as_str).unwrap_or_default();
            super::outcomes::code_map_symbols_are_structured(tool, envelope)
        }
        Some(_) => false,
    }
}

/// `output == blocks.join("\n")` without allocating the joined string.
fn joined_equals(output: &str, blocks: &[&str]) -> bool {
    let mut rest = output;
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            let Some(tail) = rest.strip_prefix('\n') else { return false };
            rest = tail;
        }
        let Some(tail) = rest.strip_prefix(block) else { return false };
        rest = tail;
    }
    rest.is_empty()
}

/// One short line for clients that still look at `content`. A tool error also
/// repeats its (already redacted) message, so the failure stays readable even
/// where a client hides `structuredContent` for error results.
fn pointer_line(envelope: &Map<String, Value>, is_error: bool) -> String {
    let location = if envelope.contains_key("output") {
        "structuredContent.output"
    } else {
        "structuredContent"
    };
    let mut line = String::new();
    if is_error {
        let message = envelope.get("message").and_then(Value::as_str).map_or("", str::trim);
        if !message.is_empty() {
            let shown = truncate_on_char_boundary(message, MAX_POINTER_MESSAGE_BYTES);
            line.push_str(shown);
            if shown.len() < message.len() {
                line.push('…');
            }
            line.push(' ');
        }
    }
    line.push_str("Result is in ");
    line.push_str(location);
    if let Some(status) = envelope.get("status").and_then(Value::as_str) {
        line.push_str(" (status: ");
        line.push_str(status);
        line.push(')');
    }
    line.push('.');
    line
}

fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
