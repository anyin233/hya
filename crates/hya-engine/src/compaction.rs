//! Combined compaction settings and compatibility exports for terminal handoff.

use crate::terminal_handoff::TerminalHandoffOptions;

pub use crate::terminal_handoff::{ModelSummarizer, SummarizeOptions, Summarizer, UsageCollector};

/// Compaction settings. `keep_recent` goes to the `hya/compaction` bundle as
/// JSON; `summary_max_tokens` caps the terminal handoff.
#[derive(Clone, Copy, Debug)]
pub struct CompactionConfig {
    /// Number of recent messages whose tool outputs are kept.
    pub keep_recent: usize,
    /// Output cap for the summarizer call that writes the terminal handoff.
    pub summary_max_tokens: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            keep_recent: 6,
            summary_max_tokens: TerminalHandoffOptions::default().max_output_tokens,
        }
    }
}
