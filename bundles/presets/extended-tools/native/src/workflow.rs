//! Workflow tool implementation.
use async_trait::async_trait;
use hya_proto::{ToolSchema, WorkflowCommand, WorkflowRevision, WorkflowRunId};
use hya_tool::tool::obj_schema;
use hya_tool::{Action, Resource, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct WorkflowToolInput {
    /// Command name. `list` remains the default for compatibility.
    #[serde(default, alias = "command")]
    action: Option<String>,
    /// Declared Workflow name for `info`, `select`, and `run`.
    #[serde(default)]
    name: Option<String>,
    /// Optimistic compiler revision for `select` and `run`.
    #[serde(default)]
    expected_revision: Option<WorkflowRevision>,
    /// Values for the Workflow's declared inputs (`run` only).
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    /// Stable direct-call run id for idempotent `run` retries.
    #[serde(default)]
    run: Option<WorkflowRunId>,
}

/// Execute one typed Workflow command through the application control seam.
pub struct WorkflowTool;

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &str {
        "workflow"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "workflow",
            "List, inspect, select, run, or inspect the state of user-authored workflow DAGs. Workflow execution uses the same durable Session control path as the CLI and direct server requests.",
            json!({
                "action": {
                    "type": "string",
                    "enum": ["list", "info", "select", "run", "state"],
                    "description": "list: discover workflows (default); info: inspect one graph; select: persist one workflow selection; run: execute one workflow; state: read durable selection/run state"
                },
                "name": {
                    "type": "string",
                    "description": "Declared Workflow name (required for action=info|select|run)"
                },
                "expected_revision": {
                    "type": "string",
                    "description": "Optional 64-character compiler revision fence for select/run"
                },
                "inputs": {
                    "type": "object",
                    "description": "Values for the Workflow's declared input keys (action=run)",
                    "additionalProperties": { "type": ["string", "number", "boolean"] }
                },
                "run": {
                    "type": "string",
                    "description": "Optional stable Workflow run id for direct idempotent retries"
                }
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WorkflowToolInput =
            serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))?;
        let action = input.action.as_deref().unwrap_or("list");
        let command = match action {
            "list" => WorkflowCommand::List,
            "info" => WorkflowCommand::Info {
                name: required_name(input.name, "info")?,
            },
            "select" => WorkflowCommand::Select {
                name: required_name(input.name, "select")?,
                expected_revision: input.expected_revision,
            },
            "state" => WorkflowCommand::State,
            "run" => {
                let name = input.name.map(|name| name.trim().to_string());
                if name.as_deref().is_some_and(str::is_empty) {
                    return Err(ToolError::Input(
                        "action=run `name` must not be empty".to_string(),
                    ));
                }
                ctx.permission
                    .assert(
                        Action::Task,
                        Resource::Subagent(format!(
                            "workflow:{}",
                            name.as_deref().unwrap_or("selected")
                        )),
                    )
                    .await?;
                let inputs = input
                    .inputs
                    .into_iter()
                    .map(|(key, value)| {
                        let value = match value {
                            Value::String(text) => text,
                            other => other.to_string(),
                        };
                        (key, value)
                    })
                    .collect();
                WorkflowCommand::Run {
                    name,
                    expected_revision: input.expected_revision,
                    inputs,
                    run: input.run,
                }
            }
            other => {
                return Err(ToolError::Input(format!(
                    "unknown workflow action `{other}` (expected list|info|select|run|state)"
                )));
            }
        };
        let result = ctx
            .workflows
            .execute(ctx.operation, command, ctx.cancel.clone())
            .await?;
        serde_json::to_value(result)
            .map_err(|error| ToolError::Other(format!("serialize workflow result: {error}")))
    }
}

fn required_name(name: Option<String>, action: &str) -> Result<String, ToolError> {
    let name = name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ToolError::Input(format!("action={action} requires `name`")));
    }
    Ok(name)
}
