//! Model-facing agent discovery over the immutable per-turn spawn roster.

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
    /// Logical model category from the bound Bundle definition, if any.
    pub category: Option<String>,
    /// Agent mode, e.g. `primary`, `subagent`, `all`.
    pub mode: String,
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
        let mut agents = ctx.agents.to_vec();
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
