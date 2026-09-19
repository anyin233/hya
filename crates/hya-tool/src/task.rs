use async_trait::async_trait;
use hya_proto::ToolSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::permission::{Action, Resource};
use crate::spawn::{InlineAgent, SpawnError, SpawnMember};
use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

pub struct TaskTool;

/// Request-scoped inline agent overlay for a `task` call. Applies only to this
/// request/child spawn and is not retained as a reusable agent definition.
#[derive(Deserialize)]
struct InlineAgentInput {
    #[serde(default)]
    name: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

impl InlineAgentInput {
    /// Convert to the runtime [`InlineAgent`], defaulting the name to the caller's
    /// `subagent_type` when the inline block omits one.
    fn into_inline(self, subagent_type: &str) -> InlineAgent {
        let name = if self.name.trim().is_empty() {
            subagent_type.to_string()
        } else {
            self.name
        };
        InlineAgent {
            name,
            prompt: self.prompt,
            description: self.description.filter(|value| !value.trim().is_empty()),
            category: self.category,
            model: self.model,
        }
    }
}

#[derive(Deserialize)]
struct TaskMemberInput {
    #[serde(default)]
    description: String,
    prompt: String,
    #[serde(default)]
    subagent_type: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    inline_agent: Option<InlineAgentInput>,
}

#[derive(Deserialize)]
struct TaskInput {
    #[serde(default)]
    description: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    subagent_type: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    inline_agent: Option<InlineAgentInput>,
    #[serde(default)]
    members: Vec<TaskMemberInput>,
}

struct TaskResult {
    title: String,
    parent_session: String,
    session: String,
    subagent_type: String,
    status: String,
    summary: String,
    command: Option<String>,
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "task",
            "Launch a specialized subagent (ADR-0015 episodic actor). Non-blocking: returns immediately with the agent's handle; results arrive later as its `report` mail. Continue the conversation, then check mail or wait for the report. Follow up on a finished agent by sending mail to its handle; archived agents are revived by that mail.",
            json!({
                "description": {
                    "type": "string",
                    "description": "A short (3-5 words) description of the task"
                },
                "prompt": {
                    "type": "string",
                    "description": "The task for the agent to perform"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "The type of specialized agent to use for this task"
                },
                "category": {
                    "type": "string",
                    "description": "Override the agent's logical model category (e.g. quick, deep) for this spawn; resolves to a concrete provider/model with failover"
                },
                "model": {
                    "type": "string",
                    "description": "Override the concrete provider/model for this spawn; wins over category and the agent's own model"
                },
                "command": {
                    "type": "string",
                    "description": "The command that triggered this task"
                },
                "inline_agent": {
                    "type": "object",
                    "description": "Request-scoped agent overlay for this spawn only. Supplies its own system prompt and name for the child and folds into the same model/category precedence chain; not retained for later reuse as an agent definition.",
                    "properties": {
                        "name": { "type": "string", "description": "Agent name (defaults to subagent_type when omitted)" },
                        "prompt": { "type": "string", "description": "The system prompt / persona for the request-scoped overlay" },
                        "category": { "type": "string", "description": "Logical model category (request overlay; folds into spawn model precedence)" },
                        "model": { "type": "string", "description": "Concrete provider/model (request overlay; folds into spawn model precedence)" },
                        "resident": { "type": "boolean", "description": "Make this request-scoped overlay a resident actor" }
                    }
                },
                "members": {
                    "type": "array",
                    "description": "hya extension: dispatch several members in one tool call",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "prompt": { "type": "string" },
                            "subagent_type": { "type": "string" },
                            "category": { "type": "string" },
                            "model": { "type": "string" },
                            "inline_agent": {
                                "type": "object",
                                "description": "Request-scoped agent overlay for this member spawn only. Supplies its own system prompt and name for the child and folds into the same model/category precedence chain; not retained for later reuse as an agent definition.",
                                "properties": {
                                    "name": { "type": "string" },
                                    "prompt": { "type": "string" },
                                    "category": { "type": "string" },
                                    "model": { "type": "string" },
                                    "resident": { "type": "boolean" }
                                }
                            }
                        },
                        "required": ["prompt"]
                    }
                }
            }),
            &["description", "prompt"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        // Nested subagents are allowed: a subagent may call `task` to spawn its own
        // subagents. Recursion depth and total fan-out are bounded by the engine's
        // SubagentGovernor (max_depth + per-run budget), enforced in `run_team`, so
        // there is no hard one-level cap here.
        let input: TaskInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let parent_session = ctx
            .session
            .ok_or_else(|| ToolError::Other("task tool requires a session".to_string()))?
            .to_string();

        let mut members: Vec<SpawnMember> = input
            .members
            .into_iter()
            .map(|m| {
                let subagent_type = normalized_agent_target(&m.subagent_type);
                let inline_agent = m
                    .inline_agent
                    .map(|inline| inline.into_inline(&subagent_type));
                SpawnMember {
                    description: m.description,
                    prompt: m.prompt,
                    subagent_type,
                    model: m.model,
                    category: m.category,
                    inline_agent,
                }
            })
            .collect();
        if members.is_empty() {
            if input.description.trim().is_empty() || input.prompt.trim().is_empty() {
                return Err(ToolError::Input(
                    "provide description and prompt".to_string(),
                ));
            }
            let subagent_type = normalized_agent_target(&input.subagent_type);
            let inline_agent = input
                .inline_agent
                .map(|inline| inline.into_inline(&subagent_type));
            members.push(SpawnMember {
                description: input.description,
                prompt: input.prompt,
                subagent_type,
                model: input.model,
                category: input.category,
                inline_agent,
            });
        }

        for member in &members {
            ctx.permission
                .assert(
                    Action::Task,
                    Resource::Subagent(member.subagent_type.clone()),
                )
                .await?;
        }

        let outcomes = ctx
            .spawner
            .spawn(ctx.operation, members.clone(), ctx.cancel.clone())
            .await
            .map_err(|error| match error {
                SpawnError::Overloaded => ToolError::Overloaded(error.to_string()),
                SpawnError::Unavailable => ToolError::Other(error.to_string()),
                SpawnError::Cancelled => ToolError::Other(error.to_string()),
                SpawnError::OperationIdConflict => ToolError::OperationIdConflict,
                SpawnError::OperationAlreadyHandled => ToolError::OperationAlreadyHandled,
                SpawnError::UnknownAgentId { agent_id } => ToolError::UnknownAgentId { agent_id },
                SpawnError::AgentSpawnNotAllowed { caller, agent_id } => {
                    ToolError::AgentSpawnNotAllowed { caller, agent_id }
                }
                SpawnError::UnsupportedInlineAgentField { field } => {
                    ToolError::UnsupportedInlineAgentField { field }
                }
            })?;
        if members.len() == 1 && outcomes.len() == 1 {
            let member = members.remove(0);
            let Some(outcome) = outcomes.into_iter().next() else {
                return Err(ToolError::Other(
                    "task spawner returned no outcome".to_string(),
                ));
            };
            return Ok(render_single(TaskResult {
                title: member.description,
                parent_session,
                session: outcome.session,
                subagent_type: member.subagent_type,
                status: outcome.status,
                summary: outcome.summary,
                command: input.command,
            }));
        }

        // Pair outcomes with the original member specs so the TUI can show every
        // launched subagent (type + short description + session) in the main
        // message, matching OpenCode's multi-task rows.
        let members_json: Vec<Value> = outcomes
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                let member = members.get(i);
                json!({
                    "member": o.member,
                    "session": o.session,
                    "sessionId": o.session,
                    "status": o.status,
                    "summary": o.summary,
                    "description": member.map(|m| m.description.as_str()).unwrap_or(""),
                    "subagent_type": member.map(|m| m.subagent_type.as_str()).unwrap_or(""),
                })
            })
            .collect();
        let title = format!(
            "{} subagent{}",
            members_json.len(),
            if members_json.len() == 1 { "" } else { "s" }
        );
        Ok(json!({
            "title": title,
            "metadata": {
                "parentSessionId": parent_session,
                "members": members_json,
            },
            "output": format!(
                "<task state=\"completed\">\n<task_result>\n{} members finished\n</task_result>\n</task>",
                members_json.len()
            ),
        }))
    }
}

