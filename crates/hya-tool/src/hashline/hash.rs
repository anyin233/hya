//! Contextual hashline formatting and strict anchor parsing.
//!
//! The observable behavior in this module is derived from
//! `pi-hashline-edit` 0.8.3 (`ba7db9943d0f58499b24c1f6bd64722580f772a5`),
//! whose hashline implementation is MIT licensed. hya uses the
//! `xxhash-rust` implementation with the same seed and update order as the
//! source package's `xxhashjs` wrapper. Hashes identify stale display rows;
//! they are not integrity or authorization data.

use std::borrow::Cow;

use xxhash_rust::xxh32::Xxh32;

/// First supported hash width.
pub(super) const HASH_LENGTH_MIN: usize = 2;
/// Last supported hash width.
pub(super) const HASH_LENGTH_MAX: usize = 4;
/// Default hash width used by the private runtime.
pub(super) const DEFAULT_HASH_LENGTH: usize = 2;
/// Nibble alphabet copied from the pinned hashline package.
pub(super) const NIBBLE_STR: &str = "ZPMQVRWSNKTXJBYH";

/// Parsed one-based line anchor and its optional copied display text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Anchor {
    /// One-based line number. The terminal split sentinel is addressable only
    /// for boundary operations and is never rendered.
    pub(super) line: usize,
    /// Configured-width contextual hash.
    pub(super) hash: String,
    /// Optional text copied after the display colon.
    pub(super) text_hint: Option<String>,
}

/// Return whether a width is one of the package-supported contextual widths.
#[must_use]
pub(super) const fn valid_hash_length(width: usize) -> bool {
    matches!(width, HASH_LENGTH_MIN..=HASH_LENGTH_MAX)
}

/// Return the example anchor used in parser diagnostics for a width.
#[must_use]
pub(super) fn example_anchor(width: usize) -> String {
    let suffix = "MQQV";
    let width = width.min(suffix.len());
    format!("5#{}", &suffix[..width])
}

/// Return whether a character is in the ECMAScript trailing whitespace set
/// used by JavaScript `String.trimEnd()`.
#[must_use]
fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            | '\u{2001}'
            | '\u{2002}'
            | '\u{2003}'
            | '\u{2004}'
            | '\u{2005}'
            | '\u{2006}'
            | '\u{2007}'
            | '\u{2008}'
            | '\u{2009}'
            | '\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// Trim only ECMAScript trailing whitespace without changing leading content.
#[must_use]
fn trim_ecmascript_end(text: &str) -> &str {
    let end = text
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!is_ecmascript_whitespace(character)).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    &text[..end]
}

/// Normalize one line for contextual hashing by removing CR and JavaScript
/// trailing whitespace. The common LF-normalized path borrows its input.
#[must_use]
pub(super) fn normalize_hash_input(line: &str) -> Cow<'_, str> {
    let trimmed = trim_ecmascript_end(line);
    if !trimmed.contains('\r') {
        return Cow::Borrowed(trimmed);
    }
    Cow::Owned(trimmed.replace('\r', ""))
}

/// Normalize CRLF and lone CR separators to LF for the hashline document view.
#[must_use]
pub(super) fn normalize_line_endings(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    normalized
}

/// Hash a contextual three-line window with seed zero and the source package's
/// NUL separators, without allocating a joined context string.
pub(super) fn compute_hash_from_context_with_width(
    previous: &str,
    current: &str,
    next: &str,
    width: usize,
) -> Result<String, super::HashlineError> {
    if !valid_hash_length(width) {
        return Err(super::HashlineError::new(
            "E_BAD_CONFIG",
            format!(
                "Hash length must be between {HASH_LENGTH_MIN} and {HASH_LENGTH_MAX}, got {width}."
            ),
        ));
    }

    let previous = normalize_hash_input(previous);
    let current = normalize_hash_input(current);
    let next = normalize_hash_input(next);
    // Xxh32 is the same XXH32 algorithm used by the pinned JavaScript wrapper.
    // Updating each part separately is byte-identical to prev + NUL + curr +
    // NUL + next and avoids a full joined-context allocation.
    let mut hasher = Xxh32::new(0);
    hasher.update(previous.as_bytes());
    hasher.update(&[0]);
    hasher.update(current.as_bytes());
    hasher.update(&[0]);
    hasher.update(next.as_bytes());
    Ok(encode_hash(hasher.digest(), width))
}

