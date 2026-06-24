//! The live message/part store (parity with `packages/tui/src/context/sync.tsx`).
//!
//! The visible session timeline is built from the top-level `message.updated` /
//! `message.part.updated` / `message.part.delta` events — NOT the `session.next.*`
//! turn-stream (that is the durable prompt-admission inbox, parked in `reducer.rs`).
//! `sync` envelopes are event-sourced replay duplicates and are ignored here.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::types::{GlobalEvent, Message, Part, Session};

const MESSAGE_CAP: usize = 100;

/// A message part plus the keys the store sorts/indexes on (lifted out of the JSON
/// so `binary_search` never pays a map lookup per compare).
#[derive(Debug, Clone)]
pub struct StoredPart {
    pub id: String,
    pub message_id: String,
    pub inner: Part,
}

impl StoredPart {
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.to_string();
        let message_id = value.get("messageID")?.as_str()?.to_string();
        let inner = serde_json::from_value::<Part>(value.clone()).ok()?;
        Some(Self {
            id,
            message_id,
            inner,
        })
    }
}

/// Live conversation store: `sessionID -> messages` and `messageID -> parts`, each kept
/// id-sorted. Backend IDs are ULID-like (time-prefixed), so lexical order is chronological.
#[derive(Debug, Default)]
pub struct MessageStore {
    pub messages: HashMap<String, Vec<Message>>,
    pub parts: HashMap<String, Vec<StoredPart>>,
    pub sessions: HashMap<String, Session>,
    pub todos: HashMap<String, Vec<Value>>,
    pub diffs: HashMap<String, Vec<Value>>,
    pub permissions: HashMap<String, Vec<Value>>,
    pub questions: HashMap<String, Vec<Value>>,
}

