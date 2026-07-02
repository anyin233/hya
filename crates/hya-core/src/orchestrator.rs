//! Bounded subagent orchestration primitives.
//!
//! [`SubagentLimits`] carries the user-configurable caps that keep nested,
//! massively-parallel subagent fan-out safe: a maximum recursion depth, a global
//! cap on concurrently-*streaming* members, and a per-top-level-run budget on the
//! total number of members that may be spawned. The [`SubagentGovernor`] that
//! enforces these lands with the orchestration workstream; this module defines the
//! limits type so config parsing (`hya-app`) can resolve it independently of the
//! engine.

/// Configurable caps for nested/parallel subagent execution.
///
/// - `max_depth`: how many levels a subagent tree may recurse (the interactive
///   lead session is depth 0; its direct subagents are depth 1, and so on).
/// - `max_concurrency`: global ceiling on members whose provider stream is running
///   at the same time. Excess members park until a slot frees, which is the
///   backpressure that keeps 100+ agents from exhausting resources.
/// - `per_run_budget`: maximum total number of members that may be spawned under a
///   single top-level run, bounding the total task fan-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentLimits {
    pub max_depth: u32,
    pub max_concurrency: usize,
    pub per_run_budget: u64,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_depth: 5,
            max_concurrency: 128,
            per_run_budget: 256,
        }
    }
}
