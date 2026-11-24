use async_trait::async_trait;
use hya_proto::{TodoItem, TodoStatus, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

pub(super) struct Todo;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::kernel) struct Args {
    ops: Vec<Op>,
}

enum Op {
    Add {
        content: String,
        status: Option<TodoStatus>,
    },
    Update {
        id: String,
        content: Option<String>,
        status: Option<TodoStatus>,
    },
    Remove {
        id: String,
    },
}

const OP_HINT: &str =
    r#"use add, edit, status, or remove; for example {"op":"add","content":"tests pass"}"#;

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{Error, MapAccess, Visitor, value::MapAccessDeserializer};

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            #[serde(default, deserialize_with = "present")]
            op: Option<String>,
            #[serde(default, deserialize_with = "present")]
            id: Option<String>,
            #[serde(default, deserialize_with = "present")]
            content: Option<String>,
            #[serde(default, deserialize_with = "present")]
            status: Option<TodoStatus>,
        }

        struct Operation;
        impl<'de> Visitor<'de> for Operation {
            type Value = Op;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(OP_HINT)
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Op, M::Error> {
                let Fields {
                    op,
                    id,
                    content,
                    status,
                } = Fields::deserialize(MapAccessDeserializer::new(map))
                    .map_err(|error| M::Error::custom(format!("{error}; {OP_HINT}")))?;
                match (op.as_deref(), id, content, status) {
                    (None | Some("add"), None, Some(content), status) => {
                        Ok(Op::Add { content, status })
                    }
                    (None | Some("edit" | "status"), Some(id), content, status) => Ok(Op::Update {
                        id,
                        content,
                        status,
                    }),
                    (Some("remove"), Some(id), None, None) => Ok(Op::Remove { id }),
                    (Some(label), Some(id), content, None) => {
                        let status = TodoStatus::deserialize(serde::de::value::StrDeserializer::<
                            M::Error,
                        >::new(label))
                        .map_err(|_| M::Error::custom(OP_HINT))?;
                        Ok(Op::Update {
                            id,
                            content,
                            status: Some(status),
                        })
                    }
                    _ => Err(M::Error::custom(OP_HINT)),
                }
            }
        }
        deserializer.deserialize_map(Operation)
    }
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl Args {
    pub(in crate::kernel) fn apply(
        self,
        mut items: Vec<TodoItem>,
        mut last_id: u64,
    ) -> Result<Vec<TodoItem>, ToolError> {
        for op in self.ops {
            match op {
                Op::Add { content, status } => {
                    last_id = last_id
                        .checked_add(1)
                        .ok_or_else(|| ToolError::Input("no todo ids available".to_string()))?;
                    items.push(TodoItem {
                        id: last_id.to_string(),
                        content: checked_content(content)?,
                        status: status.unwrap_or(TodoStatus::Pending),
                    });
                }
                Op::Update {
                    id,
                    content,
                    status,
                } => {
                    if content.is_none() && status.is_none() {
                        return Err(ToolError::Input(format!(
                            "todo edit or status requires content or status; {OP_HINT}"
                        )));
                    }
                    let index = index(&items, &id)?;
                    if let Some(content) = content {
                        items[index].content = checked_content(content)?;
                    }
                    if let Some(status) = status {
                        items[index].status = status;
                    }
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

    // The advertised contract requires op. Only non-validating callers can
    // reach the decoder's omitted-op and status-as-op compatibility forms.
    fn schema(&self) -> ToolSchema {
        let id = json!({"type": "string", "description": "Item id from the todo list"});
        let content = json!({"type": "string", "pattern": r"\S"});
        let status = json!({
            "type": "string", "enum": ["pending", "in_progress", "blocked", "completed"]
        });
        super::schema(
            super::TODO,
            "Record checkable completion criteria (such as \"tests pass\") and results needed after earlier context is removed. The list survives rewrites verbatim. Write it when the task is clear, update statuses as they change, and leave correct items as written. Add takes content and optional status (pending by default). Edit and status both take an id and content, status, or both. Remove takes an id. Operations apply atomically in order and return the full list with ids. Pass empty ops to read it. Examples: {\"ops\":[{\"op\":\"add\",\"content\":\"tests pass\"}]}, {\"ops\":[{\"op\":\"status\",\"id\":\"#1\",\"status\":\"completed\"}]}, {\"ops\":[{\"op\":\"remove\",\"id\":\"#1\"}]}.",
            json!({
                "type": "object",
                "properties": {
                    "ops": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "oneOf": [
                                {
                                    "properties": {"op": {"const": "add"}, "content": content, "status": status},
                                    "required": ["op", "content"],
                                    "additionalProperties": false
                                },
                                {
                                    "properties": {"op": {"enum": ["edit", "status"]}, "id": id, "content": content, "status": status},
                                    "required": ["op", "id"],
                                    "anyOf": [{"required": ["content"]}, {"required": ["status"]}],
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

    #[test]
    fn add_accepts_initial_status() -> Result<(), Box<dyn std::error::Error>> {
        let args: Args = serde_json::from_value(json!({"ops": [
            {"op": "add", "content": "tests pass", "status": "in_progress"},
        ]}))?;
        let items = args.apply(Vec::new(), 0)?;
        assert_eq!(items[0].content, "tests pass");
        assert_eq!(items[0].status, TodoStatus::InProgress);
        Ok(())
    }

    #[test]
    fn edit_and_status_each_accept_content_and_status() -> Result<(), Box<dyn std::error::Error>> {
        for op in ["edit", "status"] {
            let args: Args = serde_json::from_value(json!({"ops": [
                {"op": "add", "content": "tests pass"},
                {"op": op, "id": "1", "content": "focused tests pass", "status": "completed"},
            ]}))?;
            let items = args.apply(Vec::new(), 0)?;
            assert_eq!(items[0].content, "focused tests pass");
            assert_eq!(items[0].status, TodoStatus::Completed);
        }
        Ok(())
    }

    #[test]
    fn edit_and_status_require_at_least_one_change() -> Result<(), Box<dyn std::error::Error>> {
        for op in ["edit", "status"] {
            let args: Args = serde_json::from_value(json!({"ops": [
                {"op": "add", "content": "tests pass"},
                {"op": op, "id": "1"},
            ]}))?;
            assert!(matches!(
                args.apply(Vec::new(), 0),
                Err(ToolError::Input(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn omitted_op_uses_id_to_distinguish_edit_from_add() -> Result<(), Box<dyn std::error::Error>> {
        let args: Args = serde_json::from_value(json!({"ops": [
            {"content": "write tests"},
            {"content": "run tests", "status": "blocked"},
            {"id": "#1", "content": "write regression test"},
            {"id": "#2", "status": "completed"},
        ]}))?;
        let items = args.apply(Vec::new(), 0)?;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].content, "write regression test");
        assert_eq!(items[0].status, TodoStatus::Pending);
        assert_eq!(items[1].status, TodoStatus::Completed);
        Ok(())
    }

    #[test]
    fn status_as_op_updates_the_item() -> Result<(), Box<dyn std::error::Error>> {
        for status in [
            TodoStatus::Pending,
            TodoStatus::InProgress,
            TodoStatus::Completed,
            TodoStatus::Blocked,
        ] {
            let args: Args = serde_json::from_value(json!({"ops": [
                {"content": "original", "status": "blocked"},
                {"op": status, "id": "#1", "content": "updated"},
            ]}))?;
            let items = args.apply(Vec::new(), 0)?;
            assert_eq!(items[0].status, status);
            assert_eq!(items[0].content, "updated");
        }
        Ok(())
    }

    #[test]
    fn ambiguous_or_malformed_ops_get_a_canonical_example() {
        for op in [
            json!({"op":"done","id":"#1"}),
            json!({"op":"completed","id":"#1","status":"pending"}),
            json!({"op":"completed","content":"no id"}),
            json!({"op":null,"content":"no op"}),
            json!({"id":"#1","status":null}),
            json!({"content":"new","extra":true}),
            json!({"op":"remove","id":"#1","content":"extra"}),
        ] {
            let err = serde_json::from_value::<Args>(json!({"ops":[op]})).err();
            assert!(err.is_some_and(|e| e.to_string().contains(OP_HINT)), "{op}");
        }
    }

    #[test]
    fn add_rejects_an_id_and_null_status() {
        for op in [
            json!({"op": "add", "id": "1", "content": "tests pass"}),
            json!({"op": "add", "content": "tests pass", "status": null}),
        ] {
            assert!(serde_json::from_value::<Args>(json!({"ops": [op]})).is_err());
        }
    }
}
