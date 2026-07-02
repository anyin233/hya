use async_trait::async_trait;
use hya_proto::{SessionId, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::permission::{Action, Resource};
use crate::spawn::SpawnMember;
use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

pub struct TaskTool;

#[derive(Deserialize)]
struct TaskMemberInput {
    #[serde(default)]
    description: String,
    prompt: String,
    subagent_type: String,
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
    task_id: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    background: bool,
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
    background: bool,
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "task",
            "Launch a specialized subagent for a complex task. Use task_id to resume a prior subagent session; background launches are accepted by schema but currently require foreground execution in hya.",
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
                "task_id": {
                    "type": "string",
                    "description": "Resume a previous task session instead of creating a fresh one"
                },
                "command": {
                    "type": "string",
                    "description": "The command that triggered this task"
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the agent in the background"
                },
                "members": {
                    "type": "array",
                    "description": "hya extension: dispatch several members in one tool call",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "prompt": { "type": "string" },
                            "subagent_type": { "type": "string" }
                        },
                        "required": ["prompt", "subagent_type"]
                    }
                }
            }),
            &["description", "prompt", "subagent_type"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        // Nested subagents are allowed: a subagent may call `task` to spawn its own
        // subagents. Recursion depth and total fan-out are bounded by the engine's
        // SubagentGovernor (max_depth + per-run budget), enforced in `run_team`, so
        // there is no hard one-level cap here.
        let input: TaskInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let background = input.background;
        let parent_session = ctx
            .session
            .ok_or_else(|| ToolError::Other("task tool requires a session".to_string()))?
            .to_string();
        let task_id = input.task_id;
        if let Some(task_id) = task_id.as_deref() {
            task_id
                .parse::<SessionId>()
                .map_err(|e| ToolError::Input(format!("invalid task_id: {e}")))?;
        }

        let mut members: Vec<SpawnMember> = input
            .members
            .into_iter()
            .map(|m| SpawnMember {
                description: m.description,
                prompt: m.prompt,
                subagent_type: m.subagent_type,
                task_id: None,
            })
            .collect();
        if members.is_empty() {
            if input.description.trim().is_empty()
                || input.prompt.trim().is_empty()
                || input.subagent_type.trim().is_empty()
            {
                return Err(ToolError::Input(
                    "provide description, prompt, and subagent_type".to_string(),
                ));
            }
            members.push(SpawnMember {
                description: input.description,
                prompt: input.prompt,
                subagent_type: input.subagent_type,
                task_id,
            });
        }
        if background && members.len() != 1 {
            return Err(ToolError::Input(
                "background task execution requires a single task member".to_string(),
            ));
        }

        for member in &members {
            ctx.permission
                .assert(
                    Action::Task,
                    Resource::Subagent(member.subagent_type.clone()),
                )
                .await?;
        }

        let outcomes = if background {
            ctx.spawner
                .spawn_background(members.clone(), ctx.cancel.clone())
                .await
        } else {
            ctx.spawner.spawn(members.clone(), ctx.cancel.clone()).await
        }
        .map_err(|e| ToolError::Other(e.to_string()))?;
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
                background,
            }));
        }

        let members_json: Vec<Value> = outcomes
            .into_iter()
            .map(|o| {
                json!({
                    "member": o.member,
                    "session": o.session,
                    "status": o.status,
                    "summary": o.summary,
                })
            })
            .collect();
        Ok(json!({ "members": members_json }))
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
    if result.background {
        metadata.insert("background".to_string(), json!(true));
        metadata.insert("jobId".to_string(), json!(result.session.clone()));
    }
    let summary = if result.background && state == "running" {
        "<summary>Background task started</summary>\n"
    } else {
        ""
    };
    json!({
        "title": result.title,
        "metadata": metadata,
        "output": format!(
            "<task id=\"{}\" state=\"{}\">\n{}<{}>\n{}\n</{}>\n</task>",
            result.session, state, summary, tag, result.summary, tag
        ),
    })
}
