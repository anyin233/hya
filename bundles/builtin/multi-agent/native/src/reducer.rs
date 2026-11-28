//! Durable `Event::Extension` projection reducer for `hya/multi-agent`.

use hya_proto::{AgentName, MemberId, MemberRunStatus, Projection, RosterStatus, scope};
use hya_runtime_abi::{ExtensionEventContext, HostError, ProjectionReducer};
use hya_tool::ReportOutcome;
use hya_tool::extension::{
    ArchivedEntry, BUNDLE_MULTI_AGENT, ExtensionEvent, HandoffProjection, MemberProjection,
    MultiAgentExtensionState, ResidentWorkProjection, RosterEntry,
};
use serde_json::Value;

/// Decode folded `hya/multi-agent` extension state from a [`Projection`].
#[must_use]
pub fn decode_multi_agent_projection(projection: &Projection) -> MultiAgentExtensionState {
    projection
        .extensions
        .get(BUNDLE_MULTI_AGENT)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn member_mut(members: &mut Vec<MemberProjection>, member: MemberId) -> &mut MemberProjection {
    if let Some(idx) = members.iter().position(|m| m.member == member) {
        return &mut members[idx];
    }
    members.push(MemberProjection {
        member,
        child: None,
        subagent_type: AgentName::new(""),
        description: String::new(),
        depth: 0,
        status: MemberRunStatus::Spawning,
        summary: String::new(),
        directive: String::new(),
        tool_call: None,
    });
    let last = members.len() - 1;
    &mut members[last]
}

/// ABI v2 [`ProjectionReducer`] folding `hya/multi-agent` extension events into
/// `projection.extensions["hya/multi-agent"]`.
#[derive(Debug, Default)]
pub struct MultiAgentProjectionReducer;

impl ProjectionReducer for MultiAgentProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_MULTI_AGENT
    }

    fn initial_state(&self) -> Value {
        serde_json::to_value(MultiAgentExtensionState::default()).unwrap_or(Value::Null)
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        let Some(ext) = ExtensionEvent::from_parts(
            event.session,
            BUNDLE_MULTI_AGENT,
            &event.kind,
            &event.payload,
        ) else {
            return Ok(());
        };
        let mut multi: MultiAgentExtensionState = if state.is_null() {
            MultiAgentExtensionState::default()
        } else {
            serde_json::from_value(state.clone()).unwrap_or_default()
        };

        match ext {
            ExtensionEvent::SessionStatus { .. } => return Ok(()),
            ExtensionEvent::MemberSpawned {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
                ..
            } => {
                let entry = member_mut(&mut multi.members, member);
                entry.child = child;
                entry.subagent_type = subagent_type;
                entry.description = description;
                entry.depth = depth;
                entry.status = MemberRunStatus::Spawning;
                if !directive.is_empty() {
                    entry.directive = directive;
                }
                if tool_call.is_some() {
                    entry.tool_call = tool_call;
                }
            }
            ExtensionEvent::MemberStatusChanged { member, status, .. } => {
                member_mut(&mut multi.members, member).status = status;
            }
            ExtensionEvent::MemberFinished {
                member,
                status,
                summary,
                child,
                ..
            } => {
                let entry = member_mut(&mut multi.members, member);
                entry.status = status;
                entry.summary = summary;
                if child.is_some() {
                    entry.child = child;
                }
            }
            ExtensionEvent::AgentRegistered {
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
                ..
            } => {
                let path = match &parent {
                    Some(parent) => scope::join_path(parent, &handle),
                    None => handle,
                };
                let entry = multi
                    .roster
                    .entry(path.clone())
                    .or_insert_with(|| RosterEntry {
                        handle: path,
                        session: agent_session,
                        agent_type: agent_type.clone(),
                        mode,
                        status: RosterStatus::default(),
                        current_task: None,
                        resident_cursor: 0,
                        heartbeat_ms: 0,
                        resident_work: None,
                    });
                entry.session = agent_session;
                entry.agent_type = agent_type;
                entry.mode = mode;
            }
            ExtensionEvent::AgentActivityChanged {
                handle,
                status,
                current_task,
                ..
            } => {
                if let Some(entry) = multi.roster.get_mut(&handle) {
                    entry.status = status;
                    if current_task.is_some() {
                        entry.current_task = current_task;
                    }
                }
            }
            ExtensionEvent::ResidentWorkStarted {
                actor_session,
                handle,
                epoch,
                inbox_through,
                ..
            } => {
                if let Some(entry) = multi.roster.get_mut(&handle)
                    && entry.session == actor_session
                    && entry.mode.is_resident()
                {
                    entry.status = RosterStatus::Busy;
                    entry.resident_work = Some(ResidentWorkProjection {
                        epoch,
                        inbox_through,
                    });
                }
            }
            ExtensionEvent::ResidentWorkFinished {
                handle,
                epoch,
                inbox_through,
                ..
            } => {
                if let Some(entry) = multi.roster.get_mut(&handle)
                    && entry.resident_work.is_some_and(|work| work.epoch == epoch)
                {
                    entry.resident_work = None;
                    entry.resident_cursor = entry.resident_cursor.max(inbox_through);
                }
            }
            ExtensionEvent::AgentHeartbeat {
                handle,
                heartbeat_ms,
                ..
            } => {
                if let Some(entry) = multi.roster.get_mut(&handle) {
                    entry.heartbeat_ms = entry.heartbeat_ms.max(heartbeat_ms);
                }
            }
            ExtensionEvent::SubagentReported {
                member,
                outcome,
                report,
                ..
            } => {
                let entry = member_mut(&mut multi.members, member);
                entry.status = match outcome {
                    ReportOutcome::Done => MemberRunStatus::Done,
                    ReportOutcome::Failed => MemberRunStatus::Failed,
                };
                entry.summary = report;
            }
            ExtensionEvent::HandoffCommitted {
                generation,
                doc,
                degraded,
                ..
            } => {
                multi.handoff = Some(HandoffProjection {
                    generation,
                    doc,
                    degraded,
                });
            }
            ExtensionEvent::AgentArchived {
                handle,
                child,
                reason,
                ..
            } => {
                if let Some(entry) = multi.roster.remove(&handle) {
                    multi.archived.insert(
                        handle.clone(),
                        ArchivedEntry {
                            handle,
                            session: child,
                            agent_type: entry.agent_type,
                            reason,
                        },
                    );
                }
            }
            ExtensionEvent::AgentRestarted { handle, .. } => {
                multi.archived.remove(&handle);
            }
            _ => return Ok(()),
        }

        *state = serde_json::to_value(&multi).map_err(|error| {
            HostError::Internal(format!("serialize multi-agent state: {error}"))
        })?;
        Ok(())
    }
}
