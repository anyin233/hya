//! Archived-agent search tool.
use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{MailboxError, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct SearchAgentTool;

#[derive(Deserialize)]
struct SearchAgentInput {
    #[serde(default)]
    query: String,
}

#[async_trait]
impl Tool for SearchAgentTool {
    fn name(&self) -> &str {
        "search_agent"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "search_agent",
            "Search YOUR OWN archived direct subagents by their final state \
             handoff (goal / current state / pending). Returns each agent's \
             handle and digest; `dm` that handle to revive the agent with its \
             saved state.",
            json!({
                "query": {"type": "string", "description": "Free-text query over goal/state/pending digests; empty lists all"}
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SearchAgentInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let rows = ctx
            .mailbox
            .search_agents(input.query)
            .await
            .map_err(map_err)?;
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| {
                format!(
                    "  {} ({}) · goal: {} · pending: {}{}",
                    row.handle,
                    row.agent_type,
                    row.goal,
                    row.pending,
                    if row.degraded {
                        " · degraded handoff"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        let output = if rendered.is_empty() {
            "No archived agents match.".to_string()
        } else {
            rendered.join("\n")
        };
        Ok(json!({
            "title": format!("{} archived agent(s)", rows.len()),
            "output": output,
            "agents": rows.iter().map(|row| json!({
                "handle": row.handle,
                "agent_type": row.agent_type,
                "session": row.session,
                "goal": row.goal,
                "pending": row.pending,
                "degraded": row.degraded,
            })).collect::<Vec<_>>(),
        }))
    }
}

fn map_err(err: MailboxError) -> ToolError {
    match err {
        MailboxError::Unavailable => ToolError::Other("mailbox service unavailable".to_string()),
        MailboxError::Rejected(message) => ToolError::Input(message),
    }
}
