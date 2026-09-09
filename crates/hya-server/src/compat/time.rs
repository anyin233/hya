//! Shared time helpers for Compat wire shapes.

/// Compat timestamps are unsigned milliseconds; clamp negative event clocks to 0.
pub(super) fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
