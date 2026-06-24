//! Wire types for the backend server.
//!
//! `GlobalEvent`/`EventPayload` are STRICT and verified against real `GET /global/event`
//! output (PLAN.md: `data: {"payload":{"id":..,"type":"server.connected","properties":{}}}`).
//! `Session`/`Message`/`Part`/`ToolPart`/`SessionMessage` are contract shells for W0: the
//! discriminants are fixed, deep field typing lands in W2/W6 (kept lenient via `#[serde(flatten)]`
//! so unknown fields never break decoding).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Server config. The full schema is large; we keep `theme` typed (used by the theme system)
/// and retain everything else verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Envelope delivered over `GET /global/event` (SSE). VERIFIED wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    pub payload: EventPayload,
}

/// The inner payload of a `GlobalEvent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The discriminant, e.g. `server.connected`, `session.created`, `session.next.text.delta`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub properties: Value,
}

impl GlobalEvent {
    /// The TUI drops sync-envelope events (`payload.type == "sync"`).
    #[must_use]
    pub fn is_sync_envelope(&self) -> bool {
        self.payload.kind == "sync"
    }

    /// Heartbeats (`server.heartbeat`, ~every 10s) are ignored by the UI.
    #[must_use]
    pub fn is_heartbeat(&self) -> bool {
        self.payload.kind == "server.heartbeat"
    }
}

/// A session (v1 cache shape). Contract shell for W0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "parentID")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl Session {
    #[must_use]
    pub fn revert_message_id(&self) -> Option<&str> {
        self.rest.get("revert")?.get("messageID")?.as_str()
    }
}

/// A message (the `message.updated` `info` shape). `session_id` and `time` are lifted to
/// typed fields (store keying + idle/working status); everything else stays in `rest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    /// `user` | `assistant` (loose for W0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sessionID")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub time: MessageTime,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Message lifecycle timestamps. `completed` is absent until the assistant turn finishes
/// (parity: drives idle/working status in the session view).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageTime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<i64>,
}

/// An agent (`GET /agent`). `model` carries the agent's provider/model; `hidden` agents
/// are excluded from default selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    #[serde(default, deserialize_with = "bool_or_null")]
    pub hidden: bool,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

fn bool_or_null<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

/// A message part. Discriminant is fixed (`type`); payloads are lenient for W0.
/// Variants mirror the TS `Part` union: text, reasoning, file, tool, step-start,
/// step-finish, snapshot, patch, agent.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Part {
    Text {
        #[serde(default)]
        text: String,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Reasoning {
        #[serde(default)]
        text: String,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    File {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Tool(ToolPart),
    StepStart {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    StepFinish {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Snapshot {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Patch {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Agent {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Any future/unknown part type — tolerated, never breaks decoding.
    #[serde(other)]
    Unknown,
}

/// A tool invocation part. The per-tool rendering (14 renderers) is W6; here we
/// freeze the shape (tool name + lenient state/metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Projected v2 timeline entry produced by the reducer (the `data` cache).
/// Variants mirror data.tsx: agent-switched, model-switched, user, system,
/// synthetic, shell, assistant, compaction. Field detail is W2/W6.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "kebab-case")]
pub enum SessionMessage {
    AgentSwitched {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    ModelSwitched {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    User {
        id: String,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    System {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Synthetic {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Shell {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Assistant {
        id: String,
        #[serde(default)]
        parts: Vec<Part>,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    Compaction {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-2 serde golden: the EXACT verified `/global/event` first frame must round-trip.
    #[test]
    fn global_event_server_connected_round_trips() {
        let raw = r#"{"payload":{"id":"evt_eefcfbfa1001G4eSYet2EoEJON","type":"server.connected","properties":{}}}"#;
        let ev: GlobalEvent = serde_json::from_str(raw).expect("decode verified frame");
        assert_eq!(ev.payload.kind, "server.connected");
        assert_eq!(
            ev.payload.id.as_deref(),
            Some("evt_eefcfbfa1001G4eSYet2EoEJON")
        );
        assert!(!ev.is_sync_envelope());
        assert!(!ev.is_heartbeat());
        // round-trip back to JSON and re-decode
        let back = serde_json::to_string(&ev).expect("encode");
        let ev2: GlobalEvent = serde_json::from_str(&back).expect("re-decode");
        assert_eq!(ev2.payload.kind, ev.payload.kind);
    }

    #[test]
    fn sync_envelope_and_heartbeat_detected() {
        let sync: GlobalEvent =
            serde_json::from_str(r#"{"payload":{"type":"sync","properties":{}}}"#).unwrap();
        assert!(sync.is_sync_envelope());
        let hb: GlobalEvent =
            serde_json::from_str(r#"{"payload":{"type":"server.heartbeat","properties":{}}}"#)
                .unwrap();
        assert!(hb.is_heartbeat());
    }

    #[test]
    fn unknown_part_type_is_tolerated() {
        let p: Part = serde_json::from_str(r#"{"type":"brand-new-part","foo":1}"#).unwrap();
        assert!(matches!(p, Part::Unknown));
    }

    #[test]
    fn text_part_decodes() {
        let p: Part = serde_json::from_str(r#"{"type":"text","text":"hi"}"#).unwrap();
        match p {
            Part::Text { text, .. } => assert_eq!(text, "hi"),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn agent_decodes_with_model_and_hidden() {
        let agent: Agent = serde_json::from_str(
            r#"{"name":"build","hidden":false,"model":{"modelID":"m","providerID":"p"},"options":{}}"#,
        )
        .unwrap();
        assert_eq!(agent.name, "build");
        assert!(!agent.hidden);
    }

    #[test]
    fn agent_without_name_is_rejected() {
        assert!(serde_json::from_str::<Agent>(r#"{"hidden":true}"#).is_err());
    }

    #[test]
    fn agents_fixture_yields_a_non_hidden_default() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/agents.json");
        let raw = std::fs::read_to_string(path).expect("fixtures/agents.json missing");
        let agents: Vec<Agent> = serde_json::from_str(&raw).expect("deser agents array");
        assert!(
            agents.iter().any(|a| !a.hidden && !a.name.is_empty()),
            "a non-hidden default agent exists"
        );
    }
}
