//! Spawnable agent definition exposed by the tool interface.

/// A single spawnable agent definition surfaced to the model by `list_agents`.
#[derive(Clone, Debug)]
pub struct AgentDef {
    /// The `subagent_type` value to pass to the `task` tool.
    pub name: String,
    /// Optional human-readable description for the model listing.
    pub description: Option<String>,
    /// Logical model category from the bound Bundle definition, if any.
    pub category: Option<String>,
    /// Agent mode, e.g. `primary`, `subagent`, `all`.
    pub mode: String,
    /// The Agent's default thinking effort label; `None` means the child
    /// runs at its model's default effort.
    pub effort: Option<String>,
    /// Which layer chose `effort`: `preference` (set by the user at runtime),
    /// `configured` (`agents.<id>.reasoning`), or `authored` (bundle policy).
    pub effort_source: Option<String>,
}
