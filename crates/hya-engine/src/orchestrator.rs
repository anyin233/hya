//! Bounded subagent orchestration primitives (re-exported from `hya-multi-agent`).
//!
//! [`SubagentGovernor`] bounds concurrent provider streams. The store enforces
//! live-member capacity with active claims; team activity counters belong to
//! the resident team.

pub use hya_multi_agent::governor::{SubagentGovernor, SubagentLimits};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// ADR-0015: exactly two subagent layers, engine-owned and not
    /// configurable — no limits field can raise it.
    #[test]
    fn subagent_depth_is_hardcoded_to_two_layers() {
        assert_eq!(crate::MAX_SUBAGENT_DEPTH, 2);
        assert_eq!(
            SubagentGovernor::new(SubagentLimits::default()).max_depth(),
            2
        );
        assert_eq!(
            SubagentGovernor::new(SubagentLimits {
                max_concurrency: 100,
                per_run_budget: 1024,
                ..SubagentLimits::default()
            })
            .max_depth(),
            2
        );
    }
}
