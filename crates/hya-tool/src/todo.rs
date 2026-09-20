//! In-memory per-session todo list plane and the `todo__` namespaced tool
//! group (`todo__read`, `todo__update_status`, `todo__update_content`).
//!
//! The plane owns stable item ids: adds draw sequential string ids ("1",
//! "2", …) from a per-session counter that never reuses values. Write tools
//! mutate the list under the plane's lock and return the full snapshot in
//! `metadata.todos` so the engine's replay fold stays "take the latest
//! write result".

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
    /// Stable item identifier assigned by the plane (never reused).
    pub id: String,
    /// Human-readable task text.
    pub content: String,
    /// Lifecycle status of the item.
    pub status: TodoStatus,
}

/// Lifecycle status of a [`TodoItem`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// Currently being worked on.
    InProgress,
    /// Waiting on an external unblock (dependency, user input, review).
    Blocked,
    /// Done.
    Completed,
}

impl TodoStatus {
    /// Borrow the wire spelling of the status.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
        }
    }
}

/// Mutable per-session todo state guarded by the plane's lock.
#[derive(Default)]
pub struct SessionTodos {
    /// Ordered items of the session.
    pub items: Vec<TodoItem>,
    /// Monotonic id counter; drawn ids are never reused.
    next_id: u64,
}

impl SessionTodos {
    /// Draw the next sequential id.
    pub fn next_id(&mut self) -> String {
        self.next_id += 1;
        self.next_id.to_string()
    }
}

/// Session-scoped todo store (not independently persisted outside the event log).
#[derive(Clone, Default)]
pub struct TodoPlane {
    todos: Arc<Mutex<HashMap<SessionId, SessionTodos>>>,
}

impl TodoPlane {
    /// Run `f` on the session's todo state under the plane's lock and return
    /// its value. Mutation is atomic with respect to other `apply`/`get`
    /// calls on the same plane.
    pub async fn apply<T>(&self, session: SessionId, f: impl FnOnce(&mut SessionTodos) -> T) -> T {
        let mut guard = self.todos.lock().await;
        let state = guard.entry(session).or_default();
        f(state)
    }

    /// Return a clone of the current list (empty if never written).
    pub async fn get(&self, session: SessionId) -> Vec<TodoItem> {
        self.apply(session, |state| state.items.clone()).await
    }
}

fn todo_snapshot_metadata(items: &[TodoItem]) -> Value {
    json!({
        "todos": items,
        "open": items
            .iter()
            .filter(|item| item.status != TodoStatus::Completed)
            .count(),
    })
}

fn todo_title(items: &[TodoItem]) -> String {
    format!(
        "{} todo{}",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    )
}

fn session_required(ctx: &ToolCtx) -> Result<hya_proto::SessionId, ToolError> {
    ctx.session
        .ok_or_else(|| ToolError::Other("todo tools require a session".to_string()))
}

fn unknown_id_error(items: &[TodoItem], id: &str) -> ToolError {
    let valid = items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    ToolError::Input(format!("unknown todo id `{id}`; current ids: [{valid}]"))
}

pub(crate) struct TodoReadTool;

#[async_trait]
impl Tool for TodoReadTool {
    fn name(&self) -> &str {
        "todo__read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("todo__read"),
            description: "Read the session's current todo list with stable item ids and statuses."
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let session = session_required(ctx)?;
        let items = ctx.todo.get(session).await;
        Ok(json!({
            "title": todo_title(&items),
            "output": serde_json::to_string_pretty(&items)?,
            "metadata": todo_snapshot_metadata(&items),
        }))
    }
}

#[derive(Deserialize)]
struct UpdateStatusInput {
    updates: Vec<StatusUpdate>,
}

#[derive(Deserialize)]
struct StatusUpdate {
    id: String,
    status: TodoStatus,
}

pub(crate) struct TodoUpdateStatusTool;

#[async_trait]
impl Tool for TodoUpdateStatusTool {
    fn name(&self) -> &str {
        "todo__update_status"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("todo__update_status"),
            description: "Batch-update todo item statuses (`pending`, `in_progress`, `blocked`, `completed`) by stable id.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "updates": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "blocked", "completed"] }
                            },
                            "required": ["id", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["updates"],
                "additionalProperties": false
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: UpdateStatusInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::TodoWrite, Resource::Any)
            .await?;
        let session = session_required(ctx)?;
        let items = ctx
            .todo
            .apply(session, |state| {
                for update in &input.updates {
                    let Some(item) = state.items.iter_mut().find(|item| item.id == update.id)
                    else {
                        return Err(unknown_id_error(&state.items, &update.id));
                    };
                    item.status = update.status;
                }
                Ok(state.items.clone())
            })
            .await?;
        Ok(json!({
            "title": todo_title(&items),
            "output": serde_json::to_string_pretty(&items)?,
            "metadata": todo_snapshot_metadata(&items),
        }))
    }
}

#[derive(Deserialize)]
struct UpdateContentInput {
    operations: Vec<ContentOp>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ContentOp {
    Add { content: String },
    Remove { id: String },
    Edit { id: String, content: String },
}

pub(crate) struct TodoUpdateContentTool;

#[async_trait]
impl Tool for TodoUpdateContentTool {
    fn name(&self) -> &str {
        "todo__update_content"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("todo__update_content"),
            description: "Batch-edit the todo list itself: `add` a task, `remove` one by id, or `edit` a task's text. Operations apply atomically in order.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "op": { "const": "add" },
                                        "content": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["op", "content"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "op": { "const": "remove" },
                                        "id": { "type": "string" }
                                    },
                                    "required": ["op", "id"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "op": { "const": "edit" },
                                        "id": { "type": "string" },
                                        "content": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["op", "id", "content"]
                                }
                            ]
                        }
                    }
                },
                "required": ["operations"],
                "additionalProperties": false
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: UpdateContentInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::TodoWrite, Resource::Any)
            .await?;
        let session = session_required(ctx)?;
        let (items, added_ids) = ctx
            .todo
            .apply(session, |state| {
                // Validate every referenced id before mutating anything so a
                // failed batch leaves the list untouched.
                for op in &input.operations {
                    let id = match op {
                        ContentOp::Remove { id } | ContentOp::Edit { id, .. } => id,
                        ContentOp::Add { .. } => continue,
                    };
                    if !state.items.iter().any(|item| &item.id == id) {
                        return Err(unknown_id_error(&state.items, id));
                    }
                }
                let mut added_ids = Vec::new();
                for op in &input.operations {
                    match op {
                        ContentOp::Add { content } => {
                            let id = state.next_id();
                            added_ids.push(id.clone());
                            state.items.push(TodoItem {
                                id,
                                content: content.clone(),
                                status: TodoStatus::Pending,
                            });
                        }
                        ContentOp::Remove { id } => {
                            state.items.retain(|item| &item.id != id);
                        }
                        ContentOp::Edit { id, content } => {
                            if let Some(item) = state.items.iter_mut().find(|item| &item.id == id) {
                                item.content = content.clone();
                            }
                        }
                    }
                }
                Ok((state.items.clone(), added_ids))
            })
            .await?;
        let mut metadata = todo_snapshot_metadata(&items);
        metadata["addedIds"] = json!(added_ids);
        Ok(json!({
            "title": todo_title(&items),
            "output": serde_json::to_string_pretty(&items)?,
            "metadata": metadata,
        }))
    }
}