/// Hash a contextual window at the runtime's default two-character width.
#[must_use]
pub(super) fn compute_hash_from_context(previous: &str, current: &str, next: &str) -> String {
    compute_hash_from_context_with_width(previous, current, next, DEFAULT_HASH_LENGTH)
        .unwrap_or_default()
}

/// Encode the most-significant nibble of the selected low digest bits first.
#[must_use]
fn encode_hash(digest: u32, width: usize) -> String {
    let mut hash = String::with_capacity(width);
    for shift in (0..width).rev() {
        let nibble = ((digest >> (shift * 4)) & 0x0f) as usize;
        if let Some(character) = NIBBLE_STR.as_bytes().get(nibble) {
            hash.push(*character as char);
        }
    }
    hash
}

/// Compute a line hash against its complete document neighbors at width two.
#[must_use]
pub(super) fn compute_line_hash<L: AsRef<str>>(lines: &[L], index: usize) -> String {
    compute_line_hash_with_width(lines, index, DEFAULT_HASH_LENGTH).unwrap_or_default()
}

/// Compute a line hash against its complete document neighbors at a validated
/// width. Out-of-range neighbors are empty, matching package boundary behavior.
pub(super) fn compute_line_hash_with_width<L: AsRef<str>>(
    lines: &[L],
    index: usize,
    width: usize,
) -> Result<String, super::HashlineError> {
    let previous = index
        .checked_sub(1)
        .and_then(|position| lines.get(position))
        .map_or("", |line| line.as_ref());
    let current = lines.get(index).map_or("", |line| line.as_ref());
    let next = index
        .checked_add(1)
        .and_then(|position| lines.get(position))
        .map_or("", |line| line.as_ref());
    compute_hash_from_context_with_width(previous, current, next, width)
}

/// Split normalized text into borrowed model-visible lines.
///
/// The terminal empty split sentinel caused by a final newline is excluded;
/// the returned vector owns only slice pointers, not line contents.
#[must_use]
pub(super) fn visible_line_slices(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[cfg(test)]
/// Split normalized text into owned model-visible lines for edit application.
#[must_use]
pub(super) fn visible_lines(text: &str) -> Vec<String> {
    visible_line_slices(text)
        .into_iter()
        .map(str::to_owned)
        .collect()
}
/// Count visible normalized lines without copying their contents.
#[must_use]
pub(super) fn visible_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let count = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
    if text.ends_with('\n') {
        count.saturating_sub(1)
    } else {
        count
    }
}

/// Format one visible line with a caller-selected number-column width.
pub(super) fn format_hashline_line_with_width<L: AsRef<str>>(
    lines: &[L],
    line_number: usize,
    hash_width: usize,
    line_number_width: usize,
) -> Result<String, super::HashlineError> {
    if !valid_hash_length(hash_width) {
        return Err(super::HashlineError::new(
            "E_BAD_CONFIG",
            format!(
                "Hash length must be between {HASH_LENGTH_MIN} and {HASH_LENGTH_MAX}, got {hash_width}."
            ),
        ));
    }
    if line_number == 0 || line_number > lines.len() {
        return Err(super::HashlineError::new(
            "E_RANGE_OOB",
            format!(
                "Cannot format line {line_number} for {} visible lines.",
                lines.len()
            ),
        ));
    }
    let Some(line) = lines.get(line_number - 1) else {
        return Err(super::HashlineError::new(
            "E_RANGE_OOB",
            format!(
                "Cannot format line {line_number} for {} visible lines.",
                lines.len()
            ),
        ));
    };
    let hash = compute_line_hash_with_width(lines, line_number - 1, hash_width)?;
    Ok(format!(
        "{:>line_number_width$}#{hash}:{}",
        line_number,
        line.as_ref(),
        line_number_width = line_number_width
    ))
}

