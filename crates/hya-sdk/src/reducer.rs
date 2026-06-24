//! The v2 projected-timeline cache (`data`) and its turn-stream reducer.
//!
//! `V2Event` is the FROZEN enum shape (W0) covering the 29 `session.next.*` turn-stream
//! events from `data.tsx`. `apply` is the FROZEN reducer SIGNATURE — its full 29-arm body,
//! tested against a recorded `turn_stream.jsonl`, is the W2 deliverable. Today it is a no-op
//! skeleton, except that `prompt.admitted` is *intentionally* silent (durable inbox row;
//! the visible user message only appears on `prompt.promoted`).

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::types::{EventPayload, Part, SessionMessage, ToolPart};

/// The v2 projected timeline cache.
#[derive(Debug, Default)]
pub struct Data {
    /// session id -> ordered visible timeline (the rendered conversation, newest-first).
    pub timeline: HashMap<String, Vec<SessionMessage>>,
    next_id: u64,
}

impl Data {
    fn mint_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}_{}", self.next_id)
    }
}

/// Decoded `session.next.*` turn-stream events. FROZEN enum shape (W0).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2Event {
    /// SILENT durable inbox row — must NOT mutate the visible timeline.
    PromptAdmitted {
        session_id: String,
    },
    /// Materializes the visible user message.
    PromptPromoted {
        session_id: String,
    },
    StepStarted {
        session_id: String,
    },
    StepEnded {
        session_id: String,
    },
    StepFailed {
        session_id: String,
    },
    TextStarted {
        session_id: String,
        part_id: String,
    },
    TextDelta {
        session_id: String,
        part_id: String,
        text: String,
    },
    TextEnded {
        session_id: String,
        part_id: String,
    },
    ReasoningStarted {
        session_id: String,
        part_id: String,
    },
    ReasoningDelta {
        session_id: String,
        part_id: String,
        text: String,
    },
    ReasoningEnded {
        session_id: String,
        part_id: String,
    },
    ToolInputStarted {
        session_id: String,
        part_id: String,
    },
    ToolInputDelta {
        session_id: String,
        part_id: String,
    },
    ToolInputEnded {
        session_id: String,
        part_id: String,
    },
    ToolCalled {
        session_id: String,
        part_id: String,
    },
    ToolProgress {
        session_id: String,
        part_id: String,
    },
    ToolSuccess {
        session_id: String,
        part_id: String,
    },
    ToolFailed {
        session_id: String,
        part_id: String,
    },
    AgentSwitched {
        session_id: String,
    },
    ModelSwitched {
        session_id: String,
    },
    Prompted {
        session_id: String,
    },
    ContextUpdated {
        session_id: String,
    },
    Synthetic {
        session_id: String,
    },
    ShellStarted {
        session_id: String,
    },
    ShellEnded {
        session_id: String,
    },
    Retried {
        session_id: String,
    },
    CompactionStarted {
        session_id: String,
    },
    CompactionDelta {
        session_id: String,
    },
    CompactionEnded {
        session_id: String,
    },
    /// Tolerated unknown turn-stream event (reducer no-op).
    Unknown {
        kind: String,
    },
}

