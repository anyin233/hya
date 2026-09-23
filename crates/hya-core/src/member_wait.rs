//! The `wait` lifecycle request (0.41.0): block the caller until its
//! subagents finish their current work — report, go idle, or are archived —
//! or, for the channel-tools `wait`, until mail for the caller arrives
//! (harness mail such as `LEADER FAILED` included), bounded by a timeout.
//!
//! The waiter usually runs INSIDE the lead's own turn (it is the model's tool
//! call), holding that session's turn lease. It therefore never relies on a
//! resident wake of the lead (which would queue behind the very turn that is
//! waiting): it subscribes to the engine bus and re-evaluates its targets on
//! every team-lifecycle event — `AgentActivityChanged`, `SubagentReported`,
//! `AgentArchived`, `MailSent`, … on the team root or the caller's log —
//! against the resident supervisor's in-memory slot state (busy, owed work,
//! a report accepted but not yet executed) plus the folded projection. A slow
//! periodic re-check backs up a lagged bus. Dropping the request (the tool
//! call was cancelled) drops this future; the lifecycle service watches its
//! reply channel for that.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hya_proto::{
    Event, MailEndpoint, MemberRunStatus, PartProjection, Projection, Role, RosterStatus,
    SessionId, scope,
};
use hya_tool::{WaitMail, WaitMember, WaitMemberState, WaitMode, WaitOutcome, WaitSpec, WaitWake};

use crate::engine::SessionEngine;
use crate::error::CoreError;
use crate::resident::{ResidentSupervisor, resolve_member_target};

/// Backstop re-evaluation interval when no bus event arrives.
const RECHECK: Duration = Duration::from_secs(5);
/// Bound on report/answer/mail previews in the outcome.
const PREVIEW_CHARS: usize = 600;

/// Run one `wait` for `caller`. See the module docs.
///
/// # Errors
/// [`CoreError::Invalid`] when a target is unknown (the message lists the
/// caller's live subagents), the team lead, or not one of the caller's
/// subagents; store failures propagate.
pub(crate) async fn wait_for_members(
    engine: &Arc<SessionEngine>,
    supervisor: &ResidentSupervisor,
    caller: SessionId,
    spec: WaitSpec,
) -> Result<WaitOutcome, CoreError> {
    let started = Instant::now();
    // Subscribe before the first snapshot so no transition is lost between.
    let mut bus = engine.bus().subscribe();
    let (root, _) = engine.session_lineage(caller).await?;
    let caller_path = engine.resolve_handle(root, caller).await?;
    let projection = engine.read_projection_shared(root).await?;
    let targets = wait_targets(&projection, root, &caller_path, &spec.targets)?;
    // Mail at or after this inbox index is new to the caller: in-turn steering
    // advances the durable cursor as it surfaces mail, and a resident wake
    // records what it injected.
    let seen = projection.team.roster.get(&caller_path).map_or(0, |entry| {
        entry.resident_work.map_or(entry.resident_cursor, |work| {
            entry.resident_cursor.max(work.inbox_through)
        })
    });
    let seen = usize::try_from(seen).unwrap_or(usize::MAX);
    // The lead has no parent: without subagents there is nobody to hear from.
    let mail_only = targets.is_empty() && spec.wake_on_mail && caller != root;
    let deadline = tokio::time::Instant::now() + spec.timeout;
    loop {
        // Shared cached fold: each wake folds only the root's new events.
        let projection = engine.read_projection_shared(root).await?;
        let (finished, running) = evaluate(engine, supervisor, root, &projection, &targets).await;
        let mail = if spec.wake_on_mail {
            new_mail(&projection, &caller_path, seen)
        } else {
            Vec::new()
        };
        // A report is mailed a moment before its archive commits: mail that
        // is only the report of a target still archiving is a member finish,
        // not a separate wake — hold on for its `AgentArchived`.
        let only_pending_reports = !mail.is_empty()
            && mail.iter().all(|message| {
                running.iter().any(|member| {
                    member.handle == message.from
                        && supervisor.member_archiving(root, member.session)
                })
            });
        let members_done = !targets.is_empty()
            && match spec.mode {
                WaitMode::Any => !finished.is_empty(),
                WaitMode::All => running.is_empty(),
            };
        let woke_by = if members_done {
            Some(WaitWake::Members)
        } else if !mail.is_empty() && !only_pending_reports {
            Some(WaitWake::Mail)
        } else if targets.is_empty() && !mail_only {
            Some(WaitWake::NothingToWaitFor)
        } else if tokio::time::Instant::now() >= deadline {
            Some(WaitWake::Timeout)
        } else {
            None
        };
        if let Some(woke_by) = woke_by {
            return Ok(WaitOutcome {
                woke_by,
                finished,
                running,
                mail,
                waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        let recheck = (tokio::time::Instant::now() + RECHECK).min(deadline);
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(recheck) => break,
                received = bus.recv() => match received {
                    Ok(envelope) if relevant(&envelope.event, root, caller) => break,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(CoreError::Invalid(
                            "the engine bus closed during wait".to_string(),
                        ));
                    }
                },
            }
        }
    }
}

