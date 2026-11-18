//! Session todo rows: the shape the kernel todo tool writes, the
//! `TodosUpdated` event records, and the projection folds.

use serde::{Deserialize, Serialize};

/// One todo row stored for a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Stable item identifier assigned by the kernel todo tool.
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

/// Recover snapshots from pre-TodosUpdated tool results.
pub(crate) fn legacy_result(name: &str, output: &serde_json::Value) -> Option<Vec<TodoItem>> {
    if !matches!(
        name,
        "todo__update_status" | "todo__update_content" | "todowrite" | "todo"
    ) {
        return None;
    }
    let value = output.get("metadata")?.get("todos")?;
    if let Ok(items) = serde_json::from_value(value.clone()) {
        return Some(items);
    }
    #[derive(Deserialize)]
    struct LegacyItem {
        content: String,
        status: String,
    }
    let items = serde_json::from_value::<Vec<LegacyItem>>(value.clone()).ok()?;
    Some(
        items
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
            .collect(),
    )
}
