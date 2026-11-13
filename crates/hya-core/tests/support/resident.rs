#![allow(dead_code, clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use hya_core::{
    AgentResourcePolicy, AgentSpec, BoundSidecarFactory, ResidentSupervisor, SessionEngine,
    TaskSpawnOrigin, TurnBinding,
};
use hya_proto::{Role, RosterStatus, SessionId, ToolCallId};
use hya_tool::AgentDef;
use tokio_util::sync::CancellationToken;

/// Resolved inputs for resident integration tests; no alternate turn executor.
pub struct ResidentTurn {
    pub agent: AgentSpec,
    pub binding: TurnBinding,
    pub agents: Arc<[AgentDef]>,
    pub resources: Option<AgentResourcePolicy>,
    pub guidance: Option<Arc<str>>,
    pub directive: String,
    pub tool_call: Option<ToolCallId>,
    pub description: String,
    pub sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
}

/// Register ordinary residents, await their durable settlement, then archive.
/// Tests needing multiple wakes or reports retain their own supervisor instead.
pub async fn run_residents(
    engine: Arc<SessionEngine>,
    parent: SessionId,
    turns: Vec<ResidentTurn>,
    cancel: CancellationToken,
) -> Vec<SessionId> {
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _activity = supervisor
        .root_driver_activity(parent)
        .await
        .expect("root driver");
    let (root, _) = engine.session_lineage(parent).await.expect("lineage");
    let mut members = Vec::new();
    for turn in turns {
        let resources = turn.resources.unwrap_or_else(|| {
            turn.binding
                .agent_resource_policy(turn.agent.name.as_str())
                .expect("resources")
        });
        let task = TaskSpawnOrigin {
            subagent_type: turn.agent.name.to_string(),
            description: turn.description,
            tool_call: turn.tool_call,
        };
        members.push(
            supervisor
                .spawn_resident_typed(
                    parent,
                    turn.agent,
                    (turn.binding, turn.agents, resources, turn.sidecar_factory),
                    turn.directive,
                    task,
                    None,
                    turn.guidance,
                )
                .await
                .expect("register resident"),
        );
    }
    for (child, handle) in &members {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let projection = engine.read_projection(root).await.expect("root projection");
                if projection.team.archived.contains_key(handle) {
                    break;
                }
                let row = projection.team.roster.get(handle).expect("resident row");
                let child_projection = engine
                    .read_projection(*child)
                    .await
                    .expect("child projection");
                if row.status == RosterStatus::Idle
                    && !engine.turn_active(*child)
                    && child_projection
                        .session
                        .messages
                        .iter()
                        .any(|m| m.role == Role::Assistant && m.finish.is_some())
                {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        })
        .await
        .expect("resident settlement");
        supervisor
            .end_member(root, handle)
            .await
            .expect("archive resident");
    }
    members.into_iter().map(|(session, _)| session).collect()
}
