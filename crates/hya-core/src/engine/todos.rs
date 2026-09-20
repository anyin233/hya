use std::collections::HashSet;

use hya_proto::{Event, SessionId};
use hya_tool::{TodoItem, TodoStatus};
use serde::Deserialize;

use super::SessionEngine;

/// Legacy `todowrite` result item (pre-0.36.53 events): opaque status
/// string, optional priority, no stable id.
#[derive(Deserialize)]
struct LegacyTodoItem {
    content: String,
    status: String,
}

impl SessionEngine {
    /// Return the in-memory todo list for `session`.
    pub async fn todos(&self, session: SessionId) -> Vec<TodoItem> {
        let mut latest = None;
        let mut todo_calls = HashSet::new();
        let Ok(envelopes) = self.store.replay(session).await else {
            return self.todo.get(session).await;
        };
        for envelope in envelopes {
            match envelope.event {
                Event::ToolCallRequested { call, name, .. }
                    if matches!(
                        name.as_str(),
                        "todo__update_status" | "todo__update_content" | "todowrite" | "todo"
                    ) =>
                {
                    todo_calls.insert(call);
                }
                Event::ToolResult { call, output, .. } if todo_calls.contains(&call) => {
                    if let Some(value) = output
                        .get("metadata")
                        .and_then(|metadata| metadata.get("todos"))
                    {
                        if let Ok(todos) = serde_json::from_value::<Vec<TodoItem>>(value.clone()) {
                            latest = Some(todos);
                        } else if let Ok(legacy) =
                            serde_json::from_value::<Vec<LegacyTodoItem>>(value.clone())
                        {
                            latest = Some(legacy_items(legacy));
                        }
                    }
                }
                _ => {}
            }
        }
        match latest {
            Some(todos) => todos,
            None => self.todo.get(session).await,
        }
    }
}

/// Convert legacy rows to the current shape: keep the historical
/// `todo-{index}` wire ids and map unknown status strings to `pending`.
fn legacy_items(legacy: Vec<LegacyTodoItem>) -> Vec<TodoItem> {
    legacy
        .into_iter()
        .enumerate()
        .map(|(index, item)| TodoItem {
            id: format!("todo-{index}"),
            content: item.content,
            status: match item.status.as_str() {
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => TodoStatus::Pending,
            },
        })
        .collect()
}
