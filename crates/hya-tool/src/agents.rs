//! Model-facing agent discovery: the `list_agents` tool + the plane that feeds
//! it the team's agent catalog.
//!
//! The agent catalog lives in `hya-server`, which depends on `hya-tool` (not the
//! other way round). To let a tool enumerate agents without a circular
//! dependency, the app/runtime layer injects a closure via [`AgentCatalogPlane`]
//! — exactly how [`crate::skill::SkillPlane`] / [`crate::spawn::SpawnerPlane`]
//! are injected into [`ToolCtx`]. `hya-tool` never names the catalog type; it
//! only knows the flattened [`AgentDef`] shape.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError};

/// A single spawnable agent definition surfaced to the model by `list_agents`.
#[derive(Clone, Debug)]
pub struct AgentDef {
    /// The `subagent_type` value to pass to the `task` tool.
    pub name: String,
    pub description: Option<String>,
    /// Logical model category (frontmatter `category:`), if any.
    pub category: Option<String>,
    /// Agent mode, e.g. `primary`, `subagent`, `all`.
    pub mode: String,
}

type CatalogSource = Arc<dyn Fn(&Path) -> Vec<AgentDef> + Send + Sync>;

/// Injected access to the workdir-scoped agent catalog. Default yields an empty
/// list so tests/engines without a wired catalog degrade gracefully.
#[derive(Clone)]
pub struct AgentCatalogPlane {
    source: CatalogSource,
}

impl Default for AgentCatalogPlane {
    fn default() -> Self {
        Self {
            source: Arc::new(|_| Vec::new()),
        }
    }
}

impl AgentCatalogPlane {
    /// Wire a catalog resolver. The closure is called with the active workdir and
    /// returns the agent definitions visible there (already filtered to the ones
    /// worth showing the model, i.e. non-hidden).
    #[must_use]
    pub fn new<F>(source: F) -> Self
    where
        F: Fn(&Path) -> Vec<AgentDef> + Send + Sync + 'static,
    {
        Self {
            source: Arc::new(source),
        }
    }

    /// Enumerate the agent definitions available in `workdir`.
    #[must_use]
    pub fn list(&self, workdir: &Path) -> Vec<AgentDef> {
        (self.source)(workdir)
    }
}

pub(crate) struct ListAgentsTool;

#[async_trait]
impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("list_agents"),
            description: "List the agent definitions available to spawn via the `task` tool. Returns each agent's name (the `subagent_type` to pass to `task`), description, logical model category, and mode. Call this to discover which subagent types exist before spawning one.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let mut agents = ctx.agents.list(&ctx.workdir);
        agents.sort_by(|left, right| left.name.cmp(&right.name));
        let rows: Vec<Value> = agents
            .iter()
            .map(|agent| {
                json!({
                    "name": agent.name,
                    "description": agent.description,
                    "category": agent.category,
                    "mode": agent.mode,
                })
            })
            .collect();
        let output = if agents.is_empty() {
            "No agents available.".to_string()
        } else {
            agents
                .iter()
                .map(|agent| {
                    let description = agent.description.as_deref().unwrap_or("");
                    let category = agent
                        .category
                        .as_deref()
                        .map(|category| format!(" [category: {category}]"))
                        .unwrap_or_default();
                    format!(
                        "- {} ({}){}: {}",
                        agent.name, agent.mode, category, description
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(json!({
            "title": format!("{} agents available", agents.len()),
            "output": output,
            "agents": rows,
            "metadata": { "count": agents.len() },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_plane_lists_nothing() {
        let plane = AgentCatalogPlane::default();
        assert!(plane.list(&PathBuf::from("/tmp")).is_empty());
    }

    #[test]
    fn injected_closure_is_workdir_scoped() {
        // The plane forwards the active workdir to the injected resolver, mirroring
        // how the app layer wires `hya_server::agent_definitions`.
        let plane = AgentCatalogPlane::new(|workdir: &Path| {
            vec![AgentDef {
                name: workdir.to_string_lossy().into_owned(),
                description: Some("d".to_string()),
                category: Some("deep".to_string()),
                mode: "subagent".to_string(),
            }]
        });
        let defs = plane.list(&PathBuf::from("/work/here"));
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "/work/here");
        assert_eq!(defs[0].category.as_deref(), Some("deep"));
    }
}
