//! Model-facing multi-agent tools: `list_agents`, `task`, `search_agent`, and
//! `archive` (plus `hya_tool::lifecycle::WaitTool::new(false)` for `wait`).

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{
    Action, AgentDef, InlineAgent, LifecyclePlane, MailboxPlane, Resource, SpawnError, SpawnMember,
    SpawnerPlane, Tool, ToolCtx, ToolError,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// List available agent definitions that the caller can spawn via `task`.
pub struct ListAgentsTool;

#[async_trait]
impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "list_agents",
            "List the agent definitions available to spawn via the `task` tool. Returns each agent's name (the `subagent_type` to pass to `task`), description, logical model category, mode, and default thinking effort (`effort`; none means the model's default). Pass `effort` to `task` to override it for one spawn. Use this when the available roster is unknown, then put independent assignments in one task tasks[] batch; set subagent_type and effort per item.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let mut agents = ctx
            .extension::<Arc<[AgentDef]>>()
            .map(|roster| roster.to_vec())
            .unwrap_or_default();
        agents.sort_by(|left, right| left.name.cmp(&right.name));
        let rows: Vec<Value> = agents
            .iter()
            .map(|agent| {
                json!({
                    "name": agent.name,
                    "description": agent.description,
                    "category": agent.category,
                    "mode": agent.mode,
                    "effort": agent.effort,
                    "effort_source": agent.effort_source,
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
                    let effort = agent
                        .effort
                        .as_deref()
                        .map(|effort| format!(" [effort: {effort}]"))
                        .unwrap_or_default();
                    format!(
                        "- {} ({}){}{}: {}",
                        agent.name, agent.mode, category, effort, description
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

/// Launch one or more subagents concurrently on independent tasks.
pub struct TaskTool;

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
    effort: Option<String>,
    #[serde(default)]
    inline_agent: Option<InlineAgentInput>,
    #[serde(default)]
    name: Option<Value>,
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
    effort: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    inline_agent: Option<InlineAgentInput>,
    #[serde(default)]
    #[serde(alias = "members")]
    tasks: Option<Vec<TaskMemberInput>>,
    #[serde(default)]
    context: String,
    #[serde(default)]
    name: Option<Value>,
}

const REMOVED_NAME: &str = "`name` was removed; the handle is derived from `subagent_type`. Choose the agent with `subagent_type` (e.g. `\"subagent_type\": \"scout\"`) and drop `name`: the harness names the member `<subagent_type>-<operator>` (`main/scout-suzuran`).";

fn reject_removed_name(name: Option<&Value>) -> Result<(), ToolError> {
    match name {
        Some(_) => Err(ToolError::Input(REMOVED_NAME.to_string())),
        None => Ok(()),
    }
}

struct TaskResult {
    title: String,
    parent_session: String,
    member: String,
    session: String,
    subagent_type: String,
    status: String,
    summary: String,
    model: Option<String>,
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
            include_str!("task.txt"),
            json!({
                "context": {
                    "type": "string",
                    "description": "Shared goal, interfaces and constraints prepended to every task prompt. Put member-specific scope and acceptance in each item prompt."
                },
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
                    "description": "The agent id to spawn (chooses the agent; it also names the member `<subagent_type>-<operator>`). Omitted or empty spawns `hya-task`."
                },
                "category": {
                    "type": "string",
                    "description": "Override the agent's logical model category (e.g. quick, deep) for this spawn; resolves to a concrete provider/model with failover"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model request for this spawn; resolved automatically against the current catalog"
                },
                "effort": {
                    "type": "string",
                    "description": "Optional thinking effort override for this subagent. Omit to inherit the child agent's default effort (shown by list_agents) or an explicit model#effort override; only pass a value supported by the child's model."
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
                        "model": { "type": "string", "description": "Concrete provider/model (request overlay; folds into spawn model precedence)" }
                    }
                },
                "tasks": {
                    "type": "array",
                    "description": "Independent tasks to launch concurrently in one call. Each item needs a non-empty prompt; set agent and model overrides per item. No top-level description or prompt required.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "prompt": { "type": "string", "minLength": 1, "description": "Member-specific scope, non-goals and acceptance criteria" },
                            "subagent_type": { "type": "string" },
                            "category": { "type": "string" },
                            "model": { "type": "string" },
                            "effort": { "type": "string", "description": "Thinking effort for this member (see top-level `effort`)" },
                            "inline_agent": {
                                "type": "object",
                                "description": "Request-scoped agent overlay for this member spawn only. Supplies its own system prompt and name for the child and folds into the same model/category precedence chain; not retained for later reuse as an agent definition.",
                                "properties": {
                                    "name": { "type": "string" },
                                    "prompt": { "type": "string" },
                                    "category": { "type": "string" },
                                    "model": { "type": "string" }
                                }
                            }
                        },
                        "required": ["prompt"]
                    }
                }
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        for field in ["tasks", "members"] {
            if input.get(field).is_some_and(|value| !value.is_array()) {
                return Err(ToolError::Input(format!(
                    "{field} must be a non-empty array"
                )));
            }
        }
        let input: TaskInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let parent_session = ctx
            .session
            .ok_or_else(|| ToolError::Other("task tool requires a session".to_string()))?
            .to_string();
        reject_removed_name(input.name.as_ref())?;

        let batch = input.tasks.is_some();
        if input.tasks.as_ref().is_some_and(Vec::is_empty) {
            return Err(ToolError::Input(
                "tasks must contain at least one task".to_string(),
            ));
        }
        let mut members: Vec<SpawnMember> = input
            .tasks
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                reject_removed_name(m.name.as_ref())?;
                if m.prompt.trim().is_empty() {
                    return Err(ToolError::Input(
                        "each task needs a non-empty prompt".to_string(),
                    ));
                }
                let subagent_type = normalized_agent_target(&m.subagent_type);
                let inline_agent = m
                    .inline_agent
                    .map(|inline| inline.into_inline(&subagent_type));
                Ok(SpawnMember {
                    description: m.description,
                    prompt: m.prompt,
                    subagent_type,
                    model: m.model,
                    category: m.category,
                    effort: m.effort,
                    inline_agent,
                })
            })
            .collect::<Result<_, ToolError>>()?;
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
                effort: input.effort,
                inline_agent,
            });
        }

        if !input.context.trim().is_empty() {
            for member in &mut members {
                member.prompt = format!("{}\n\n{}", input.context, member.prompt);
            }
        }

        for member in &members {
            ctx.permission
                .assert(
                    Action::Task,
                    Resource::Subagent(member.subagent_type.clone()),
                )
                .await?;
        }

        let spawner = ctx
            .extension::<SpawnerPlane>()
            .ok_or_else(|| ToolError::Other(SpawnError::Unavailable.to_string()))?;
        let outcomes = spawner
            .spawn(ctx.operation, members.clone(), ctx.cancel.clone())
            .await
            .map_err(|error| match error {
                SpawnError::Overloaded => ToolError::Overloaded(error.to_string()),
                SpawnError::Unavailable | SpawnError::Cancelled => {
                    ToolError::Other(error.to_string())
                }
                SpawnError::UnknownAgentId { agent_id } => ToolError::UnknownAgentId { agent_id },
                SpawnError::AgentSpawnNotAllowed { caller, agent_id } => {
                    ToolError::AgentSpawnNotAllowed { caller, agent_id }
                }
                SpawnError::UnsupportedInlineAgentField { field } => {
                    ToolError::UnsupportedInlineAgentField { field }
                }
                SpawnError::InvalidEffort { .. } => ToolError::Input(error.to_string()),
            })?;
        if outcomes.len() != members.len() {
            return Err(ToolError::Other(
                "task spawner returned an unexpected number of outcomes".to_string(),
            ));
        }
        if !batch && outcomes.len() == 1 {
            let member = members.remove(0);
            let Some(outcome) = outcomes.into_iter().next() else {
                return Err(ToolError::Other(
                    "task spawner returned no outcome".to_string(),
                ));
            };
            return Ok(render_single(TaskResult {
                title: member.description,
                parent_session,
                member: outcome.member,
                session: outcome.session,
                subagent_type: member.subagent_type,
                status: outcome.status,
                summary: outcome.summary,
                model: outcome.model,
                command: input.command,
            }));
        }

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
                    "model": o.model,
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
        let running = members_json
            .iter()
            .filter(|m| m["status"] == "running")
            .count();
        let failed = members_json
            .iter()
            .filter(|m| {
                m["status"] != "running" && m["status"] != "done" && m["status"] != "completed"
            })
            .count();
        let state = if running > 0 {
            "running"
        } else if failed > 0 {
            "error"
        } else {
            "completed"
        };
        Ok(json!({
            "title": title,
            "metadata": {
                "parentSessionId": parent_session,
                "members": members_json,
                "status": state,
            },
            "output": format!(
                "<task state=\"{state}\">\n<task_result>\n{running} running, {failed} failed. Running agents report later; continue independent work, then use wait when blocked.\n{}\n</task_result>\n</task>",
                serde_json::to_string(&members_json).map_err(|error| ToolError::Other(error.to_string()))?
            ),
        }))
    }
}

