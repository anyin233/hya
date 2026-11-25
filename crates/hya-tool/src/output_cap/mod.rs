//! Global tool-output size guard.
//!
//! Ordinary results retain the historical display-text cap. Coding tools use a
//! separate allowlisted envelope cap so titles, model output, and presentation
//! metadata survive the trip through the durable event log without permitting
//! hostile fields to grow without bound.

mod coding_envelope;
mod grep_budget;
mod json_budget;
mod read_views;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

use crate::handle::ArtifactStore;
use crate::tool::ToolResultPolicy;
use serde_json::Value;

use coding_envelope::cap_coding_result;

/// Return the serialized byte width of one character inside a JSON string.
pub(crate) fn json_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{08}' | '\u{0c}' | '\n' | '\r' | '\t' => 2,
        character if character < ' ' => 6,
        character => character.len_utf8(),
    }
}
/// Return a JSON string's serialized width without allocating an encoded copy.
///
/// # Parameters
/// - `text`: UTF-8 string whose JSON width is needed.
///
/// # Returns
/// The quoted JSON string width, saturating on arithmetic overflow.
pub(crate) fn serialized_string_len(text: &str) -> usize {
    text.chars().fold(2usize, |used, character| {
        used.saturating_add(json_char_len(character))
    })
}

/// Maximum characters of ordinary tool output before it is truncated.
pub const MAX_TOOL_OUTPUT_CHARS: usize = 5000;
/// Characters kept from the start of a truncated ordinary result (headers,
/// summaries, and the leading JSON keys usually live there).
pub const TRUNCATED_HEAD_CHARS: usize = 2000;
/// Characters kept from the end of a truncated ordinary result (the latest
/// lines of a log or command output).
pub const TRUNCATED_TAIL_CHARS: usize = 2500;

/// Maximum raw UTF-8 bytes retained in a coding tool's model-facing output.
pub const MAX_CODING_OUTPUT_BYTES: usize = 50 * 1024;
/// Maximum raw UTF-8 bytes retained in a coding tool title.
pub const MAX_CODING_TITLE_BYTES: usize = 512;
/// Maximum raw UTF-8 bytes retained in one coding display text field.
pub const MAX_CODING_DISPLAY_BYTES: usize = 50 * 1024;
/// Maximum raw UTF-8 bytes retained in one Edit diff or patch field.
pub const MAX_CODING_DIFF_BYTES: usize = 50 * 1024;
/// Maximum serialized bytes retained by one coding attachment or its container.
/// Sized so an ordinary image or scanned page reaches a model that takes
/// images; it is counted apart from [`MAX_CODING_ENVELOPE_BYTES`].
pub const MAX_CODING_ATTACHMENT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum serialized bytes retained for an artifact path that must stay atomic.
pub(crate) const MAX_CODING_OUTPUT_PATH_BYTES: usize = 32 * 1024;
/// Maximum attachment entries retained after validation.
const MAX_CODING_ATTACHMENT_ENTRIES: usize = 16;
/// Maximum serialized bytes retained for one path-keyed diagnostic identity.
const MAX_CODING_DIAGNOSTIC_PATH_BYTES: usize = 4 * 1024;
/// Maximum warning or diagnostic rows retained in one metadata field.
pub const MAX_CODING_DIAGNOSTIC_ROWS: usize = 16;
/// Maximum raw UTF-8 bytes retained in one warning or diagnostic row.
pub const MAX_CODING_DIAGNOSTIC_ROW_BYTES: usize = 2 * 1024;
/// Maximum Grep display groups retained in one result.
pub const MAX_CODING_GREP_GROUPS: usize = 100;
/// Maximum Grep display rows retained across all groups.
pub const MAX_CODING_GREP_ROWS: usize = 2000;
/// Maximum serialized bytes retained by Grep display rows and groups.
pub const MAX_CODING_GREP_DISPLAY_BYTES: usize = 50 * 1024;
/// Maximum serialized JSON bytes retained by a coding result envelope, not
/// counting its attachment array (bounded by [`MAX_CODING_ATTACHMENT_BYTES`]).
pub const MAX_CODING_ENVELOPE_BYTES: usize = 256 * 1024;

const MAX_GENERIC_METADATA_ITEMS: usize = 128;
const MAX_METADATA_KEY_BYTES: usize = 128;
const MAX_SMALL_METADATA_STRING_BYTES: usize = 2 * 1024;

/// Facts reported in the retained metadata when a safety bound removed data.
#[derive(Default)]
struct TruncationFacts {
    output: bool,
    title: bool,
    display: bool,
    attachments: bool,
    diff: bool,
    diagnostics: bool,
    warnings: bool,
    rows: bool,
    groups: bool,
    metadata: bool,
    unknown: bool,
    envelope: bool,
}

impl TruncationFacts {
    /// Return whether any field was removed or shortened.
    fn any(&self) -> bool {
        self.output
            || self.title
            || self.display
            || self.attachments
            || self.diff
            || self.diagnostics
            || self.warnings
            || self.rows
            || self.groups
            || self.metadata
            || self.unknown
            || self.envelope
    }

    /// Mark a dropped or atomically bounded attachment and its display payload.
    fn mark_attachment(&mut self) {
        self.attachments = true;
        self.display = true;
    }