impl V2Event {
    /// Map a raw `GlobalEvent` payload into a `V2Event` when its `type` is a
    /// `session.next.*` turn-stream event; otherwise `None` (handled by other state).
    #[must_use]
    pub fn from_payload(payload: &EventPayload) -> Option<Self> {
        let kind = payload.kind.strip_prefix("session.next.")?;
        let sid = || {
            payload
                .properties
                .get("sessionID")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let pid = || {
            for key in ["textID", "reasoningID", "callID", "partID"] {
                if let Some(s) = payload
                    .properties
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                {
                    return s.to_string();
                }
            }
            String::new()
        };
        let text = || {
            for key in ["delta", "text"] {
                if let Some(s) = payload
                    .properties
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                {
                    return s.to_string();
                }
            }
            String::new()
        };
        let ev = match kind {
            "prompt.admitted" => V2Event::PromptAdmitted { session_id: sid() },
            "prompt.promoted" => V2Event::PromptPromoted { session_id: sid() },
            "step.started" => V2Event::StepStarted { session_id: sid() },
            "step.ended" => V2Event::StepEnded { session_id: sid() },
            "step.failed" => V2Event::StepFailed { session_id: sid() },
            "text.started" => V2Event::TextStarted {
                session_id: sid(),
                part_id: pid(),
            },
            "text.delta" => V2Event::TextDelta {
                session_id: sid(),
                part_id: pid(),
                text: text(),
            },
            "text.ended" => V2Event::TextEnded {
                session_id: sid(),
                part_id: pid(),
            },
            "reasoning.started" => V2Event::ReasoningStarted {
                session_id: sid(),
                part_id: pid(),
            },
            "reasoning.delta" => V2Event::ReasoningDelta {
                session_id: sid(),
                part_id: pid(),
                text: text(),
            },
            "reasoning.ended" => V2Event::ReasoningEnded {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.input.started" => V2Event::ToolInputStarted {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.input.delta" => V2Event::ToolInputDelta {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.input.ended" => V2Event::ToolInputEnded {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.called" => V2Event::ToolCalled {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.progress" => V2Event::ToolProgress {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.success" => V2Event::ToolSuccess {
                session_id: sid(),
                part_id: pid(),
            },
            "tool.failed" => V2Event::ToolFailed {
                session_id: sid(),
                part_id: pid(),
            },
            "agent.switched" => V2Event::AgentSwitched { session_id: sid() },
            "model.switched" => V2Event::ModelSwitched { session_id: sid() },
            "prompted" => V2Event::Prompted { session_id: sid() },
            "context.updated" => V2Event::ContextUpdated { session_id: sid() },
            "synthetic" => V2Event::Synthetic { session_id: sid() },
            "shell.started" => V2Event::ShellStarted { session_id: sid() },
            "shell.ended" => V2Event::ShellEnded { session_id: sid() },
            "retried" => V2Event::Retried { session_id: sid() },
            "compaction.started" => V2Event::CompactionStarted { session_id: sid() },
            "compaction.delta" => V2Event::CompactionDelta { session_id: sid() },
            "compaction.ended" => V2Event::CompactionEnded { session_id: sid() },
            other => V2Event::Unknown {
                kind: format!("session.next.{other}"),
            },
        };
        Some(ev)
    }
}

/// Apply a turn-stream event to the projected timeline (mirrors data.tsx, newest-first).
///
/// `PromptAdmitted` is a silent durable-inbox row: it never mutates the visible timeline.
pub fn apply(data: &mut Data, event: &V2Event) {
    use SessionMessage as M;
    match event {
        V2Event::PromptAdmitted { .. } => {}
        V2Event::PromptPromoted { session_id } => {
            let id = data.mint_id("usr");
            prepend(
                data,
                session_id,
                M::User {
                    id,
                    rest: Map::new(),
                },
            );
        }
        V2Event::StepStarted { session_id } => {
            let id = data.mint_id("ast");
            prepend(
                data,
                session_id,
                M::Assistant {
                    id,
                    parts: Vec::new(),
                    rest: Map::new(),
                },
            );
        }
        V2Event::TextStarted {
            session_id,
            part_id,
        } => {
            push_part(data, session_id, text_part(part_id));
        }
        V2Event::TextDelta {
            session_id,
            part_id,
            text,
        } => {
            append_delta(data, session_id, part_id, text, PartKind::Text);
        }
        V2Event::ReasoningStarted {
            session_id,
            part_id,
        } => {
            push_part(data, session_id, reasoning_part(part_id));
        }
        V2Event::ReasoningDelta {
            session_id,
            part_id,
            text,
        } => {
            append_delta(data, session_id, part_id, text, PartKind::Reasoning);
        }
        V2Event::ToolInputStarted {
            session_id,
            part_id,
        } => {
            push_part(data, session_id, tool_part(part_id, "pending"));
        }
        V2Event::ToolCalled {
            session_id,
            part_id,
        } => {
            set_tool_status(data, session_id, part_id, "running");
        }
        V2Event::ToolSuccess {
            session_id,
            part_id,
        } => {
            set_tool_status(data, session_id, part_id, "completed");
        }
        V2Event::ToolFailed {
            session_id,
            part_id,
        } => {
            set_tool_status(data, session_id, part_id, "error");
        }
        V2Event::AgentSwitched { session_id } => {
            prepend(data, session_id, M::AgentSwitched { rest: Map::new() });
        }
        V2Event::ModelSwitched { session_id } => {
            prepend(data, session_id, M::ModelSwitched { rest: Map::new() });
        }
        V2Event::Synthetic { session_id } => {
            prepend(data, session_id, M::Synthetic { rest: Map::new() });
        }
        V2Event::ContextUpdated { session_id } => {
            prepend(data, session_id, M::System { rest: Map::new() });
        }
        V2Event::ShellStarted { session_id } => {
            prepend(data, session_id, M::Shell { rest: Map::new() });
        }
        V2Event::CompactionEnded { session_id } => {
            prepend(data, session_id, M::Compaction { rest: Map::new() });
        }
        V2Event::TextEnded { .. }
        | V2Event::ReasoningEnded { .. }
        | V2Event::ToolInputDelta { .. }
        | V2Event::ToolInputEnded { .. }
        | V2Event::ToolProgress { .. }
        | V2Event::StepEnded { .. }
        | V2Event::StepFailed { .. }
        | V2Event::Prompted { .. }
        | V2Event::ShellEnded { .. }
        | V2Event::Retried { .. }
        | V2Event::CompactionStarted { .. }
        | V2Event::CompactionDelta { .. }
        | V2Event::Unknown { .. } => {}
    }
}

#[derive(Clone, Copy)]
enum PartKind {
    Text,
    Reasoning,
}

fn prepend(data: &mut Data, session_id: &str, msg: SessionMessage) {
    let list = data.timeline.entry(session_id.to_string()).or_default();
    if let Some(id) = msg_id(&msg) {
        if list.iter().any(|m| msg_id(m) == Some(id)) {
            return;
        }
    }
    list.insert(0, msg);
}

fn msg_id(msg: &SessionMessage) -> Option<&str> {
    match msg {
        SessionMessage::User { id, .. } | SessionMessage::Assistant { id, .. } => Some(id),
        _ => None,
    }
}

fn latest_assistant_parts<'a>(data: &'a mut Data, session_id: &str) -> Option<&'a mut Vec<Part>> {
    data.timeline
        .get_mut(session_id)?
        .iter_mut()
        .find_map(|m| match m {
            SessionMessage::Assistant { parts, .. } => Some(parts),
            _ => None,
        })
}

fn push_part(data: &mut Data, session_id: &str, part: Part) {
    if let Some(parts) = latest_assistant_parts(data, session_id) {
        parts.push(part);
    }
}

fn id_rest(part_id: &str) -> Map<String, Value> {
    let mut rest = Map::new();
    rest.insert("id".to_string(), Value::String(part_id.to_string()));
    rest
}

fn text_part(part_id: &str) -> Part {
    Part::Text {
        text: String::new(),
        rest: id_rest(part_id),
    }
}

fn reasoning_part(part_id: &str) -> Part {
    Part::Reasoning {
        text: String::new(),
        rest: id_rest(part_id),
    }
}

fn tool_part(part_id: &str, status: &str) -> Part {
    Part::Tool(ToolPart {
        tool: None,
        state: Some(json!({ "status": status })),
        rest: id_rest(part_id),
    })
}

fn part_matches(rest: &Map<String, Value>, id: &str) -> bool {
    rest.get("id").and_then(Value::as_str) == Some(id)
}

fn append_delta(data: &mut Data, session_id: &str, part_id: &str, delta: &str, kind: PartKind) {
    let Some(parts) = latest_assistant_parts(data, session_id) else {
        return;
    };
    for part in parts.iter_mut() {
        match (kind, part) {
            (PartKind::Text, Part::Text { text, rest }) if part_matches(rest, part_id) => {
                text.push_str(delta);
                return;
            }
            (PartKind::Reasoning, Part::Reasoning { text, rest })
                if part_matches(rest, part_id) =>
            {
                text.push_str(delta);
                return;
            }
            _ => {}
        }
    }
}

fn set_tool_status(data: &mut Data, session_id: &str, part_id: &str, status: &str) {
    let Some(parts) = latest_assistant_parts(data, session_id) else {
        return;
    };
    for part in parts.iter_mut() {
        if let Part::Tool(tool) = part {
            if part_matches(&tool.rest, part_id) {
                tool.state = Some(json!({ "status": status }));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EventPayload;

    fn payload(kind: &str, props: serde_json::Value) -> EventPayload {
        EventPayload {
            id: None,
            kind: kind.to_string(),
            properties: props,
        }
    }

    #[test]
    fn non_turn_stream_event_maps_to_none() {
        assert!(V2Event::from_payload(&payload("server.connected", json!({}))).is_none());
        assert!(V2Event::from_payload(&payload("session.created", json!({}))).is_none());
    }

    #[test]
    fn admission_pair_maps_correctly() {
        let admitted = V2Event::from_payload(&payload(
            "session.next.prompt.admitted",
            json!({ "sessionID": "ses_1" }),
        ))
        .unwrap();
        assert_eq!(
            admitted,
            V2Event::PromptAdmitted {
                session_id: "ses_1".into()
            }
        );

        let promoted = V2Event::from_payload(&payload(
            "session.next.prompt.promoted",
            json!({ "sessionID": "ses_1" }),
        ))
        .unwrap();
        assert_eq!(
            promoted,
            V2Event::PromptPromoted {
                session_id: "ses_1".into()
            }
        );
    }

    #[test]
    fn text_delta_carries_text() {
        let ev = V2Event::from_payload(&payload(
            "session.next.text.delta",
            json!({ "sessionID": "ses_1", "partID": "prt_1", "text": "hello" }),
        ))
        .unwrap();
        assert_eq!(
            ev,
            V2Event::TextDelta {
                session_id: "ses_1".into(),
                part_id: "prt_1".into(),
                text: "hello".into()
            }
        );
    }

    #[test]
    fn admitted_is_silent_in_reducer() {
        let mut data = Data::default();
        apply(
            &mut data,
            &V2Event::PromptAdmitted {
                session_id: "ses_1".into(),
            },
        );
        assert!(
            data.timeline.is_empty(),
            "prompt.admitted must not mutate the visible timeline"
        );
    }

    #[test]
    fn unknown_turn_stream_event_tolerated() {
        let ev = V2Event::from_payload(&payload("session.next.brand.new", json!({}))).unwrap();
        assert!(matches!(ev, V2Event::Unknown { .. }));
    }

    #[test]
    fn all_29_events() {
        use crate::types::GlobalEvent;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/turn_stream.jsonl"
        );
        let raw = std::fs::read_to_string(path)
            .expect("fixtures/turn_stream.jsonl missing — see PLAN.md W2");
        let mut data = Data::default();
        let mut applied = 0usize;
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let global: GlobalEvent = serde_json::from_str(line).expect("parse turn_stream line");
            if global.is_sync_envelope() || global.is_heartbeat() {
                continue;
            }
            if let Some(ev) = V2Event::from_payload(&global.payload) {
                apply(&mut data, &ev);
                applied += 1;
            }
        }
        assert!(
            applied >= 20,
            "fixture should exercise many arms, applied {applied}"
        );

        let timeline = data.timeline.get("ses_t").expect("ses_t timeline built");
        assert!(
            timeline
                .iter()
                .any(|m| matches!(m, SessionMessage::User { .. })),
            "prompt.promoted materializes a user message"
        );
        assert!(
            timeline
                .iter()
                .any(|m| matches!(m, SessionMessage::Compaction { .. })),
            "compaction.ended inserts a compaction entry"
        );

        let parts = timeline
            .iter()
            .find_map(|m| match m {
                SessionMessage::Assistant { parts, .. } => Some(parts),
                _ => None,
            })
            .expect("assistant message present");
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, Part::Text { text, .. } if text == "Hello, world")),
            "text deltas accumulate into the assistant text part"
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, Part::Reasoning { text, .. } if text == "thinking")),
            "reasoning deltas accumulate"
        );
        assert!(
            parts.iter().any(|p| matches!(p, Part::Tool(t)
                if t.state.as_ref().and_then(|s| s.get("status")).and_then(Value::as_str) == Some("completed"))),
            "tool reaches completed via input.started -> called -> success"
        );

        assert!(
            !data.timeline.contains_key("ses_a"),
            "a session with only prompt.admitted has no visible timeline"
        );
    }
}
