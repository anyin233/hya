//! Native `workflow` tool registered by `hya/workflow`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{Action, Resource, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{WorkflowCommand, WorkflowDelivery, WorkflowRevision, WorkflowRunId};
use crate::service::{WorkflowInvocation, WorkflowService};

#[derive(Deserialize)]
struct WorkflowToolInput {
    /// Command name. `list` is the default.
    #[serde(default)]
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

/// Execute one typed Workflow command through [`WorkflowService`].
pub struct WorkflowTool {
    service: Arc<WorkflowService>,
}

impl WorkflowTool {
    /// Construct a [`WorkflowTool`] backed by `service`.
    #[must_use]
    pub fn new(service: Arc<WorkflowService>) -> Self {
        Self { service }
    }
}

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

        if !self.service.is_started() {
            // Byte-identical to what an unstarted service returned before.
            return Err(ToolError::Other(
                "workflow tool requires a session".to_string(),
            ));
        }
        let session = ctx.session.ok_or_else(|| ToolError::Coded {
            code: "SESSION_NOT_FOUND".to_string(),
            message: "workflow tool requires an active session".to_string(),
        })?;
        let invocation = WorkflowInvocation {
            caller: None,
            operation: Some(ctx.operation),
            delivery: WorkflowDelivery::Finished,
        };
        let result = self
            .service
            .execute(session, invocation, command, ctx.cancel.clone())
            .await
            .map_err(|error| ToolError::Coded {
                code: error.code().to_string(),
                message: error.to_string(),
            })?;
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
