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
    /// Active UI theme name when present.
    #[serde(default)]
    pub theme: Option<String>,
    /// Remaining config keys retained verbatim.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Envelope delivered over `GET /global/event` (SSE). VERIFIED wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEvent {
    /// Optional directory scope for the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Optional project payload from the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Value>,
    /// Optional workspace payload from the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    /// Typed event body (`type` + `properties`).
    pub payload: EventPayload,
}

/// The inner payload of a `GlobalEvent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPayload {
    /// Server-assigned event id when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The discriminant, e.g. `server.connected`, `session.created`, `session.next.text.delta`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Event-specific JSON properties object.
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
    /// Session id.
    pub id: String,
    /// Display title when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Parent session for subagents.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "parentID")]
    pub parent_id: Option<String>,
    /// Working directory for the session when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Remaining session fields retained verbatim.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl Session {
    /// Message id that begins a revert range, if the session is in revert state.
    #[must_use]
    pub fn revert_message_id(&self) -> Option<&str> {
        self.rest.get("revert")?.get("messageID")?.as_str()
    }
}

/// A message (the `message.updated` `info` shape). `session_id` and `time` are lifted to
/// typed fields (store keying + idle/working status); everything else stays in `rest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Message id.
    pub id: String,
    /// `user` | `assistant` (loose for W0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Owning session id.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sessionID")]
    pub session_id: Option<String>,
    /// Created/completed timestamps.
    #[serde(default)]
    pub time: MessageTime,
    /// Remaining message fields retained verbatim.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Message lifecycle timestamps. `completed` is absent until the assistant turn finishes
/// (parity: drives idle/working status in the session view).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageTime {
    /// Creation time (unix millis) when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// Completion time (unix millis) when the assistant turn finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<i64>,
}

/// An agent (`GET /agent`). `model` carries the agent's provider/model; `hidden` agents
/// are excluded from default selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    /// Agent name / id.
    pub name: String,
    /// When true, exclude from default picker selection.
    #[serde(default, deserialize_with = "bool_or_null")]
    pub hidden: bool,
    /// Remaining agent fields (including model) retained verbatim.
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
    /// Visible assistant/user text.
    Text {
        /// Text body (may stream in via deltas).
        #[serde(default)]
        text: String,
        /// Remaining part fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Model reasoning/thinking text.
    Reasoning {
        /// Reasoning body.
        #[serde(default)]
        text: String,
        /// Remaining part fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// File/media attachment part.
    File {
        /// Attachment metadata retained verbatim.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Tool invocation part.
    Tool(ToolPart),
    /// Step boundary start marker.
    StepStart {
        /// Remaining fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Step boundary finish marker.
    StepFinish {
        /// Remaining fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Snapshot part.
    Snapshot {
        /// Remaining fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Patch part.
    Patch {
        /// Remaining fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Agent-related part.
    Agent {
        /// Remaining fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Any future/unknown part type — tolerated, never breaks decoding.
    #[serde(other)]
    Unknown,
}

/// A tool invocation part (tool name + lenient state/metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPart {
    /// Tool name when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Tool state JSON (pending/running/completed/error shapes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// Remaining tool part fields.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Projected v2 timeline entry produced by the reducer (the `data` cache).
/// Variants mirror data.tsx: agent-switched, model-switched, user, system,
/// synthetic, shell, assistant, compaction.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "kebab-case")]
pub enum SessionMessage {
    /// Agent switch marker row.
    AgentSwitched {
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Model switch marker row.
    ModelSwitched {
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Visible user message row.
    User {
        /// Local/projected row id.
        id: String,
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// System message row.
    System {
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Synthetic/system-generated row.
    Synthetic {
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Shell-command row.
    Shell {
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Assistant turn row with streamed parts.
    Assistant {
        /// Local/projected row id.
        id: String,
        /// Ordered parts on this assistant row.
        #[serde(default)]
        parts: Vec<Part>,
        /// Remaining row fields.
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    /// Compaction marker row.
    Compaction {
        /// Remaining row fields.
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
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/agents.json"
        );
        let raw = std::fs::read_to_string(path).expect("tests/fixtures/agents.json missing");
        let agents: Vec<Agent> = serde_json::from_str(&raw).expect("deser agents array");
        assert!(
            agents.iter().any(|a| !a.hidden && !a.name.is_empty()),
            "a non-hidden default agent exists"
        );
    }
}