fn normalized_agent_target(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "general".to_string()
    } else {
        value.to_string()
    }
}

/// Coerce model-supplied `task_id` placeholders into "create fresh".
///
/// Empty / whitespace and common sentinels (`new`, `null`, `none`, `undefined`)
/// become `None`. Non-empty real session ids are kept for resume validation.
// Pending re-wiring in the in-flight task tool refactor; kept for the
// follow-up commit that consumes it.
#[allow(dead_code)]
fn normalize_task_id(raw: Option<String>) -> Option<String> {
    let s = raw?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    match s.to_ascii_lowercase().as_str() {
        "new" | "null" | "none" | "undefined" => None,
        _ => Some(s),
    }
}

fn render_single(result: TaskResult) -> Value {
    let state = if result.status == "done" || result.status == "completed" {
        "completed"
    } else if result.status == "running" {
        "running"
    } else {
        "error"
    };
    let tag = if state == "error" {
        "task_error"
    } else {
        "task_result"
    };
    let mut metadata = Map::from_iter([
        (
            "parentSessionId".to_string(),
            json!(result.parent_session.clone()),
        ),
        ("sessionId".to_string(), json!(result.session.clone())),
        (
            "subagent_type".to_string(),
            json!(result.subagent_type.clone()),
        ),
        ("status".to_string(), json!(result.status.clone())),
    ]);
    if let Some(command) = result.command {
        metadata.insert("command".to_string(), json!(command));
    }
    json!({
        "title": result.title,
        "metadata": metadata,
        "output": format!(
            "<task id=\"{}\" state=\"{}\">\n<{}>\n{}\n</{}>\n</task>",
            result.session, state, tag, result.summary, tag
        ),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::normalize_task_id;

    #[test]
    fn normalize_task_id_treats_empty_and_sentinels_as_new() {
        assert_eq!(normalize_task_id(None), None);
        assert_eq!(normalize_task_id(Some(String::new())), None);
        assert_eq!(normalize_task_id(Some("   ".into())), None);
        assert_eq!(normalize_task_id(Some("new".into())), None);
        assert_eq!(normalize_task_id(Some("NEW".into())), None);
        assert_eq!(normalize_task_id(Some("null".into())), None);
        assert_eq!(normalize_task_id(Some("none".into())), None);
        assert_eq!(normalize_task_id(Some("undefined".into())), None);
    }

    #[test]
    fn normalize_task_id_keeps_real_ids() {
        assert_eq!(
            normalize_task_id(Some("hysec_00000000000000000000".into())).as_deref(),
            Some("hysec_00000000000000000000")
        );
        assert_eq!(
            normalize_task_id(Some("  hysec_abc ".into())).as_deref(),
            Some("hysec_abc")
        );
    }
}
