//! In-memory per-session todo list plane for the `todowrite` tool.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{SessionId, ToolName, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

/// One todo row stored for a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Human-readable task text.
    pub content: String,
    /// Status string (for example `pending` / `completed`).
    pub status: TodoStatus,
    /// Priority string (opaque to the plane).
    pub priority: TodoPriority,
}

/// Opaque status token carried on a [`TodoItem`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TodoStatus(String);

impl TodoStatus {
    /// Borrow the raw status string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque priority token carried on a [`TodoItem`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TodoPriority(String);

impl TodoPriority {
    /// Borrow the raw priority string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Session-scoped todo store (not independently persisted outside the event log).
#[derive(Clone, Default)]
pub struct TodoPlane {
    todos: Arc<Mutex<HashMap<SessionId, Vec<TodoItem>>>>,
}

impl TodoPlane {
    /// Replace the entire todo list for `session`.
    pub async fn update(&self, session: SessionId, todos: Vec<TodoItem>) {
        self.todos.lock().await.insert(session, todos);
    }

    /// Return a clone of the current list (empty if never written).
    pub async fn get(&self, session: SessionId) -> Vec<TodoItem> {
        self.todos
            .lock()
            .await
            .get(&session)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) struct TodoWriteTool;

#[derive(Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todowrite"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("todowrite"),
            description: "Write the current session's todo list.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": { "type": "string" },
                                "priority": { "type": "string" }
                            },
                            "required": ["content", "status", "priority"]
                        }
                    }
                },
                "required": ["todos"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: TodoWriteInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::TodoWrite, Resource::Any)
            .await?;
        let session = ctx
            .session
            .ok_or_else(|| ToolError::Other("todowrite requires a session".to_string()))?;
        let todos = input.todos;
        ctx.todo.update(session, todos.clone()).await;
        let open = todos
            .iter()
            .filter(|todo| todo.status.as_str() != "completed")
            .count();
        let output = serde_json::to_string_pretty(&todos)?;
        Ok(json!({
            "title": format!("{open} todos"),
            "output": output,
            "metadata": { "todos": todos },
        }))
    }
}
