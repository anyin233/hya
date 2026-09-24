//! The `wait` lifecycle request (0.41.0): block the caller until its
//! subagents finish — a subagent finishes only when it **reports** or is
//! **archived** (terminated), never merely by going idle between turns — or,
//! for the channel-tools `wait`, until new mail for the caller arrives
//! (harness mail such as `LEADER FAILED` included), bounded by a timeout.
//!
//! Every call is measured against a baseline taken when it starts, so a
//! repeated call never returns the same news twice:
//!
//! - **Finishes** count only when they happen during the call: a target that
//!   had already reported or been archived at the start is listed as
//!   `already_finished` and never wakes the wait. A target woken again (mail
//!   to an archived member revives it) is working until its NEXT report; the
//!   member's terminal handoff generation, bumped by every archive, tells a
//!   fresh finish from the one before the call.
//! - **Mail** is new only past the caller's durable inbox cursor (the same
//!   `MailConsumed` cursor in-turn steering and resident wakes use). The mail a
//!   wait returns — and the report mail of targets it reports finished — is
//!   committed as consumed, so neither the next wait nor the `[NEW MAIL]`
//!   steer notice nor a later resident wake delivers it again.
//! - **Stalls**: a target whose turn ended without a report while nothing is
//!   queued for it (no running turn, owed mail or directive, accepted report,
//!   or live subagent of its own) will not continue by itself. That is not a
//!   finish; the wait says so once per stopped turn (`woke_by: stalled`, the
//!   member `idle` under `running`) and a repeated wait blocks.
//! - Only `timeout_secs: 0` returns the current state without blocking;
//!   `nothing_to_wait_for` returns at once when there is no target left to
//!   wait on (no live subagent, or every target already finished).
//!
//! The waiter usually runs INSIDE the lead's own turn (it is the model's tool
//! call), holding that session's turn lease. It therefore never relies on a
//! resident wake of the lead (which would queue behind the very turn that is
//! waiting): it subscribes to the engine bus and re-evaluates its targets on
//! every team-lifecycle event — `AgentActivityChanged`, `SubagentReported`,
//! `AgentArchived`, `MailSent`, … on the team root or the caller's log —
//! against the resident supervisor's in-memory slot state (busy, owed work,
//! a report accepted but not yet executed) plus the cached projections. A
//! slow periodic re-check backs up a lagged bus. Dropping the request (the
//! tool call was cancelled) drops this future; the lifecycle service watches
//! its reply channel for that.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hya_proto::{
    ArchiveReason, Event, MailEndpoint, MemberRunStatus, MessageId, Projection, Role, RosterStatus,
    SessionId, scope,
};
use hya_tool::{WaitMail, WaitMember, WaitMemberState, WaitMode, WaitOutcome, WaitSpec, WaitWake};

use crate::engine::SessionEngine;
use crate::error::CoreError;
use crate::resident::{ResidentSupervisor, resolve_member_target};

/// Backstop re-evaluation interval when no bus event arrives.
const RECHECK: Duration = Duration::from_secs(5);
/// Bound on report/mail bodies in the outcome (the steer notice's bound).
const PREVIEW_CHARS: usize = 600;

/// One waited-on subagent and where it stood when the call began.
struct Target {
    handle: String,
    session: SessionId,
    /// Reported, archived, or terminal on the roster at the start.
    done_at_start: bool,
    /// Its terminal handoff generation at the start (every archive bumps it).
    generation: u32,
}