    /// Mark the category associated with a recursively bounded value.
    fn mark(&mut self, kind: TruncationKind) {
        match kind {
            TruncationKind::Output => self.output = true,
            TruncationKind::Title => self.title = true,
            TruncationKind::Display => self.display = true,
            TruncationKind::Diff => self.diff = true,
            TruncationKind::Diagnostics => self.diagnostics = true,
            TruncationKind::Warnings => self.warnings = true,
            TruncationKind::Metadata => self.metadata = true,
        }
    }

    /// Merge truncation facts collected while bounding one nested value.
    fn merge(&mut self, other: &Self) {
        self.output |= other.output;
        self.title |= other.title;
        self.display |= other.display;
        self.attachments |= other.attachments;
        self.diff |= other.diff;
        self.diagnostics |= other.diagnostics;
        self.warnings |= other.warnings;
        self.rows |= other.rows;
        self.groups |= other.groups;
        self.metadata |= other.metadata;
        self.unknown |= other.unknown;
        self.envelope |= other.envelope;
    }
}

#[derive(Clone, Copy)]
enum TruncationKind {
    Output,
    Title,
    Display,
    Diff,
    Diagnostics,
    Warnings,
    Metadata,
}

/// Cap a successful tool `output` value using the arbitrary-value policy.
///
/// Under the limit the original [`Value`] is returned unchanged (shape preserved).
/// Over the limit the result becomes a string: a notice, the **first**
/// [`TRUNCATED_HEAD_CHARS`] and the **last** [`TRUNCATED_TAIL_CHARS`]
/// characters of the display text, with a marker between them naming the
/// omitted size.
#[must_use]
pub fn cap_tool_output(output: Value) -> Value {
    let text = value_as_display_text(&output);
    let n = text.chars().count();
    if n <= MAX_TOOL_OUTPUT_CHARS {
        return output;
    }
    Value::String(head_and_tail(&text, n, None))
}

/// The truncated view of `text` (`n` characters): notice, head, a marker
/// naming the omitted size (and the artifact holding everything), tail.
fn head_and_tail(text: &str, n: usize, artifact: Option<&str>) -> String {
    let head: String = text.chars().take(TRUNCATED_HEAD_CHARS).collect();
    let tail = last_n_chars(text, TRUNCATED_TAIL_CHARS);
    let omitted = n.saturating_sub(TRUNCATED_HEAD_CHARS + TRUNCATED_TAIL_CHARS);
    let (full, marker) = match artifact {
        Some(id) => (
            format!(" Full output: artifact://{id} — read that handle for the rest"),
            format!("[… {omitted} chars omitted; full output: artifact://{id} …]"),
        ),
        None => (String::new(), format!("[… {omitted} chars omitted …]")),
    };
    format!(
        "[tool output truncated: original {n} chars; showing first {TRUNCATED_HEAD_CHARS} and last {TRUNCATED_TAIL_CHARS} chars.{full}]\n{head}\n{marker}\n{tail}"
    )
}

/// Cap a successful result according to the executing tool's policy.
///
/// Coding policies retain the approved `{title, output, metadata}` envelope
/// and bounded presentation fields. A non-object coding result is treated as
/// an ordinary result because it is not an approved presentation envelope.
#[must_use]
pub fn cap_tool_output_with_policy(output: Value, policy: ToolResultPolicy) -> Value {
    match policy {
        ToolResultPolicy::Default => cap_tool_output(output),
        ToolResultPolicy::SelfBudgeted => output,
        ToolResultPolicy::Coding => cap_coding_result(output, false),
        ToolResultPolicy::CodingWithDiff => cap_coding_result(output, true),
    }
}

/// Cap a successful result, preserving whatever the cap drops.
///
/// Truncation on its own tells the model *that* output was lost; it cannot say
/// where the rest went, so the only route back to the full result is to run the
/// tool again and pay its cost a second time. Writing the whole value to `store`
/// first and naming the artifact in the notice turns a one-way truncation into a
/// bounded view plus an address.
///
/// Only [`ToolResultPolicy::Default`] results spill. A coding envelope already
/// describes a file the model can re-read by path, so an artifact would be a
/// second copy of something that is addressable.
#[must_use]
pub fn cap_tool_output_spilling(
    output: Value,
    policy: ToolResultPolicy,
    store: &ArtifactStore,
    tool: &str,
) -> Value {
    if !matches!(policy, ToolResultPolicy::Default) {
        return cap_tool_output_with_policy(output, policy);
    }
    let text = value_as_display_text(&output);
    let n = text.chars().count();
    if n <= MAX_TOOL_OUTPUT_CHARS {
        return output;
    }
    // A store that cannot accept the body still yields a capped result. Bounding
    // the transcript is the job that cannot be skipped; losing the recovery path
    // is better than losing the turn.
    let Ok(meta) = store.store(tool, "text/plain", text.as_bytes()) else {
        return cap_tool_output(output);
    };
    Value::String(head_and_tail(&text, n, Some(&meta.id.to_string())))
}

/// Convert a value to its plain display text for the legacy policy.
fn value_as_display_text(value: &Value) -> String {
    match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    }
}

/// Return the last `n` Unicode scalar values without splitting UTF-8.
fn last_n_chars(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= n {
        return text.to_string();
    }
    text.chars().skip(total - n).collect()
}
