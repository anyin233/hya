use async_trait::async_trait;
use hya_proto::{TodoItem, TodoStatus, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Todo;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::kernel) struct Args {
    ops: Vec<Op>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Op {
    Add { content: String },
    Edit { id: String, content: String },
    Status { id: String, status: TodoStatus },
    Remove { id: String },
}

impl Args {
    pub(in crate::kernel) fn apply(
        self,
        mut items: Vec<TodoItem>,
        mut last_id: u64,
    ) -> Result<Vec<TodoItem>, ToolError> {
        for op in self.ops {
            match op {
                Op::Add { content } => {
                    last_id = last_id
                        .checked_add(1)
                        .ok_or_else(|| ToolError::Input("no todo ids available".to_string()))?;
                    items.push(TodoItem {
                        id: last_id.to_string(),
                        content: checked_content(content)?,
                        status: TodoStatus::Pending,
                    });
                }
                Op::Edit { id, content } => {
                    let index = index(&items, &id)?;
                    items[index].content = checked_content(content)?;
                }
                Op::Status { id, status } => {
                    let index = index(&items, &id)?;
                    items[index].status = status;
                }
                Op::Remove { id } => {
                    items.remove(index(&items, &id)?);
                }
            }
        }
        Ok(items)
    }
}

/// `id` as given or as the list renders it (`#3`).
fn index(items: &[TodoItem], id: &str) -> Result<usize, ToolError> {
    let bare = id.strip_prefix('#').unwrap_or(id);
    items
        .iter()
        .position(|item| item.id == bare)
        .ok_or_else(|| ToolError::Input(format!("unknown todo id `{id}`")))
}

fn checked_content(content: String) -> Result<String, ToolError> {
    if content.trim().is_empty() {
        return Err(ToolError::Input(
            "todo content must not be blank".to_string(),
        ));
    }
    Ok(content)
}

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &str {
        super::TODO
    }

    fn schema(&self) -> ToolSchema {
        let id = json!({"type": "string", "description": "Item id from the todo list"});
        let content = json!({"type": "string", "minLength": 1});
        super::schema(
            super::TODO,
            "Record the task's completion criteria as session todos. Each item is a checkable end state (\"tests in tests/ pass\"), not an action. Write the list when the task is clear, and set an item's status as soon as it changes, not at the end. The current list is kept verbatim when earlier context is removed. Operations apply atomically in order. Returns the full list, including assigned ids. Pass an empty ops array to read it.",
            json!({
                "type": "object",
                "properties": {
                    "ops": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "oneOf": [
                                {
                                    "properties": {"op": {"const": "add"}, "content": content},
                                    "required": ["op", "content"],
                                    "additionalProperties": false
                                },
                                {
                                    "properties": {"op": {"const": "edit"}, "id": id, "content": content},
                                    "required": ["op", "id", "content"],
                                    "additionalProperties": false
                                },
                                {
                                    "properties": {"op": {"const": "status"}, "id": id, "status": {
                                        "type": "string", "enum": ["pending", "in_progress", "blocked", "completed"]
                                    }},
                                    "required": ["op", "id", "status"],
                                    "additionalProperties": false
                                },
                                {
                                    "properties": {"op": {"const": "remove"}, "id": id},
                                    "required": ["op", "id"],
                                    "additionalProperties": false
                                }
                            ]
                        }
                    }
                },
                "required": ["ops"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Err(ToolError::Other(
            "todo runs only inside the kernel loop".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_accepted_as_the_list_renders_them() -> Result<(), Box<dyn std::error::Error>> {
        let args: Args = serde_json::from_value(json!({"ops": [
            {"op": "add", "content": "tests pass"},
            {"op": "status", "id": "#1", "status": "completed"},
        ]}))?;
        let items = args.apply(Vec::new(), 0)?;
        assert_eq!(items[0].status, TodoStatus::Completed);
        Ok(())
    }
}
