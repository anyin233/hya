//! Literal text matching shared by the bare and full edit tools.

use std::ops::Range;

/// Why a literal does not identify exactly one span.
#[derive(Debug, Eq, PartialEq)]
pub enum UniqueSpanError {
    /// An empty literal cannot identify text to replace.
    Empty,
    /// No occurrence of the literal was found.
    Missing,
    /// The first two candidate byte offsets, possibly overlapping.
    Multiple {
        /// Byte offset of the first occurrence.
        first: usize,
        /// Byte offset of the next occurrence.
        second: usize,
    },
}

/// Find the byte range of a single non-empty literal occurrence.
///
/// Starts the second search one character after the first match so overlapping
/// candidates count as separate occurrences.
pub fn find_unique_span(text: &str, literal: &str) -> Result<Range<usize>, UniqueSpanError> {
    let first_char = literal.chars().next().ok_or(UniqueSpanError::Empty)?;
    let first = text.find(literal).ok_or(UniqueSpanError::Missing)?;
    let from = first + first_char.len_utf8();
    if let Some(relative) = text[from..].find(literal) {
        return Err(UniqueSpanError::Multiple {
            first,
            second: from + relative,
        });
    }
    Ok(first..first + literal.len())
}