fn normalized_agent_target(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "hya-task".to_string()
    } else {
        value.to_string()
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
        ("member".to_string(), json!(result.member)),
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
    let model_attr = result
        .model
        .as_deref()
        .map(|model| format!(" model=\"{model}\""))
        .unwrap_or_default();
    if let Some(model) = result.model {
        metadata.insert("model".to_string(), json!(model));
    }
    json!({
        "title": result.title,
        "metadata": metadata,
        "output": format!(
            "<task id=\"{}\"{} state=\"{}\">\n<{}>\n{}\n</{}>\n</task>",
            result.session, model_attr, state, tag, result.summary, tag
        ),
    })
}

/// Search archived direct subagents by their final state handoff.
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
             handle and digest. Use `send` with the handle as `channel` to revive \
             the agent with its saved state.",
            json!({
                "query": {"type": "string", "description": "Free-text query over goal/state/pending digests; empty lists all"}
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SearchAgentInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let mailbox = ctx
            .extension::<MailboxPlane>()
            .cloned()
            .unwrap_or_else(MailboxPlane::disconnected);
        let rows = mailbox.search_agents(input.query).await?;
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

/// Stop one of the caller's subagents and archive it.
pub struct ArchiveTool;

#[async_trait]
impl Tool for ArchiveTool {
    fn name(&self) -> &str {
        "archive"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "archive",
            "Stop one of your subagents and archive it: its in-flight turn is cancelled, a state handoff is written, and it leaves your live roster (its own subagents are archived first). Use it for a subagent that is stuck, no longer needed, or blocking your own report. An archived subagent stays readable (session log, channel history) and is NOT gone: sending mail to its handle wakes it again with the same handle and session. The team lead (`main`) can never be archived.",
            json!({
                "target": {
                    "type": "string",
                    "description": "The subagent to archive: its handle as returned by `task` (e.g. `main/hya-task-exusiai`, or just the leaf `hya-task-exusiai` for your own subagent) or its session id (`hysec_...`)."
                },
                "reason": {
                    "type": "string",
                    "description": "Short reason, recorded on its member row (optional)."
                }
            }),
            &["target"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let target = input
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                ToolError::Input(
                    "archive requires `target`: the subagent's handle (e.g. `main/hya-task-exusiai`) or session id from the `task` result".to_string(),
                )
            })?
            .to_string();
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default();
        let lifecycle = ctx
            .extension::<LifecyclePlane>()
            .cloned()
            .unwrap_or_else(LifecyclePlane::disconnected);
        let receipt = lifecycle.archive(target, reason).await?;
        let mut output = format!(
            "Archived `{}` (session {}).",
            receipt.handle, receipt.session
        );
        if receipt.cancelled_turn {
            output.push_str(" Its in-flight turn was cancelled.");
        }
        if !receipt.descendants.is_empty() {
            output.push_str(&format!(
                " Its subagents were archived first: {}.",
                receipt.descendants.join(", ")
            ));
        }
        output.push_str(&format!(
            " Send mail to `{}` to wake it again with its handoff.",
            receipt.handle
        ));
        Ok(json!({
            "title": format!("Archived {}", receipt.handle),
            "output": output,
            "metadata": receipt,
        }))
    }
}
