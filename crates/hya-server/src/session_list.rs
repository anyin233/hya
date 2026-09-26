//! Live root-session list notices: busy/idle transitions and deletions.
//!
//! The session list's durable changes (created, titled, agent/model switch,
//! permission mode, archived) are engine events on the bus. Two list changes
//! are not: a session's busy state (`ServerState::is_busy`, process-local)
//! and its deletion (which removes the log). This hub carries both as live
//! notices of **root** sessions to every global stream, shared by every
//! router and gRPC binding built from one `AppState`.
//!
//! One tracker task per hub (the first `router` built claims it) turns busy
//! signals into deduplicated transitions: it re-checks `is_busy` for a
//! session whenever its server run starts or ends, an engine turn opens or
//! closes a message, or a Workflow run starts or finishes, and sweeps the
//! sessions it believes busy once a second, so an idle transition whose
//! signal raced the release (an engine-internal turn, a Workflow run) still
//! arrives. Each turn so yields one `busy: true` and one `busy: false`
//! notice — never per-token traffic.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hya_proto::{Event, SessionId};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::ServerState;

/// A live change of the root-session list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionListNotice {
    /// The root session went busy (`true`) or idle (`false`).
    Busy { session: SessionId, busy: bool },
    /// The root session was deleted.
    Deleted { session: SessionId },
}

/// Broadcast hub of [`SessionListNotice`]s.
#[derive(Clone)]
pub(crate) struct SessionListHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    notices: broadcast::Sender<SessionListNotice>,
    tracker: AtomicBool,
}

impl Default for SessionListHub {
    fn default() -> Self {
        let (notices, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(HubInner {
                notices,
                tracker: AtomicBool::new(false),
            }),
        }
    }
}

impl SessionListHub {
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SessionListNotice> {
        self.inner.notices.subscribe()
    }

    pub(crate) fn publish(&self, notice: SessionListNotice) {
        let _ = self.inner.notices.send(notice);
    }

    /// `true` exactly once per hub: the caller runs the busy tracker.
    fn claim_tracker(&self) -> bool {
        !self.inner.tracker.swap(true, Ordering::AcqRel)
    }
}

/// Whether `session` is a root session (no parent). A missing session is
/// not: its notices would name a session no listing has.
pub(crate) async fn is_root(state: &ServerState, session: SessionId) -> bool {
    if !state.engine.session_exists(session).await.unwrap_or(false) {
        return false;
    }
    state
        .engine
        .read_projection_shared(session)
        .await
        .is_ok_and(|projection| projection.session.parent.is_none())
}

/// Start the busy tracker for `state`'s hub unless one already runs.
pub(crate) fn spawn_busy_tracker(state: ServerState) {
    if !state.session_list.claim_tracker() {
        return;
    }
    let mut runs = state.runs.subscribe_changes();
    let mut bus = state.engine.bus().subscribe();
    tokio::spawn(async move {
        let mut busy: HashSet<SessionId> = HashSet::new();
        let mut sweep = tokio::time::interval(Duration::from_secs(1));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let signal = tokio::select! {
                changed = runs.recv() => match changed {
                    Ok(session) => Some(session),
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                },
                envelope = bus.recv() => match envelope {
                    Ok(envelope) => match turn_signal(&envelope.event) {
                        Some(session) => Some(session),
                        None => continue,
                    },
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                },
                _ = sweep.tick() => None,
            };
            match signal {
                Some(session) => recheck(&state, &mut busy, session).await,
                None => {
                    let known: Vec<SessionId> = busy.iter().copied().collect();
                    for session in known {
                        recheck(&state, &mut busy, session).await;
                    }
                }
            }
        }
    });
}

/// Engine events that can coincide with a busy transition.
fn turn_signal(event: &Event) -> Option<SessionId> {
    match event {
        Event::MessageStarted { session, .. }
        | Event::MessageFinished { session, .. }
        | Event::WorkflowRunStarted { session, .. }
        | Event::WorkflowRunFinished { session, .. } => Some(*session),
        _ => None,
    }
}

/// Publish `session`'s busy state when it differs from the last one seen.
async fn recheck(state: &ServerState, busy: &mut HashSet<SessionId>, session: SessionId) {
    let now = state.is_busy(session);
    let changed = if now {
        busy.insert(session)
    } else {
        busy.remove(&session)
    };
    if changed && is_root(state, session).await {
        state
            .session_list
            .publish(SessionListNotice::Busy { session, busy: now });
    }
}
