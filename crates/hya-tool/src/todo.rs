use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{SessionId, ToolName, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TodoStatus(String);

impl TodoStatus {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TodoPriority(String);

impl TodoPriority {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Default)]
pub struct TodoPlane {
    todos: Arc<Mutex<HashMap<SessionId, Vec<TodoItem>>>>,
}

impl TodoPlane {
    pub async fn update(&self, session: SessionId, todos: Vec<TodoItem>) {
        self.todos.lock().await.insert(session, todos);
    }

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
