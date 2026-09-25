//! In-memory per-session TODO state exposed to tool implementations.
//!
//! The plane owns stable item ids: adds draw sequential string ids ("1",
//! "2", …) from a per-session counter that never reuses values. Write tools
//! mutate the list under the plane's lock.

use std::collections::HashMap;
use std::sync::Arc;

use hya_proto::SessionId;
use tokio::sync::Mutex;

pub use hya_proto::{TodoItem, TodoStatus};

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

    /// Restore a session's list recorded on its log (after a restart the
    /// plane starts empty) unless this process already holds state for it.
    /// The id counter resumes after the highest numeric id, so restored ids
    /// are never drawn again.
    pub async fn restore(&self, session: SessionId, items: Vec<TodoItem>) {
        let mut guard = self.todos.lock().await;
        if guard.contains_key(&session) || items.is_empty() {
            return;
        }
        let next_id = items
            .iter()
            .filter_map(|item| item.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        guard.insert(session, SessionTodos { items, next_id });
    }
}
