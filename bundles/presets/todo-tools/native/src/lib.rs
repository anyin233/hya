//! Native implementation of the TODO tool family.

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Action, Resource, TodoItem, TodoStatus, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

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

struct TodoReadTool;

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

struct TodoUpdateStatusTool;

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

struct TodoUpdateContentTool;

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

/// Report the lockstep Rust tool ABI before any Rust object crosses the library boundary.
///
/// # Safety
///
/// `out` must point to a writable array of at least 32 bytes, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_abi_v1(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let digest = hya_tool::native_bundle::abi_digest_v1();
    // SAFETY: the caller supplies a writable 32-byte output array.
    unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len()) };
}

/// Register this bundle's concrete tools with the host's tool registry loader.
///
/// # Safety
///
/// `out` must point to a live `Vec<Arc<dyn Tool>>` built with the matching
/// hya-tool ABI, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_v1(out: *mut Vec<Arc<dyn Tool>>) {
    // SAFETY: the host calls this only after the ABI digest matches.
    if let Some(out) = unsafe { out.as_mut() } {
        let tools: [Arc<dyn Tool>; 3] = [
            Arc::new(TodoReadTool),
            Arc::new(TodoUpdateStatusTool),
            Arc::new(TodoUpdateContentTool),
        ];
        out.extend(tools);
    }
}
