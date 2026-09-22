//! In-memory per-session TODO state exposed to tool implementations.
//!
//! The plane owns stable item ids: adds draw sequential string ids ("1",
//! "2", …) from a per-session counter that never reuses values. Write tools
//! mutate the list under the plane's lock.

use std::collections::HashMap;
use std::sync::Arc;

use hya_proto::SessionId;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