/// Format an inclusive region as `LINE#HASH:content` using full-file neighbors.
/// Only the decimal line-number column is padded.
pub(super) fn format_hashline_region_with_width<L: AsRef<str>>(
    lines: &[L],
    start_line: usize,
    end_line: usize,
    width: usize,
) -> Result<String, super::HashlineError> {
    if !valid_hash_length(width) {
        return Err(super::HashlineError::new(
            "E_BAD_CONFIG",
            format!(
                "Hash length must be between {HASH_LENGTH_MIN} and {HASH_LENGTH_MAX}, got {width}."
            ),
        ));
    }
    if start_line == 0 || end_line < start_line || end_line > lines.len() {
        return Err(super::HashlineError::new(
            "E_RANGE_OOB",
            format!(
                "Cannot format line range {start_line}-{end_line} for {} visible lines.",
                lines.len()
            ),
        ));
    }
    let line_number_width = end_line.to_string().len();
    let mut output = String::new();
    for line_number in start_line..=end_line {
        if line_number > start_line {
            output.push('\n');
        }
        output.push_str(&format_hashline_line_with_width(
            lines,
            line_number,
            width,
            line_number_width,
        )?);
    }
    Ok(output)
}

/// Compute the first and last changed visible line in the result document.
#[must_use]
pub(super) fn compute_changed_line_range(original: &str, result: &str) -> Option<(usize, usize)> {
    if original == result {
        return None;
    }

    let count_visible_lines = |text: &str| -> usize {
        if text.is_empty() {
            return 0;
        }
        let count = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        if text.ends_with('\n') {
            count.saturating_sub(1)
        } else {
            count
        }
    };

    if original.is_empty() {
        return Some((1, count_visible_lines(result)));
    }

    if result.starts_with(original) && original.ends_with('\n') {
        return Some((
            count_visible_lines(original) + 1,
            count_visible_lines(result),
        ));
    }

    let first_diff = original
        .as_bytes()
        .iter()
        .zip(result.as_bytes())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| original.len().min(result.len()));

    let mut last_original = original.len();
    let mut last_result = result.len();
    while last_original > first_diff
        && last_result > first_diff
        && original.as_bytes()[last_original - 1] == result.as_bytes()[last_result - 1]
    {
        last_original -= 1;
        last_result -= 1;
    }

    let index_to_line = |index: usize, text: &str| -> usize {
        text.as_bytes()
            .iter()
            .take(index.min(text.len()))
            .filter(|byte| **byte == b'\n')
            .count()
            + 1
    };

    let first_changed_line = index_to_line(first_diff + 1, result);
    let last_changed_line = if last_result <= first_diff {
        if result.is_empty() {
            1
        } else {
            count_visible_lines(result)
        }
    } else if first_diff == 0 && result.ends_with(original) {
        first_changed_line
    } else {
        index_to_line(last_result, result)
    };

    Some((first_changed_line, last_changed_line))
}

/// Return whether a parsed anchor hash uses only the configured alphabet.
#[must_use]
fn valid_hash_characters(hash: &str) -> bool {
    hash.chars().all(|character| NIBBLE_STR.contains(character))
}

/// Reduce an untrusted anchor to a bounded structural label without retaining
/// copied line-hint content in an error payload.
fn bounded_reference(reference: &str) -> String {
    let mut core = reference.trim_start();
    while core
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '>' | '+' | '-'))
    {
        let marker_len = core.chars().next().map_or(0, char::len_utf8);
        core = &core[marker_len..];
        core = core.trim_start();
    }
    let Some((line, hash_part)) = core.split_once('#') else {
        return "<invalid reference>".to_string();
    };
    let line = line.trim();
    if line.is_empty() || !line.chars().all(|character| character.is_ascii_digit()) {
        return "<invalid reference>".to_string();
    }
    let bounded_line = line.chars().take(32).collect::<String>();
    let hash = hash_part
        .trim_start()
        .split(|character: char| character.is_whitespace() || character == ':')
        .next()
        .unwrap_or_default();
    if hash.is_empty() {
        return format!("{bounded_line}#<missing>");
    }
    let bounded_hash = hash.chars().take(HASH_LENGTH_MAX + 1).collect::<String>();
    format!("{bounded_line}#{bounded_hash}")
}

