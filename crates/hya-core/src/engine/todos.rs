use hya_proto::SessionId;
use hya_tool::TodoItem;

use super::SessionEngine;

/// Todo tools whose results carry the session's full list in
/// `metadata.todos` (the current `todo__` group and the legacy names).
pub(crate) fn is_todo_tool(name: &str) -> bool {
    name.starts_with("todo__") || matches!(name, "todowrite" | "todo")
}

impl SessionEngine {
    /// Return the todo list for `session`: the folded `TodosUpdated` list, or
    /// else the in-memory plane.
    pub async fn todos(&self, session: SessionId) -> Vec<TodoItem> {
        if let Ok(projection) = self.store.read_projection_shared(session).await
            && let Some(todos) = &projection.session.todos
        {
            return todos.clone();
        }
        self.todo.get(session).await
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
