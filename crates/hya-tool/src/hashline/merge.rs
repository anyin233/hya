//! Exact stale-edit recovery using context-three, fuzz-zero hunks.
//!
//! This seam mirrors the recovery behavior required by `pi-hashline-edit` 0.8.3
//! (`ba7db9943d0f58499b24c1f6bd64722580f772a5`, tarball SHA-1
//! `8985f24c3493be375cc225a5522ed54de8daabc9`). The MIT-licensed upstream
//! behavior is intentionally conservative here: a hunk is accepted only at its
//! exact expected line after earlier hunks have been replayed. No search or
//! fuzzy relocation is performed.

use std::fmt;

use similar::{DiffOp, TextDiff};

const CONTEXT_LINES: usize = 3;

/// A typed failure from exact stale-content recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MergeError {
    /// A context hunk did not match at its exact expected live position.
    ContextConflict {
        /// Zero-based hunk order in the historical patch.
        hunk: usize,
        /// One-based expected line used for the failed exact match.
        line: usize,
    },
    /// The live and historical files disagree on terminal-newline state.
    TerminalNewlineConflict,
    #[cfg(test)]
    /// No historical version was available for recovery.
    NoHistory,
}

impl fmt::Display for MergeError {
    /// Render a bounded, content-free recovery diagnostic.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContextConflict { hunk, line } => write!(
                formatter,
                "stale edit recovery conflict in context hunk {hunk} at line {line}"
            ),
            Self::TerminalNewlineConflict => {
                write!(
                    formatter,
                    "stale edit recovery conflict at terminal newline"
                )
            }
            #[cfg(test)]
            Self::NoHistory => write!(
                formatter,
                "no historical snapshot is available for recovery"
            ),
        }
    }
}

impl std::error::Error for MergeError {}

/// Replay one historical edit onto live text with exact context-three matching.
///
/// `base` is the historical snapshot on which the edit was resolved, `desired`
/// is the resulting historical text, and `live` is the current file. Changes
/// outside each three-line context window are retained from `live`; a mismatch
/// at any expected hunk position returns [`MergeError::ContextConflict`].
pub(super) fn merge(base: &str, desired: &str, live: &str) -> Result<String, MergeError> {
    // A live file equal to the historical base is the direct, non-stale path.
    if base == live {
        return Ok(desired.to_string());
    }
    // A no-change historical candidate cannot recover a different live file.
    if base == desired {
        return Err(MergeError::ContextConflict { hunk: 0, line: 1 });
    }

    let base_terminal = has_terminal_newline(base);
    let desired_terminal = has_terminal_newline(desired);
    let live_terminal = has_terminal_newline(live);
    let base_lines = split_lines(base);
    let desired_lines = split_lines(desired);
    let live_lines = split_lines(live);
    let changes = change_groups(
        &base_lines,
        &desired_lines,
        base_terminal != desired_terminal,
    );
    let hunks = build_hunks(&base_lines, &desired_lines, &changes);

    // A completely empty base has no context with which to distinguish a new
    // insertion from an unrelated live file. Fail closed instead of relocating.
    if base_lines.is_empty() && !live_lines.is_empty() {
        return Err(MergeError::ContextConflict { hunk: 0, line: 1 });
    }

    // Terminal-newline state is part of an EOF hunk, not a global file precondition.
    // A middle hunk must leave an unrelated live EOF change untouched.
    let eof_hunk = hunks.iter().any(|hunk| hunk.old_end == base_lines.len());
    if eof_hunk && live_terminal != base_terminal {
        return Err(MergeError::TerminalNewlineConflict);
    }
    assemble_merge(
        &base_lines,
        &desired_lines,
        &live_lines,
        &hunks,
        live.len().saturating_add(desired.len()),
        if eof_hunk {
            desired_terminal
        } else {
            live_terminal
        },
    )
}

#[cfg(test)]
/// Try newest-first historical `(base, desired)` pairs until one exact merge wins.
///
/// The iterator must already be ordered newest-first by the caller's bounded
/// snapshot state. Every failed candidate is discarded without altering `live`.
pub(super) fn recover<'a, I>(historical: I, live: &str) -> Result<String, MergeError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut last_error = None;
    for (base, desired) in historical {
        match merge(base, desired, live) {
            Ok(merged) => return Ok(merged),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) => Err(error),
        None => Err(MergeError::NoHistory),
    }
}

/// One contiguous changed region in the historical line diff.
#[derive(Debug, Eq, PartialEq)]
struct ChangeGroup {
    /// First changed base line, zero based.
    old_start: usize,
    /// Exclusive end of changed base lines.
    old_end: usize,
    /// First changed desired line, zero based.
    new_start: usize,
    /// Exclusive end of changed desired lines.
    new_end: usize,
}