/// Resolve the requested targets, or every live direct subagent.
fn wait_targets(
    projection: &Projection,
    root: SessionId,
    caller_path: &str,
    requested: &[String],
) -> Result<Vec<(String, SessionId)>, CoreError> {
    if requested.is_empty() {
        return Ok(projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| scope::parent_path(path) == Some(caller_path))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect());
    }
    let prefix = format!("{caller_path}{}", scope::PATH_SEPARATOR);
    let mut targets: Vec<(String, SessionId)> = Vec::new();
    for raw in requested {
        let target = resolve_member_target(projection, root, caller_path, raw)?;
        if target.session == root || target.handle == scope::ROOT_HANDLE {
            return Err(CoreError::Invalid(
                "`main` is the team lead; wait on your subagents, not on the lead".to_string(),
            ));
        }
        if !target.handle.starts_with(&prefix) {
            return Err(CoreError::Invalid(format!(
                "`{}` is not one of your subagents; you can only wait on agents you spawned (or their subagents)",
                target.handle
            )));
        }
        if !targets.iter().any(|(handle, _)| *handle == target.handle) {
            targets.push((target.handle, target.session));
        }
    }
    Ok(targets)
}

/// Split targets into finished (reported / archived / idle) and running.
async fn evaluate(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    root: SessionId,
    projection: &Projection,
    targets: &[(String, SessionId)],
) -> (Vec<WaitMember>, Vec<WaitMember>) {
    let mut finished = Vec::new();
    let mut running = Vec::new();
    for (handle, session) in targets {
        let live = projection
            .team
            .roster
            .get(handle)
            .filter(|entry| entry.session == *session);
        if let Some(entry) = live {
            let busy = supervisor
                .member_busy(root, *session)
                .unwrap_or(entry.status == RosterStatus::Busy);
            if busy {
                running.push(WaitMember {
                    handle: handle.clone(),
                    session: *session,
                    state: WaitMemberState::Working,
                    outcome: None,
                    report: None,
                });
            } else {
                finished.push(WaitMember {
                    handle: handle.clone(),
                    session: *session,
                    state: WaitMemberState::Idle,
                    outcome: None,
                    report: last_answer(engine, *session).await,
                });
            }
            continue;
        }
        let (state, outcome, report) = match terminal_row(engine, *session).await {
            Some((MemberRunStatus::Done, summary)) => (
                WaitMemberState::Reported,
                Some("done".to_string()),
                Some(summary),
            ),
            Some((MemberRunStatus::Failed, summary)) => (
                WaitMemberState::Reported,
                Some("failed".to_string()),
                Some(summary),
            ),
            Some((MemberRunStatus::Cancelled, summary)) => (
                WaitMemberState::Archived,
                Some("cancelled".to_string()),
                Some(summary),
            ),
            _ => (WaitMemberState::Archived, None, None),
        };
        finished.push(WaitMember {
            handle: handle.clone(),
            session: *session,
            state,
            outcome,
            report: report
                .filter(|text| !text.trim().is_empty())
                .map(|text| preview(&text)),
        });
    }
    (finished, running)
}

/// The member row for `child` on its parent's log, when terminal.
async fn terminal_row(
    engine: &SessionEngine,
    child: SessionId,
) -> Option<(MemberRunStatus, String)> {
    let parent = engine
        .read_projection_shared(child)
        .await
        .ok()?
        .session
        .parent?;
    let parent_projection = engine.read_projection_shared(parent).await.ok()?;
    parent_projection
        .session
        .members
        .iter()
        .rev()
        .find(|row| row.child == Some(child))
        .map(|row| (row.status, row.summary.clone()))
}

/// The tail of the member's last assistant answer, bounded.
async fn last_answer(engine: &SessionEngine, session: SessionId) -> Option<String> {
    let projection = engine.read_projection_shared(session).await.ok()?;
    let message = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)?;
    let text: String = message
        .parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| preview(text))
}

/// Mail in the caller's inbox from index `seen` on, excluding its own posts.
fn new_mail(projection: &Projection, caller_path: &str, seen: usize) -> Vec<WaitMail> {
    projection
        .team
        .inboxes
        .get(caller_path)
        .map(|inbox| {
            inbox
                .iter()
                .skip(seen)
                .filter(|message| message.from != caller_path)
                .map(|message| WaitMail {
                    from: message.from.clone(),
                    channel: match &message.to {
                        MailEndpoint::Channel(channel) => Some(channel.clone()),
                        MailEndpoint::Handle(_) => None,
                    },
                    preview: preview(&message.body),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Team-lifecycle events on the team root or the caller's own log.
fn relevant(event: &Event, root: SessionId, caller: SessionId) -> bool {
    let lifecycle = matches!(
        event,
        Event::MailSent { .. }
            | Event::MailConsumed { .. }
            | Event::AgentActivityChanged { .. }
            | Event::AgentArchived { .. }
            | Event::AgentRestarted { .. }
            | Event::AgentRegistered { .. }
            | Event::ResidentWorkStarted { .. }
            | Event::SubagentReported { .. }
            | Event::MemberFinished { .. }
    );
    lifecycle
        && event
            .session()
            .is_some_and(|session| session == root || session == caller)
}

fn preview(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= PREVIEW_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(PREVIEW_CHARS).collect();
    format!("{cut}…")
}
