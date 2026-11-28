//! `hya/compaction`'s `Event::Extension` records: `context_evicted` and
//! `context_status`, with their wire payloads.

use hya_context::tokens::{TokenAccountingMode, TokenSource};
use hya_proto::{Event, PartId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The bundle id the records are persisted and folded under.
pub const BUNDLE_COMPACTION: &str = "hya/compaction";

const CONTEXT_EVICTED: &str = "context_evicted";
const CONTEXT_STATUS: &str = "context_status";

/// One tool output replaced during eviction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvictedPart {
    /// Evicted tool part id.
    pub part: PartId,
    /// Replacement notice text sent in place of the output.
    pub notice: String,
}

/// Latest window-occupancy report, mirroring the newest `context_status` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextStatusProjection {
    /// Tokens the last measured request is believed to occupy.
    pub tokens: u64,
    /// Whether the count was provider-anchored or locally estimated.
    pub source: TokenSource,
    /// Accounting mode in force for that round.
    pub mode: TokenAccountingMode,
    /// Resolved compaction threshold the count was judged against.
    pub threshold: u64,
}

/// `context_evicted` payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextEvicted {
    /// Number of tool outputs evicted in this pass.
    pub evicted_parts: u32,
    /// The evicted parts and their notices.
    pub parts: Vec<EvictedPart>,
    /// Estimated occupancy before eviction.
    pub tokens_before: u64,
    /// Estimated occupancy after eviction.
    pub tokens_after: u64,
    /// Threshold active during eviction.
    pub threshold: u64,
}

/// A decoded `hya/compaction` record.
#[derive(Clone, Debug, PartialEq)]
pub enum CompactionEvent {
    /// Tool outputs were spilled and replaced by notices.
    ContextEvicted(ContextEvicted),
    /// The round's window occupancy.
    ContextStatus(ContextStatusProjection),
}

impl CompactionEvent {
    /// Encode as `session`'s [`Event::Extension`].
    #[must_use]
    pub fn into_event(self, session: SessionId) -> Event {
        let (kind, payload) = match self {
            Self::ContextEvicted(evicted) => (CONTEXT_EVICTED, serde_json::to_value(evicted)),
            Self::ContextStatus(status) => (CONTEXT_STATUS, serde_json::to_value(status)),
        };
        Event::Extension {
            session,
            bundle: BUNDLE_COMPACTION.to_string(),
            kind: kind.to_string(),
            payload: payload.unwrap_or(Value::Null),
        }
    }

    /// Decode a record of this bundle; `None` for another bundle, an unknown
    /// kind, or a payload that does not decode.
    #[must_use]
    pub fn from_parts(bundle: &str, kind: &str, payload: &Value) -> Option<Self> {
        if bundle != BUNDLE_COMPACTION {
            return None;
        }
        match kind {
            CONTEXT_EVICTED => serde_json::from_value(payload.clone())
                .ok()
                .map(Self::ContextEvicted),
            CONTEXT_STATUS => serde_json::from_value(payload.clone())
                .ok()
                .map(Self::ContextStatus),
            _ => None,
        }
    }
}