impl ChangeGroup {
    /// Return the number of base lines replaced by this group.
    fn old_len(&self) -> usize {
        self.old_end.saturating_sub(self.old_start)
    }

    /// Return the number of desired lines introduced by this group.
    fn new_len(&self) -> usize {
        self.new_end.saturating_sub(self.new_start)
    }
}

/// A context-three replacement hunk represented only by borrowed ranges.
#[derive(Debug, Eq, PartialEq)]
struct Hunk {
    /// First old line in the context-bearing hunk.
    old_start: usize,
    /// Exclusive old range end.
    old_end: usize,
    /// First desired line in the context-bearing hunk.
    new_start: usize,
    /// Exclusive desired range end.
    new_end: usize,
}

/// Split normalized text into borrowed visible lines without a terminal sentinel.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Return whether text contains a final LF terminator.
fn has_terminal_newline(text: &str) -> bool {
    text.ends_with('\n')
}

/// Group non-equal diff operations and add an EOF group for newline changes.
fn change_groups(base: &[&str], desired: &[&str], terminal_changed: bool) -> Vec<ChangeGroup> {
    let diff = TextDiff::from_slices(base, desired);
    let mut groups: Vec<ChangeGroup> = Vec::new();
    for operation in diff.ops() {
        if matches!(operation, DiffOp::Equal { .. }) {
            continue;
        }
        let old_range = operation.old_range();
        let new_range = operation.new_range();
        if let Some(previous) = groups.last_mut()
            && old_range.start <= previous.old_end.saturating_add(CONTEXT_LINES * 2)
        {
            previous.old_end = previous.old_end.max(old_range.end);
            previous.new_end = previous.new_end.max(new_range.end);
            continue;
        }
        groups.push(ChangeGroup {
            old_start: old_range.start,
            old_end: old_range.end,
            new_start: new_range.start,
            new_end: new_range.end,
        });
    }
    if terminal_changed {
        let eof = ChangeGroup {
            old_start: base.len(),
            old_end: base.len(),
            new_start: desired.len(),
            new_end: desired.len(),
        };
        if let Some(previous) = groups.last_mut()
            && eof.old_start <= previous.old_end.saturating_add(CONTEXT_LINES * 2)
        {
            previous.old_end = previous.old_end.max(eof.old_end);
            previous.new_end = previous.new_end.max(eof.new_end);
        } else {
            groups.push(eof);
        }
    }
    groups
}

/// Build context-bearing hunk ranges while retaining exact line offsets.
fn build_hunks(base: &[&str], desired: &[&str], changes: &[ChangeGroup]) -> Vec<Hunk> {
    changes
        .iter()
        .enumerate()
        .map(|(index, change)| {
            let old_start = change.old_start.saturating_sub(CONTEXT_LINES);
            let old_end = change.old_end.saturating_add(CONTEXT_LINES).min(base.len());
            let new_start = map_old_to_new(old_start, changes, index);
            let new_end = map_old_to_new(old_end, changes, index + 1).min(desired.len());
            Hunk {
                old_start,
                old_end,
                new_start,
                new_end,
            }
        })
        .collect()
}

/// Map an old context boundary through preceding historical line changes.
fn map_old_to_new(index: usize, changes: &[ChangeGroup], through: usize) -> usize {
    let mut mapped = index as isize;
    for change in changes.iter().take(through) {
        mapped += change.new_len() as isize - change.old_len() as isize;
    }
    if mapped.is_negative() {
        0
    } else {
        mapped as usize
    }
}

/// Validate exact hunk positions and assemble the final text in one pass.
fn assemble_merge(
    base: &[&str],
    desired: &[&str],
    live: &[&str],
    hunks: &[Hunk],
    capacity: usize,
    terminal_newline: bool,
) -> Result<String, MergeError> {
    let mut output = String::with_capacity(capacity);
    let mut first_line = true;
    let mut cursor = 0usize;
    let mut line_delta = 0isize;

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let expected_signed = hunk.old_start as isize + line_delta;
        let Some(expected) = usize::try_from(expected_signed).ok() else {
            return Err(MergeError::ContextConflict {
                hunk: hunk_index,
                line: hunk.old_start.saturating_add(1),
            });
        };
        let Some(end) = expected.checked_add(hunk.old_end.saturating_sub(hunk.old_start)) else {
            return Err(MergeError::ContextConflict {
                hunk: hunk_index,
                line: expected.saturating_add(1),
            });
        };
        let Some(base_span) = base.get(hunk.old_start..hunk.old_end) else {
            return Err(MergeError::ContextConflict {
                hunk: hunk_index,
                line: hunk.old_start.saturating_add(1),
            });
        };
        let Some(desired_span) = desired.get(hunk.new_start..hunk.new_end) else {
            return Err(MergeError::ContextConflict {
                hunk: hunk_index,
                line: hunk.old_start.saturating_add(1),
            });
        };
        if expected < cursor || end > live.len() || live.get(expected..end) != Some(base_span) {
            return Err(MergeError::ContextConflict {
                hunk: hunk_index,
                line: expected.saturating_add(1),
            });
        }
        for &line in &live[cursor..expected] {
            push_line(&mut output, &mut first_line, line);
        }
        for &line in desired_span {
            push_line(&mut output, &mut first_line, line);
        }
        cursor = end;
        line_delta += hunk.new_end.saturating_sub(hunk.new_start) as isize
            - hunk.old_end.saturating_sub(hunk.old_start) as isize;
    }
    for line in &live[cursor..] {
        push_line(&mut output, &mut first_line, line);
    }
    if terminal_newline {
        output.push('\n');
    }
    Ok(output)
}