/// Targets sorted by standing at one evaluation.
#[derive(Default)]
struct Standing {
    /// Reported or archived during this call.
    finished: Vec<WaitMember>,
    /// Reported or archived before this call, not woken since.
    already: Vec<WaitMember>,
    /// Working, or stopped without a report (`idle`).
    running: Vec<WaitMember>,
    /// Stopped targets: (session, last assistant message).
    stalled: Vec<(SessionId, Option<MessageId>)>,
    /// Full report bodies of `finished` targets: (handle, body), to recognize
    /// their report mail.
    reports: Vec<(String, String)>,
}

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
    let resolved = wait_targets(&projection, root, &caller_path, &spec.targets)?;
    let mut targets = Vec::with_capacity(resolved.len());
    for (handle, session) in resolved {
        let done_at_start = projection
            .team
            .roster
            .get(&handle)
            .filter(|entry| entry.session == session)
            .is_none_or(|entry| terminal_status(entry.status));
        targets.push(Target {
            handle,
            session,
            done_at_start,
            generation: handoff_generation(engine, session).await,
        });
    }
    // Mail past this inbox index is new to the caller: in-turn steering and
    // earlier waits advance the durable cursor as they deliver mail, and a
    // resident wake records what it injected.
    let cursor = projection.team.roster.get(&caller_path).map_or(0, |entry| {
        entry.resident_work.map_or(entry.resident_cursor, |work| {
            entry.resident_cursor.max(work.inbox_through)
        })
    });
    let cursor = usize::try_from(cursor).unwrap_or(usize::MAX);
    // The lead has no parent: without subagents there is nobody to hear from.
    let mail_only = targets.is_empty() && spec.wake_on_mail && caller != root;
    let deadline = tokio::time::Instant::now() + spec.timeout;
    loop {
        // Shared cached fold: each wake folds only the root's new events.
        let projection = engine.read_projection_shared(root).await?;
        let standing = evaluate(engine, supervisor, root, &projection, &targets).await;
        let inbox = scan_inbox(
            supervisor,
            root,
            &projection,
            &caller_path,
            cursor,
            &standing,
            spec.wake_on_mail,
        );
        let members_done = match spec.mode {
            WaitMode::Any => !standing.finished.is_empty(),
            WaitMode::All => !standing.finished.is_empty() && standing.running.is_empty(),
        };
        let new_stall = standing
            .stalled
            .iter()
            .any(|(member, last)| !supervisor.wait_stall_reported(caller, *member, *last));
        let nothing_left = standing.finished.is_empty() && standing.running.is_empty();
        let woke_by = if members_done {
            Some(WaitWake::Members)
        } else if !inbox.mail.is_empty() {
            Some(WaitWake::Mail)
        } else if new_stall {
            Some(WaitWake::Stalled)
        } else if nothing_left && !mail_only {
            // No live subagent, or every target already finished.
            Some(WaitWake::NothingToWaitFor)
        } else if tokio::time::Instant::now() >= deadline {
            Some(WaitWake::Timeout)
        } else {
            None
        };
        if let Some(woke_by) = woke_by {
            if inbox.through > cursor {
                engine
                    .emit_for_actor(
                        None,
                        root,
                        Event::MailConsumed {
                            session: root,
                            handle: caller_path.clone(),
                            through: u64::try_from(inbox.through).unwrap_or(u64::MAX),
                        },
                    )
                    .await?;
            }
            for (member, last) in &standing.stalled {
                supervisor.record_wait_stall(caller, *member, *last);
            }
            return Ok(WaitOutcome {
                woke_by,
                finished: standing.finished,
                already_finished: standing.already,
                running: standing.running,
                mail: inbox.mail,
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
        if !targets
            .iter()
            .any(|(_, session)| *session == target.session)
        {
            targets.push((target.handle, target.session));
        }
    }
    Ok(targets)
}

/// A roster status that ends the member for good (a transient member's end,
/// or a resident stopping on its way to the archive).
fn terminal_status(status: RosterStatus) -> bool {
    matches!(status, RosterStatus::Done | RosterStatus::Failed)
}

/// The member's latest terminal handoff generation (0: never archived).
async fn handoff_generation(engine: &SessionEngine, session: SessionId) -> u32 {
    engine
        .read_projection_shared(session)
        .await
        .ok()
        .and_then(|projection| {
            projection
                .session
                .handoff
                .as_ref()
                .map(|handoff| handoff.generation)
        })
        .unwrap_or(0)
}

/// Sort the targets by standing against the call's baseline.
async fn evaluate(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    root: SessionId,
    projection: &Projection,
    targets: &[Target],
) -> Standing {
    let mut standing = Standing::default();
    for target in targets {
        let live = projection
            .team
            .roster
            .get(&target.handle)
            .filter(|entry| entry.session == target.session);
        if let Some(entry) = live.filter(|entry| !terminal_status(entry.status)) {
            // Only a resident parks between turns; a transient (one-shot
            // Workflow stage) member runs until its terminal status.
            let busy = !entry.mode.is_resident()
                || supervisor
                    .member_busy(root, target.session)
                    .unwrap_or(entry.status == RosterStatus::Busy);
            // A member idle while its own subagents run is waiting on them.
            let leads_live = projection
                .team
                .roster
                .keys()
                .any(|path| scope::parent_path(path) == Some(target.handle.as_str()));
            let state = if busy || leads_live {
                WaitMemberState::Working
            } else {
                standing
                    .stalled
                    .push((target.session, last_answer(engine, target.session).await));
                WaitMemberState::Idle
            };
            standing.running.push(WaitMember {
                handle: target.handle.clone(),
                session: target.session,
                state,
                outcome: None,
                report: None,
            });
            continue;
        }
        // Archived, or terminal on the roster.
        let reason = if live.is_some() {
            None
        } else {
            projection
                .team
                .archived
                .get(&target.handle)
                .filter(|entry| entry.session == target.session)
                .map(|entry| entry.reason)
        };
        let row = terminal_row(engine, target.session).await;
        let (state, outcome, full) = match (reason, row) {
            (None | Some(ArchiveReason::Reported), Some((MemberRunStatus::Done, summary))) => (
                WaitMemberState::Reported,
                Some("done".to_string()),
                Some(summary),
            ),
            (None | Some(ArchiveReason::Reported), Some((MemberRunStatus::Failed, summary))) => (
                WaitMemberState::Reported,
                Some("failed".to_string()),
                Some(summary),
            ),
            (_, Some((MemberRunStatus::Cancelled, note))) => (
                WaitMemberState::Archived,
                Some("cancelled".to_string()),
                Some(note),
            ),
            // Archived without a report of this episode (a revived member's
            // row still carries its earlier report — never repeat that).
            (Some(reason), _) if reason != ArchiveReason::Reported => (
                WaitMemberState::Archived,
                Some("cancelled".to_string()),
                None,
            ),
            _ => (WaitMemberState::Archived, None, None),
        };
        let member = WaitMember {
            handle: target.handle.clone(),
            session: target.session,
            state,
            outcome,
            report: full
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(preview),
        };
        let fresh = !target.done_at_start
            || handoff_generation(engine, target.session).await > target.generation;
        if fresh {
            if state == WaitMemberState::Reported
                && let Some(body) = full
            {
                standing.reports.push((target.handle.clone(), body));
            }
            standing.finished.push(member);
        } else {
            standing.already.push(member);
        }
    }
    standing
}

/// The member row for `child` on its parent's log.
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

/// The member's last assistant message: the turn a stall notice is about.
async fn last_answer(engine: &SessionEngine, session: SessionId) -> Option<MessageId> {
    let projection = engine.read_projection_shared(session).await.ok()?;
    projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.id)
}

/// What the caller's inbox holds past its cursor at one evaluation.
struct InboxScan {
    /// New mail to return (channel-aware wait only).
    mail: Vec<WaitMail>,
    /// Inbox index this outcome delivers through (commit when past the
    /// cursor): everything for the channel-aware wait; for the member-only
    /// wait just the leading run of own posts and finished targets' reports.
    through: usize,
}

/// Split the caller's unread inbox into new mail, the report mail of
/// targets finishing now (delivered as their finish, not as mail), and report
/// mail of targets still committing their archive (held: their finish follows).
fn scan_inbox(
    supervisor: &ResidentSupervisor,
    root: SessionId,
    projection: &Projection,
    caller_path: &str,
    cursor: usize,
    standing: &Standing,
    wake_on_mail: bool,
) -> InboxScan {
    let inbox = projection
        .team
        .inboxes
        .get(caller_path)
        .map_or(&[][..], Vec::as_slice);
    let mut mail = Vec::new();
    let mut through = cursor.min(inbox.len());
    let mut contiguous = true;
    for (index, message) in inbox.iter().enumerate().skip(cursor) {
        let own = message.from == caller_path;
        let finish_report = standing
            .reports
            .iter()
            .any(|(handle, body)| *handle == message.from && *body == message.body);
        if own || finish_report {
            if contiguous {
                through = index + 1;
            }
            continue;
        }
        contiguous = false;
        if !wake_on_mail {
            continue;
        }
        through = index + 1;
        // A report is mailed a moment before its archive commits: hold on
        // for the `AgentArchived` that makes it a member finish.
        let pending_report = standing.running.iter().any(|member| {
            member.handle == message.from && supervisor.member_archiving(root, member.session)
        });
        if pending_report {
            continue;
        }
        mail.push(WaitMail {
            from: message.from.clone(),
            channel: match &message.to {
                MailEndpoint::Channel(channel) => Some(channel.clone()),
                MailEndpoint::Handle(_) => None,
            },
            preview: preview(&message.body),
        });
    }
    if wake_on_mail {
        through = through.max(inbox.len());
    }
    InboxScan { mail, through }
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