impl MessageStore {
    /// Apply one decoded SSE event. Returns `true` if it mutated the store.
    pub fn apply_event(&mut self, event: &GlobalEvent) -> bool {
        let props = &event.payload.properties;
        match event.payload.kind.as_str() {
            "message.updated" => match props.get("info").cloned() {
                Some(info) => match serde_json::from_value::<Message>(info) {
                    Ok(message) => {
                        self.upsert_message(message);
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            },
            "message.part.updated" => match props.get("part").and_then(StoredPart::from_value) {
                Some(part) => {
                    self.upsert_part(part);
                    true
                }
                None => false,
            },
            "message.part.delta" => {
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                let Some(part_id) = str_field(props, "partID") else {
                    return false;
                };
                let field = str_field(props, "field").unwrap_or("text");
                let delta = str_field(props, "delta").unwrap_or_default();
                self.apply_delta(message_id, part_id, field, delta)
            }
            "message.removed" => {
                let Some(session_id) = str_field(props, "sessionID") else {
                    return false;
                };
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                self.remove_message(session_id, message_id);
                true
            }
            "message.part.removed" => {
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                let Some(part_id) = str_field(props, "partID") else {
                    return false;
                };
                self.remove_part(message_id, part_id);
                true
            }
            "session.created" | "session.updated" => match props.get("info").cloned() {
                Some(info) => match serde_json::from_value::<Session>(info) {
                    Ok(session) => {
                        self.sessions.insert(session.id.clone(), session);
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            },
            "todo.updated" => match (str_field(props, "sessionID"), props.get("todos")) {
                (Some(session_id), Some(Value::Array(todos))) => {
                    self.todos.insert(session_id.to_string(), todos.clone());
                    true
                }
                _ => false,
            },
            "session.diff" => match (str_field(props, "sessionID"), props.get("diff")) {
                (Some(session_id), Some(Value::Array(diff))) => {
                    self.diffs.insert(session_id.to_string(), diff.clone());
                    true
                }
                _ => false,
            },
            "permission.asked" => match str_field(props, "sessionID") {
                Some(session_id) => {
                    self.upsert_permission(session_id.to_string(), props.clone());
                    true
                }
                None => false,
            },
            "permission.replied" => {
                match (str_field(props, "sessionID"), str_field(props, "requestID")) {
                    (Some(session_id), Some(request_id)) => {
                        self.remove_permission(session_id, request_id);
                        true
                    }
                    _ => false,
                }
            }
            "question.asked" => match str_field(props, "sessionID") {
                Some(session_id) => {
                    self.upsert_question(session_id.to_string(), props.clone());
                    true
                }
                None => false,
            },
            "question.replied" | "question.rejected" => {
                match (str_field(props, "sessionID"), str_field(props, "requestID")) {
                    (Some(session_id), Some(request_id)) => {
                        self.remove_question(session_id, request_id);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn todos(&self, session_id: &str) -> &[Value] {
        self.todos.get(session_id).map_or(&[][..], Vec::as_slice)
    }

    #[must_use]
    pub fn diffs(&self, session_id: &str) -> &[Value] {
        self.diffs.get(session_id).map_or(&[][..], Vec::as_slice)
    }

    #[must_use]
    pub fn permissions(&self, session_id: &str) -> &[Value] {
        self.permissions
            .get(session_id)
            .map_or(&[][..], Vec::as_slice)
    }

    #[must_use]
    pub fn questions(&self, session_id: &str) -> &[Value] {
        self.questions
            .get(session_id)
            .map_or(&[][..], Vec::as_slice)
    }

    fn upsert_permission(&mut self, session_id: String, request: Value) {
        let id = request.get("id").and_then(Value::as_str).map(str::to_owned);
        let list = self.permissions.entry(session_id).or_default();
        let existing = id.and_then(|id| {
            list.iter()
                .position(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
        });
        match existing {
            Some(index) => list[index] = request,
            None => list.push(request),
        }
    }

    fn remove_permission(&mut self, session_id: &str, request_id: &str) {
        if let Some(list) = self.permissions.get_mut(session_id) {
            list.retain(|r| r.get("id").and_then(Value::as_str) != Some(request_id));
            if list.is_empty() {
                self.permissions.remove(session_id);
            }
        }
    }

    fn upsert_question(&mut self, session_id: String, request: Value) {
        let id = request.get("id").and_then(Value::as_str).map(str::to_owned);
        let list = self.questions.entry(session_id).or_default();
        let existing = id.and_then(|id| {
            list.iter()
                .position(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
        });
        match existing {
            Some(index) => list[index] = request,
            None => list.push(request),
        }
    }

    fn remove_question(&mut self, session_id: &str, request_id: &str) {
        if let Some(list) = self.questions.get_mut(session_id) {
            list.retain(|r| r.get("id").and_then(Value::as_str) != Some(request_id));
            if list.is_empty() {
                self.questions.remove(session_id);
            }
        }
    }

    #[must_use]
    pub fn session(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    #[must_use]
    pub fn child_sessions(&self, parent_id: &str) -> Vec<&Session> {
        let mut children: Vec<&Session> = self
            .sessions
            .values()
            .filter(|session| session.parent_id.as_deref() == Some(parent_id))
            .collect();
        children.sort_by(|left, right| left.id.cmp(&right.id));
        children
    }

    #[must_use]
    pub fn is_working(&self, session_id: &str) -> bool {
        let Some(last) = self.messages.get(session_id).and_then(|list| list.last()) else {
            return false;
        };
        match last.role.as_deref() {
            Some("user") => true,
            Some("assistant") => last.time.completed.is_none(),
            _ => false,
        }
    }

    pub fn upsert_message(&mut self, message: Message) {
        let Some(session_id) = message.session_id.clone() else {
            return;
        };
        let list = self.messages.entry(session_id.clone()).or_default();
        match list.binary_search_by(|m| m.id.as_str().cmp(&message.id)) {
            Ok(index) => list[index] = message,
            Err(index) => list.insert(index, message),
        }
        self.enforce_cap(&session_id);
    }

    pub fn upsert_part(&mut self, part: StoredPart) {
        let list = self.parts.entry(part.message_id.clone()).or_default();
        match list.binary_search_by(|p| p.id.as_str().cmp(&part.id)) {
            Ok(index) => list[index] = part,
            Err(index) => list.insert(index, part),
        }
    }

    fn apply_delta(&mut self, message_id: &str, part_id: &str, field: &str, delta: &str) -> bool {
        let Some(list) = self.parts.get_mut(message_id) else {
            return false;
        };
        let Ok(index) = list.binary_search_by(|p| p.id.as_str().cmp(part_id)) else {
            return false;
        };
        apply_field_delta(&mut list[index].inner, field, delta);
        true
    }

    fn remove_message(&mut self, session_id: &str, message_id: &str) {
        if let Some(list) = self.messages.get_mut(session_id) {
            if let Ok(index) = list.binary_search_by(|m| m.id.as_str().cmp(message_id)) {
                list.remove(index);
            }
        }
        self.parts.remove(message_id);
    }

    fn remove_part(&mut self, message_id: &str, part_id: &str) {
        if let Some(list) = self.parts.get_mut(message_id) {
            if let Ok(index) = list.binary_search_by(|p| p.id.as_str().cmp(part_id)) {
                list.remove(index);
            }
        }
    }

    fn enforce_cap(&mut self, session_id: &str) {
        let mut dropped = Vec::new();
        if let Some(list) = self.messages.get_mut(session_id) {
            while list.len() > MESSAGE_CAP {
                dropped.push(list.remove(0).id);
            }
        }
        for message_id in dropped {
            self.parts.remove(&message_id);
        }
    }
}

fn str_field<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn apply_field_delta(part: &mut Part, field: &str, delta: &str) {
    // Parity: the TS reducer does `part[field] += delta`. text/reasoning stream into `.text`;
    // tool parts stream into other fields (input/output) which we stash in `rest` for the
    // W6 tool renderers. Unknown (part, field) pairs are dropped.
    match part {
        Part::Text { text, .. } | Part::Reasoning { text, .. } if field == "text" => {
            text.push_str(delta);
        }
        Part::Tool(tool) => stash_delta(&mut tool.rest, field, delta),
        _ => {}
    }
}

fn stash_delta(rest: &mut Map<String, Value>, field: &str, delta: &str) {
    if let Value::String(existing) = rest
        .entry(field.to_string())
        .or_insert_with(|| Value::String(String::new()))
    {
        existing.push_str(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, session_id: &str, role: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "id": id, "sessionID": session_id, "role": role, "time": {"created": 1}
        }))
        .unwrap()
    }

    fn text_part(id: &str, message_id: &str, text: &str) -> StoredPart {
        StoredPart::from_value(&serde_json::json!({
            "id": id, "messageID": message_id, "type": "text", "text": text
        }))
        .unwrap()
    }

    #[test]
    fn message_updated_upserts_by_id_in_order() {
        let mut store = MessageStore::default();
        store.upsert_message(message("msg_2", "ses_1", "assistant"));
        store.upsert_message(message("msg_1", "ses_1", "user"));
        let list = &store.messages["ses_1"];
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "msg_1");
        assert_eq!(list[1].id, "msg_2");
        store.upsert_message(message("msg_1", "ses_1", "user"));
        assert_eq!(store.messages["ses_1"].len(), 2, "re-upsert dedupes by id");
    }

    #[test]
    fn child_sessions_returns_children_sorted_by_id() {
        let event = |info: serde_json::Value| -> GlobalEvent {
            serde_json::from_value(serde_json::json!({
                "payload": { "type": "session.created", "properties": { "info": info } }
            }))
            .unwrap()
        };
        let mut store = MessageStore::default();
        store.apply_event(&event(serde_json::json!({ "id": "ses_parent" })));
        store.apply_event(&event(
            serde_json::json!({ "id": "ses_child_b", "parentID": "ses_parent" }),
        ));
        store.apply_event(&event(
            serde_json::json!({ "id": "ses_child_a", "parentID": "ses_parent" }),
        ));
        store.apply_event(&event(serde_json::json!({ "id": "ses_unrelated" })));

        let children = store.child_sessions("ses_parent");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id, "ses_child_a");
        assert_eq!(children[1].id, "ses_child_b");
        assert!(store.child_sessions("ses_unrelated").is_empty());
    }

    #[test]
    fn part_delta_appends_to_text_field() {
        let mut store = MessageStore::default();
        store.upsert_part(text_part("prt_1", "msg_1", ""));
        assert!(store.apply_delta("msg_1", "prt_1", "text", "hel"));
        assert!(store.apply_delta("msg_1", "prt_1", "text", "lo"));
        match &store.parts["msg_1"][0].inner {
            Part::Text { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn part_delta_before_part_updated_is_dropped() {
        let mut store = MessageStore::default();
        assert!(!store.apply_delta("msg_1", "prt_1", "text", "x"));
        assert!(!store.parts.contains_key("msg_1"));
    }

    #[test]
    fn is_working_reflects_last_message() {
        let mut store = MessageStore::default();
        assert!(!store.is_working("ses_1"), "no messages => idle");
        store.upsert_message(message("msg_1", "ses_1", "user"));
        assert!(store.is_working("ses_1"), "last message is user => working");
        store.upsert_message(message("msg_2", "ses_1", "assistant"));
        assert!(
            store.is_working("ses_1"),
            "assistant without completed time => working"
        );
        let completed: Message = serde_json::from_value(serde_json::json!({
            "id": "msg_2", "sessionID": "ses_1", "role": "assistant",
            "time": { "created": 1, "completed": 2 }
        }))
        .unwrap();
        store.upsert_message(completed);
        assert!(!store.is_working("ses_1"), "assistant completed => idle");
    }

    #[test]
    fn todo_and_diff_events_populate_sidebar_stores() {
        let mut store = MessageStore::default();
        let todo_event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "todo.updated", "properties": {
                "sessionID": "ses_1",
                "todos": [{ "content": "write tests", "status": "in_progress" }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&todo_event));
        assert_eq!(store.todos("ses_1").len(), 1);

        let diff_event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "session.diff", "properties": {
                "sessionID": "ses_1",
                "diff": [{ "file": "src/main.rs", "additions": 3, "deletions": 1 }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&diff_event));
        assert_eq!(store.diffs("ses_1").len(), 1);
        assert_eq!(store.diffs("ses_2").len(), 0);
    }

    #[test]
    fn permission_asked_then_replied_lifecycle() {
        let mut store = MessageStore::default();
        let asked: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "permission.asked", "properties": {
                "id": "per_1", "sessionID": "ses_1", "permission": "edit",
                "patterns": ["src/main.rs"], "metadata": { "filepath": "src/main.rs" }, "always": []
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&asked));
        assert_eq!(store.permissions("ses_1").len(), 1);
        assert_eq!(
            store.permissions("ses_1")[0]
                .get("permission")
                .and_then(Value::as_str),
            Some("edit")
        );

        let replied: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "permission.replied", "properties": {
                "sessionID": "ses_1", "requestID": "per_1"
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&replied));
        assert_eq!(store.permissions("ses_1").len(), 0);
    }

    #[test]
    fn question_asked_then_rejected_lifecycle() {
        let mut store = MessageStore::default();
        let asked: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "question.asked", "properties": {
                "id": "que_1", "sessionID": "ses_1",
                "questions": [{
                    "header": "Mode", "question": "Which mode?",
                    "options": [{ "label": "Fast", "description": "Quick answer" }],
                    "multiple": false, "custom": true
                }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&asked));
        assert_eq!(store.questions("ses_1").len(), 1);
        assert_eq!(
            store.questions("ses_1")[0]
                .get("questions")
                .and_then(Value::as_array)
                .and_then(|questions| questions.first())
                .and_then(|question| question.get("header"))
                .and_then(Value::as_str),
            Some("Mode")
        );

        let rejected: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "question.rejected", "properties": {
                "sessionID": "ses_1", "requestID": "que_1"
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&rejected));
        assert_eq!(store.questions("ses_1").len(), 0);
    }

    #[test]
    fn session_updated_captures_title() {
        let mut store = MessageStore::default();
        let event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "session.updated", "properties": {
                "info": { "id": "ses_1", "title": "My Session" }
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&event));
        assert_eq!(
            store.session("ses_1").and_then(|s| s.title.as_deref()),
            Some("My Session")
        );
    }

    #[test]
    fn cap_100_drops_oldest_message_and_its_parts() {
        let mut store = MessageStore::default();
        for index in 0..101 {
            let id = format!("msg_{index:04}");
            store.upsert_message(message(&id, "ses_1", "user"));
            store.upsert_part(text_part(&format!("prt_{index:04}"), &id, "x"));
        }
        let list = &store.messages["ses_1"];
        assert_eq!(list.len(), MESSAGE_CAP);
        assert_eq!(list[0].id, "msg_0001", "oldest message dropped");
        assert!(
            !store.parts.contains_key("msg_0000"),
            "dropped message's parts removed"
        );
    }

    #[test]
    fn replay_live_capture_builds_user_and_assistant_text() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/live_session_turn.jsonl"
        );
        let raw = std::fs::read_to_string(path).expect("fixtures/live_session_turn.jsonl missing");
        let mut store = MessageStore::default();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let event: GlobalEvent = serde_json::from_str(line).expect("parse fixture line");
            if event.is_sync_envelope() || event.is_heartbeat() {
                continue;
            }
            store.apply_event(&event);
        }
        let sessions: Vec<_> = store.messages.keys().cloned().collect();
        assert_eq!(sessions.len(), 1, "one session in the capture");
        let messages = &store.messages[&sessions[0]];
        assert!(
            messages.iter().any(|m| m.role.as_deref() == Some("user")),
            "user message present"
        );

        let assistant = messages
            .iter()
            .find(|m| m.role.as_deref() == Some("assistant"))
            .expect("assistant message present");
        let parts = &store.parts[&assistant.id];
        let assistant_text: String = parts
            .iter()
            .filter_map(|p| match &p.inner {
                Part::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            assistant_text.contains("hello there friend"),
            "assistant streamed reply accumulated, got {assistant_text:?}"
        );
    }
}
