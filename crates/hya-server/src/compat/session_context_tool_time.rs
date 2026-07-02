use std::collections::BTreeMap;

use hya_proto::{Envelope, Event, PartId, ToolPartState};
use serde_json::{Value, json};

#[derive(Clone, Copy, Default)]
pub(in crate::compat) struct ToolTime {
    created: Option<u64>,
    ran: Option<u64>,
    completed: Option<u64>,
}

pub(in crate::compat) fn tool_times(envs: &[Envelope]) -> BTreeMap<PartId, ToolTime> {
    let mut out = BTreeMap::new();
    for env in envs {
        let time = millis(env.ts_millis);
        match &env.event {
            Event::ToolInputStart { part, .. } => {
                out.entry(*part).or_insert(ToolTime {
                    created: Some(time),
                    ran: None,
                    completed: None,
                });
            }
            Event::ToolCallRequested { part, .. } => {
                let entry = out.entry(*part).or_default();
                entry.created.get_or_insert(time);
                entry.ran = Some(time);
            }
            Event::ToolResult { part, .. } | Event::ToolError { part, .. } => {
                let entry = out.entry(*part).or_default();
                entry.created.get_or_insert(time);
                entry.completed = Some(time);
            }
            Event::ToolPartUpdated { part, state, .. } => {
                update_from_part_state(out.entry(*part).or_default(), state, time);
            }
            _ => {}
        }
    }
    out
}

pub(in crate::compat) fn tool_time(time: Option<ToolTime>) -> Value {
    let time = time.unwrap_or_default();
    let mut value = json!({ "created": time.created.unwrap_or(0) });
    if let Some(ran) = time.ran {
        value["ran"] = json!(ran);
    }
    if let Some(completed) = time.completed {
        value["completed"] = json!(completed);
    }
    value
}

fn update_from_part_state(entry: &mut ToolTime, state: &ToolPartState, time: u64) {
    entry.created.get_or_insert(time);
    match state {
        ToolPartState::Pending { .. } => {}
        ToolPartState::Running { .. } => {
            entry.ran.get_or_insert(time);
        }
        ToolPartState::Completed { .. } | ToolPartState::Error { .. } => {
            entry.completed = Some(time);
        }
    }
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
