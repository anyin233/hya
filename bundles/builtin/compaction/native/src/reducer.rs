//! ABI v2 [`ProjectionReducer`] for `hya/compaction` `Event::Extension` records.

use std::collections::BTreeMap;

use hya_runtime_abi::{ExtensionEventContext, HostError, ProjectionReducer, ToolOutputOverride};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::events::{
    BUNDLE_COMPACTION, CompactionEvent, ContextEvicted, ContextStatusProjection, EvictedPart,
};

/// Compact, deterministic state folded under `Projection::extensions["hya/compaction"]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReducerState {
    /// Latest window-occupancy report from `context_status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_status: Option<ContextStatusProjection>,
    /// Evicted tool part notices keyed by `PartId` string.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evicted_parts: BTreeMap<String, String>,
    /// Latest `context_evicted` pass summary, when any eviction occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_eviction: Option<LastEvictionSummary>,
}

/// Summary metrics of the most recent `context_evicted` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastEvictionSummary {
    /// Number of tool outputs evicted in this pass.
    pub evicted_parts: u32,
    /// Estimated token occupancy before eviction.
    pub tokens_before: u64,
    /// Estimated token occupancy after eviction.
    pub tokens_after: u64,
    /// Compaction threshold active during eviction.
    pub threshold: u64,
}

/// ABI v2 [`ProjectionReducer`] folding `hya/compaction` `Event::Extension` records.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactionProjectionReducer;

impl ProjectionReducer for CompactionProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_COMPACTION
    }

    fn initial_state(&self) -> Value {
        json!({})
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        let Some(ext) = CompactionEvent::from_parts(&event.bundle, &event.kind, &event.payload)
        else {
            return Ok(());
        };

        let mut current: CompactionReducerState = if state.is_null() {
            CompactionReducerState::default()
        } else {
            serde_json::from_value(state.clone()).unwrap_or_default()
        };

        match ext {
            CompactionEvent::ContextStatus(status) => {
                current.context_status = Some(status);
            }
            CompactionEvent::ContextEvicted(ContextEvicted {
                evicted_parts,
                parts,
                tokens_before,
                tokens_after,
                threshold,
            }) => {
                for EvictedPart { part, notice } in parts {
                    current.evicted_parts.insert(part.to_string(), notice);
                }
                current.last_eviction = Some(LastEvictionSummary {
                    evicted_parts,
                    tokens_before,
                    tokens_after,
                    threshold,
                });
            }
        }

        *state = serde_json::to_value(current)
            .map_err(|error| HostError::Internal(format!("serialize compaction state: {error}")))?;
        Ok(())
    }

    /// Every `context_evicted` notice replaces its tool part's output in later
    /// requests.
    fn tool_output_overrides(&self, event: &ExtensionEventContext) -> Vec<ToolOutputOverride> {
        match CompactionEvent::from_parts(&event.bundle, &event.kind, &event.payload) {
            Some(CompactionEvent::ContextEvicted(evicted)) => evicted
                .parts
                .into_iter()
                .map(|EvictedPart { part, notice }| ToolOutputOverride {
                    part,
                    output: notice,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}