/// Append one line with separators while retaining a single output allocation.
fn push_line(output: &mut String, first_line: &mut bool, line: &str) {
    if !*first_line {
        output.push('\n');
    }
    output.push_str(line);
    *first_line = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify a non-conflicting live change outside a context-three hunk is retained.
    #[test]
    fn exact_context_three_merge_preserves_far_live_changes() {
        let base = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
        let desired = "zero\none\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\n";
        let live = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven\nLIVE\n";
        let merged = match merge(base, desired, live) {
            Ok(merged) => merged,
            Err(error) => panic!("exact merge failed: {error}"),
        };
        assert_eq!(
            merged,
            "zero\none\ntwo\nTHREE\nfour\nfive\nsix\nseven\nLIVE\n"
        );
    }

    /// Verify a live insertion that shifts the expected hunk is not fuzzy-relocated.
    #[test]
    fn shifted_live_content_conflicts_without_relocation() {
        let base = "zero\none\ntwo\nthree\nfour\nfive\nsix\n";
        let desired = "zero\none\ntwo\nTHREE\nfour\nfive\nsix\n";
        let live = "INSERTED\nzero\none\ntwo\nthree\nfour\nfive\nsix\n";
        let error = match merge(base, desired, live) {
            Ok(_) => panic!("shifted live content unexpectedly merged"),
            Err(error) => error,
        };
        assert!(matches!(error, MergeError::ContextConflict { .. }));
    }

    /// Verify a changed context line produces a typed conflict instead of an overwrite.
    #[test]
    fn changed_context_conflicts() {
        let base = "zero\none\ntwo\nthree\nfour\nfive\nsix\n";
        let desired = "zero\none\ntwo\nTHREE\nfour\nfive\nsix\n";
        let live = "zero\none\nLIVE\nthree\nfour\nfive\nsix\n";
        let error = match merge(base, desired, live) {
            Ok(_) => panic!("conflicting live content unexpectedly merged"),
            Err(error) => error,
        };
        assert!(matches!(error, MergeError::ContextConflict { .. }));
    }

    /// Verify a no-change historical candidate rejects a different live file.
    #[test]
    fn no_change_recovery_rejects_different_live_content() {
        assert_eq!(
            merge("same", "same", "different"),
            Err(MergeError::ContextConflict { hunk: 0, line: 1 })
        );
    }

    /// Verify a middle hunk preserves a live terminal-newline change outside its context.
    #[test]
    fn middle_hunk_preserves_unrelated_live_eof() {
        let base = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
        let desired = "zero\none\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\n";
        let live = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight";
        let merged = match merge(base, desired, live) {
            Ok(merged) => merged,
            Err(error) => panic!("middle merge failed: {error}"),
        };
        assert_eq!(
            merged,
            "zero\none\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight"
        );
    }

    /// Verify terminal-newline changes are replayed only against matching live state.
    #[test]
    fn terminal_newline_state_is_exact() {
        let merged = match merge("one", "one\n", "one") {
            Ok(merged) => merged,
            Err(error) => panic!("terminal-newline merge failed: {error}"),
        };
        assert_eq!(merged, "one\n");
        assert_eq!(
            merge("one", "one\n", "one\n"),
            Err(MergeError::TerminalNewlineConflict)
        );
    }

    /// Verify recovery tries each bounded historical pair and reports no-history distinctly.
    #[test]
    fn recovery_uses_newest_exact_candidate_then_reports_no_history() {
        let historical = [("base", "new")];
        let merged = match recover(historical.iter().copied(), "base") {
            Ok(merged) => merged,
            Err(error) => panic!("recovery failed: {error}"),
        };
        assert_eq!(merged, "new");
        assert_eq!(
            recover(std::iter::empty::<(&str, &str)>(), "live"),
            Err(MergeError::NoHistory)
        );
    }
}