/// Build the detailed parser diagnostic used for malformed references.
fn diagnose_anchor(reference: &str, width: usize) -> String {
    let bounded = bounded_reference(reference);
    let mut core = reference.trim_start();
    while core
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '>' | '+' | '-'))
    {
        let marker_len = core.chars().next().map_or(0, char::len_utf8);
        core = &core[marker_len..];
        core = core.trim_start();
    }
    core = core.trim();

    if core.is_empty() {
        return format!(
            "[E_BAD_REF] Invalid line reference \"{bounded}\". Expected \"LINE#HASH\" (e.g. \"{}\").",
            example_anchor(width)
        );
    }
    if core.chars().all(|character| character.is_ascii_digit()) {
        return format!(
            "[E_BAD_REF] Invalid line reference \"{bounded}\": missing hash, use \"LINE#HASH\" from read output (e.g. \"{}\").",
            example_anchor(width)
        );
    }
    if let Some(index) = core.find(':')
        && core[..index]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return format!(
            "[E_BAD_REF] Invalid line reference \"{bounded}\": wrong separator, use \"LINE#HASH\" instead of \"LINE:...\"."
        );
    }
    if let Some((line, after_hash)) = core.split_once('#') {
        let line = line.trim();
        let hash = after_hash
            .trim_start()
            .split(|character: char| character.is_whitespace() || character == ':')
            .next()
            .unwrap_or_default();
        if line.chars().all(|character| character.is_ascii_digit()) {
            if line == "0" || line.chars().all(|character| character == '0') {
                return format!("[E_BAD_REF] Line number must be >= 1, got 0 in \"{bounded}\".");
            }
            if hash.is_empty() {
                return format!(
                    "[E_BAD_REF] Invalid line reference \"{bounded}\": missing hash after \"#\", use \"LINE#HASH\" from read output."
                );
            }
            if hash.len() != width {
                if valid_hash_characters(hash)
                    && (HASH_LENGTH_MIN..=HASH_LENGTH_MAX).contains(&hash.len())
                {
                    return format!(
                        "[E_BAD_REF] Invalid line reference \"{bounded}\": hash length is {width} in this session, but this anchor has {} characters — it looks like an anchor from a stale context or a different configuration. Re-read the file to get current anchors.",
                        hash.len()
                    );
                }
                return format!(
                    "[E_BAD_REF] Invalid line reference \"{bounded}\": hash must be exactly {width} characters from {NIBBLE_STR} (e.g. \"{}\").",
                    example_anchor(width)
                );
            }
            if !valid_hash_characters(hash) {
                return format!(
                    "[E_BAD_REF] Invalid line reference \"{bounded}\": hash uses invalid characters, hashes use alphabet {NIBBLE_STR} only."
                );
            }
        }
    }
    if core.starts_with('0') && core.contains('#') {
        return format!("[E_BAD_REF] Line number must be >= 1, got 0 in \"{bounded}\".");
    }
    format!(
        "[E_BAD_REF] Invalid line reference \"{bounded}\". Expected \"LINE#HASH\" (e.g. \"{}\").",
        example_anchor(width)
    )
}

/// Parse a strict line anchor while tolerating display/diff marker prefixes.
/// The optional text after the first colon is retained verbatim for hint checks.
pub(super) fn parse_anchor(reference: &str, width: usize) -> Result<Anchor, super::HashlineError> {
    if !valid_hash_length(width) {
        return Err(super::HashlineError::new(
            "E_BAD_CONFIG",
            format!(
                "Hash length must be between {HASH_LENGTH_MIN} and {HASH_LENGTH_MAX}, got {width}."
            ),
        ));
    }
    let bounded = bounded_reference(reference);
    let mut core = reference.trim_start();
    while core
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '>' | '+' | '-'))
    {
        let marker_len = core.chars().next().map_or(0, char::len_utf8);
        core = &core[marker_len..];
        core = core.trim_start();
    }
    let core = core.trim_end();
    let Some((line_part, hash_part)) = core.split_once('#') else {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            diagnose_anchor(reference, width),
        ));
    };
    let line_part = line_part.trim();
    if line_part.is_empty()
        || !line_part
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            diagnose_anchor(reference, width),
        ));
    }
    let line = line_part
        .parse::<usize>()
        .map_err(|_| super::HashlineError::new("E_BAD_REF", diagnose_anchor(reference, width)))?;
    if line == 0 {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            format!("Line number must be >= 1, got 0 in \"{bounded}\"."),
        ));
    }

    let hash_part = hash_part.trim_start();
    let hash_end = hash_part
        .find(|character: char| character.is_whitespace() || character == ':')
        .unwrap_or(hash_part.len());
    let hash = &hash_part[..hash_end];
    if hash.is_empty() {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            format!(
                "Invalid line reference \"{bounded}\": missing hash after \"#\", use \"LINE#HASH\" from read output."
            ),
        ));
    }
    if hash.len() != width {
        let message = if valid_hash_characters(hash)
            && (HASH_LENGTH_MIN..=HASH_LENGTH_MAX).contains(&hash.len())
        {
            format!(
                "Invalid line reference \"{bounded}\": hash length is {width} in this session, but this anchor has {} characters — it looks like an anchor from a stale context or a different configuration. Re-read the file to get current anchors.",
                hash.len()
            )
        } else {
            format!(
                "Invalid line reference \"{bounded}\": hash must be exactly {width} characters from {NIBBLE_STR} (e.g. \"{}\").",
                example_anchor(width)
            )
        };
        return Err(super::HashlineError::new("E_BAD_REF", message));
    }
    if !valid_hash_characters(hash) {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            format!(
                "Invalid line reference \"{bounded}\": hash uses invalid characters, hashes use alphabet {NIBBLE_STR} only."
            ),
        ));
    }

    let remainder = &hash_part[hash_end..];
    let remainder = remainder.trim_start();
    let text_hint = remainder.strip_prefix(':').map(str::to_owned);
    if text_hint.is_none() && !remainder.is_empty() {
        return Err(super::HashlineError::new(
            "E_BAD_REF",
            diagnose_anchor(reference, width),
        ));
    }

    Ok(Anchor {
        line,
        hash: hash.to_owned(),
        text_hint,
    })
}

