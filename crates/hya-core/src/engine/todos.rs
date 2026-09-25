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

/// Todo tools whose results carry the session's full list in
/// `metadata.todos` (the current `todo__` group and the legacy names).
pub(crate) fn is_todo_tool(name: &str) -> bool {
    name.starts_with("todo__") || matches!(name, "todowrite" | "todo")
}

impl SessionEngine {
    /// Return the todo list for `session`: the folded `TodosUpdated` list;
    /// for sessions that predate that event, the latest todo tool result on
    /// the log; else the in-memory plane.
    pub async fn todos(&self, session: SessionId) -> Vec<TodoItem> {
        if let Ok(projection) = self.store.read_projection_shared(session).await
            && let Some(todos) = &projection.session.todos
        {
            return todos.clone();
        }
        self.legacy_todos(session).await
    }

    /// Pre-`TodosUpdated` read: the latest todo tool result on the log.
    async fn legacy_todos(&self, session: SessionId) -> Vec<TodoItem> {
        let mut latest = None;
        let mut todo_calls = HashSet::new();
        let Ok(envelopes) = self.store.replay(session).await else {
            return self.todo.get(session).await;
        };
        for envelope in envelopes {
            match envelope.event {
                Event::ToolCallRequested { call, name, .. }
                    if is_todo_tool(name.as_str()) && name.as_str() != "todo__read" =>
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

    /// Before a todo tool runs: restore the session's recorded list into the
    /// in-memory plane when this process holds none (after a restart), so the
    /// tool edits the list the log shows instead of an empty one.
    pub(crate) async fn restore_todo_plane(&self, session: SessionId) {
        let todos = self.todos(session).await;
        self.todo.restore(session, todos).await;
    }

    /// After a todo tool's `ToolResult`: the list its result carries, when it
    /// differs from the recorded one (`None` for reads and no-op writes).
    pub(crate) async fn changed_todos(
        &self,
        session: SessionId,
        output: &serde_json::Value,
    ) -> Option<Vec<TodoItem>> {
        let todos = output
            .get("metadata")
            .and_then(|metadata| metadata.get("todos"))
            .and_then(|value| serde_json::from_value::<Vec<TodoItem>>(value.clone()).ok())?;
        let recorded = self
            .store
            .read_projection_shared(session)
            .await
            .ok()
            .and_then(|projection| projection.session.todos.clone());
        match recorded {
            Some(recorded) if recorded == todos => None,
            Some(_) => Some(todos),
            // First record on a session: any non-empty list starts the
            // event-sourced history (an empty one still reads correctly from
            // the tool results).
            None => (!todos.is_empty()).then_some(todos),
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
