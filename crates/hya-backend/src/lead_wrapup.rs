//! The lead of a headless root run (`exec`/`run`) must not end the run while
//! subagents it started are still live: `cmd_exec` stops every subagent when
//! the run ends, so their work would be lost. When the lead's turn ends on its
//! own with live subagents, the run gives it another turn with a harness
//! notice naming them, until a turn ends with none left.

use hya_core::{AgentSpec, CoreError, SessionEngine};
use hya_proto::{
    FinishReason, MessageId, PartProjection, Projection, RosterEntry, RosterStatus, SessionId,
    scope,
};
use tokio_util::sync::CancellationToken;

/// Consecutive notice turns that may end with live subagents and no `wait` or
/// `archive` call before the run ends anyway: a model can ignore the notice.
const MAX_IGNORED_NOTICES: u32 = 2;

/// Run the lead's turn, then one more turn per [`notice`] while it ends with
/// live subagents (bounded by [`MAX_IGNORED_NOTICES`]). Returns the last
/// turn's finish; a cancelled or failed turn ends the run at once.
pub(crate) async fn run_lead(
    engine: &SessionEngine,
    session: SessionId,
    agent: &AgentSpec,
    cancel: CancellationToken,
) -> Result<FinishReason, CoreError> {
    let mut ignored = 0;
    let mut last_notice: Option<MessageId> = None;
    loop {
        let finish = engine.run_turn(session, agent, cancel.clone()).await?;
        if !matches!(finish, FinishReason::Stop | FinishReason::Length) {
            return Ok(finish);
        }
        let projection = engine.read_projection(session).await?;
        let live = live_subagents(&projection);
        if live.is_empty() {
            return Ok(finish);
        }
        if let Some(notice) = last_notice {
            if called_wait_or_archive(&projection, notice) {
                ignored = 0;
            } else {
                ignored += 1;
            }
            if ignored >= MAX_IGNORED_NOTICES {
                return Ok(finish);
            }
        }
        last_notice = Some(engine.inject_system_message(session, notice(&live)).await?);
    }
}

/// The lead's direct subagents still on the team roster.
fn live_subagents(projection: &Projection) -> Vec<&RosterEntry> {
    projection
        .team
        .roster
        .values()
        .filter(|entry| scope::parent_path(&entry.handle) == Some(scope::ROOT_HANDLE))
        .collect()
}

/// Whether any message after the `since` notice calls `wait` or `archive`.
fn called_wait_or_archive(projection: &Projection, since: MessageId) -> bool {
    let messages = &projection.session.messages;
    messages
        .iter()
        .position(|message| message.id == since)
        .is_some_and(|at| {
            messages[at + 1..].iter().any(|message| {
                message.parts.iter().any(|part| {
                    matches!(part, PartProjection::Tool { name, .. }
                        if matches!(name.as_str(), "wait" | "archive"))
                })
            })
        })
}

fn notice(live: &[&RosterEntry]) -> String {
    let listed = live
        .iter()
        .map(|entry| {
            let status = match entry.status {
                RosterStatus::Idle => "idle",
                RosterStatus::Busy => "working",
                RosterStatus::Done => "done",
                RosterStatus::Failed => "failed",
            };
            match &entry.current_task {
                Some(task) => format!("- {} ({status}: {task})", entry.handle),
                None => format!("- {} ({status})", entry.handle),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "HEADLESS RUN — your turn ended while subagents you started are still live:\n{listed}\n\
         This run ends when your turn ends; every subagent still live is then stopped and its \
         work is lost. Before your final answer, call `wait` to collect their reports and \
         integrate the results, or `archive` the ones you no longer need."
    )
}