/// Normalize a copied hint using quote, dash, and Unicode-space forgiveness.
#[must_use]
pub(super) fn normalize_fuzzy_line(text: &str) -> String {
    trim_ecmascript_end(text)
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Compare two lines with limited Unicode and whitespace forgiveness.
#[must_use]
pub(super) fn is_fuzzy_equivalent_line(expected: &str, actual: &str) -> bool {
    normalize_fuzzy_line(expected) == normalize_fuzzy_line(actual)
}

/// Return the first ASCII or Unicode ellipsis marker in a hint.
fn ellipsis_index(text: &str) -> Option<usize> {
    let ascii = text.find("...");
    let unicode = text.find('…');
    match (ascii, unicode) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

/// Match a full or ellipsis-truncated hint against an actual line.
#[must_use]
pub(super) fn hint_matches_line(hint: &str, line: &str) -> bool {
    let normalized_hint = normalize_fuzzy_line(hint);
    let normalized_line = normalize_fuzzy_line(line);
    let Some(index) = ellipsis_index(&normalized_hint) else {
        return normalized_hint == normalized_line;
    };
    normalized_line.starts_with(&normalized_hint[..index])
}

/// Return whether a hint contains non-empty content before its first ellipsis.
#[must_use]
pub(super) fn hint_has_signal(hint: &str) -> bool {
    let normalized_hint = normalize_fuzzy_line(hint);
    let prefix_end = ellipsis_index(&normalized_hint).unwrap_or(normalized_hint.len());
    !normalized_hint[..prefix_end].is_empty()
}

/// Return whether a line begins with a bare configured-width hash prefix.
#[must_use]
pub(super) fn bare_hash_prefix(line: &str, width: usize) -> Option<&str> {
    if !valid_hash_length(width) {
        return None;
    }
    let line = line.trim_start();
    let hash_end = line.find(':')?;
    let candidate = &line[..hash_end];
    if candidate.len() == width && valid_hash_characters(candidate) {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn visible_lines_excludes_only_terminal_sentinel() {
        assert_eq!(visible_lines(""), Vec::<String>::new());
        assert_eq!(visible_lines("a\n"), vec!["a"]);
        assert_eq!(visible_lines("a\n\n"), vec!["a", ""]);
        assert_eq!(visible_lines("\n"), vec![""]);
    }

    #[test]
    fn parser_accepts_display_markers_and_hints() {
        let anchor = parse_anchor(" >>> 2#JB:beta  ", 2).unwrap();
        assert_eq!(anchor.line, 2);
        assert_eq!(anchor.hash, "JB");
        assert_eq!(anchor.text_hint.as_deref(), Some("beta"));
    }

    #[test]
    fn parser_rejects_wrong_width_and_separator() {
        let width = parse_anchor("2#JBY", 2).unwrap_err();
        assert_eq!(width.code, "E_BAD_REF");
        let separator = parse_anchor("2: beta", 2).unwrap_err();
        assert!(separator.message.contains("wrong separator"));
    }
}
