use std::collections::HashSet;

use yaca_proto::{Event, SessionId};
use yaca_tool::TodoItem;

use super::SessionEngine;

impl SessionEngine {
    pub async fn todos(&self, session: SessionId) -> Vec<TodoItem> {
        let mut latest = None;
        let mut todo_calls = HashSet::new();
        let Ok(envelopes) = self.store.replay(session).await else {
            return self.todo.get(session).await;
        };
        for envelope in envelopes {
            match envelope.event {
                Event::ToolCallRequested { call, name, .. }
                    if matches!(name.as_str(), "todowrite" | "todo") =>
                {
                    todo_calls.insert(call);
                }
                Event::ToolResult { call, output, .. } if todo_calls.contains(&call) => {
                    if let Some(value) = output
                        .get("metadata")
                        .and_then(|metadata| metadata.get("todos"))
                        && let Ok(todos) = serde_json::from_value::<Vec<TodoItem>>(value.clone())
                    {
                        latest = Some(todos);
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
