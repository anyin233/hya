//! Session todo rows: the shape the todo tools write, the
//! `TodosUpdated` event records, and the projection folds.

use serde::{Deserialize, Serialize};

/// One todo row stored for a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Stable item identifier assigned by the todo plane (never reused).
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
