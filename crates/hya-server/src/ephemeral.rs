//! Ephemeral sessions: the server drops a session a client created before
//! the user asked for one (`CreateSessionRequest.ephemeral`, the TUI's
//! session on connect) once it is still unused and no client watches it
//! (docs/protocol/README.md "Ephemeral sessions"; ADR-0023 amendment "The
//! daemon drops unused sessions").
//!
//! - **Unused** is the projection's `ephemeral` flag: set by
//!   `SessionEphemeralSet { ephemeral: true }` at creation, cleared for good
//!   by the first message, a title, an archive, or a fork taken from it.
//! - **Watching** is an open `StreamSessionEvents` stream (SSE or gRPC) on
//!   the session: `frame_stream` holds a [`WatchGuard`] for as long as the
//!   stream lives. [`SessionWatchers`] counts them per session and is shared
//!   by every router and gRPC binding built from one `AppState`.
//! - **Checks.** When the last watcher of a session leaves, a check is due
//!   after [`EphemeralGrace::unwatched`] (a client that reconnects or reopens
//!   the session within it keeps it); a session created ephemeral gets one
//!   after [`EphemeralGrace::unclaimed`] (it may never be watched); a server
//!   start sweeps the unused ephemeral sessions left over (a crash, a kill)
//!   with one after [`EphemeralGrace::startup`]. Each check carries the
//!   session's watch generation, so it fires only while nobody watches the
//!   session and no later leave scheduled a newer one.
//! - **Drop.** The reaper re-reads the session under a quiet run
//!   reservation (the admission slot every prompt, command, shell turn, and
//!   Workflow run takes, so none is admitted meanwhile), deletes the log only
//!   if it did not grow since that read (`SessionStore::delete_session_at`),
//!   and publishes the live `sessionDeleted` notice of a root session (and
//!   `projectsUpdated` for a Project's session). Like `DeleteSession`, it
//!   leaves a temporary session's scratch directory on disk (ADR-0024).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hya_proto::{EventSeq, SessionId};
use tokio::sync::mpsc;

use crate::ServerState;
use crate::session_list::SessionListNotice;

/// Largest event log the startup sweep reads: an unused ephemeral session
/// holds its creation and a few setting changes, never a transcript, so
/// larger logs are skipped without folding them.
const SWEEP_MAX_EVENTS: u64 = 256;

/// How long the server waits before it checks an unused ephemeral session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EphemeralGrace {
    /// After the last `StreamSessionEvents` stream on it closes.
    pub unwatched: Duration,
    /// After it was created, when no client ever watched it.
    pub unclaimed: Duration,
    /// After the server started, for sessions left over from before.
    pub startup: Duration,
}

impl Default for EphemeralGrace {
    fn default() -> Self {
        Self {
            unwatched: Duration::from_secs(5),
            unclaimed: Duration::from_secs(30),
            startup: Duration::from_secs(30),
        }
    }
}

impl EphemeralGrace {
    fn of(self, wait: Wait) -> Duration {
        match wait {
            Wait::Unwatched => self.unwatched,
            Wait::Unclaimed => self.unclaimed,
            Wait::Startup => self.startup,
        }
    }
}

/// Which grace a check waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// The last watcher left.
    Unwatched,
    /// Just created ephemeral.
    Unclaimed,
    /// Found by the startup sweep.
    Startup,
}

/// One due check of a session.
#[derive(Clone, Copy, Debug)]
struct Check {
    session: SessionId,
    generation: u64,
    wait: Wait,
}

/// Open session streams per session.
#[derive(Default)]
struct Watch {
    count: usize,
    /// Bumped each time `count` drops to zero; 0 until the first leave.
    generation: u64,
}

/// Counts the open `StreamSessionEvents` streams of every session and
/// queues the checks of unused ephemeral sessions for the reaper.
#[derive(Clone)]
pub(crate) struct SessionWatchers {
    inner: Arc<Inner>,
}

struct Inner {
    watches: Mutex<HashMap<SessionId, Watch>>,
    generations: AtomicU64,
    checks: mpsc::UnboundedSender<Check>,
    /// Taken by the one reaper task (the first `router` built).
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Check>>>,
    reaper: AtomicBool,
}

impl Default for SessionWatchers {
    fn default() -> Self {
        let (checks, receiver) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                watches: Mutex::default(),
                generations: AtomicU64::new(0),
                checks,
                receiver: Mutex::new(Some(receiver)),
                reaper: AtomicBool::new(false),
            }),
        }
    }
}

/// Held by one open session stream; dropping it (the client went away, or
/// the stream ended) may make a check of the session due.
pub(crate) struct WatchGuard {
    watchers: SessionWatchers,
    session: SessionId,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.watchers.release(self.session);
    }
}

impl SessionWatchers {
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Watch>> {
        self.inner
            .watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A stream on `session` opened; it watches the session until the guard
    /// drops.
    pub(crate) fn watch(&self, session: SessionId) -> WatchGuard {
        self.lock().entry(session).or_default().count += 1;
        WatchGuard {
            watchers: self.clone(),
            session,
        }
    }

    /// How many open streams watch `session`.
    #[cfg(test)]
    fn watching(&self, session: SessionId) -> usize {
        self.lock().get(&session).map_or(0, |watch| watch.count)
    }

    /// Queue a check of `session` after `wait`'s grace, valid while its
    /// watch state stays as it is now.
    pub(crate) fn schedule(&self, session: SessionId, wait: Wait) {
        let generation = self
            .lock()
            .get(&session)
            .map_or(0, |watch| watch.generation);
        let _ = self.inner.checks.send(Check {
            session,
            generation,
            wait,
        });
    }

    fn release(&self, session: SessionId) {
        let mut watches = self.lock();
        let Some(watch) = watches.get_mut(&session) else {
            return;
        };
        watch.count = watch.count.saturating_sub(1);
        if watch.count > 0 {
            return;
        }
        let generation = self.inner.generations.fetch_add(1, Ordering::Relaxed) + 1;
        watch.generation = generation;
        drop(watches);
        let _ = self.inner.checks.send(Check {
            session,
            generation,
            wait: Wait::Unwatched,
        });
    }

    /// Whether `check` still stands: nobody watches its session and no later
    /// leave scheduled a newer check.
    fn current(&self, check: &Check) -> bool {
        match self.lock().get(&check.session) {
            None => check.generation == 0,
            Some(watch) => watch.count == 0 && watch.generation == check.generation,
        }
    }

    /// Drop the bookkeeping of a session that needs no more checks.
    fn forget(&self, check: &Check) {
        let mut watches = self.lock();
        if watches
            .get(&check.session)
            .is_some_and(|watch| watch.count == 0 && watch.generation == check.generation)
        {
            watches.remove(&check.session);
        }
    }

    fn claim_receiver(&self) -> Option<mpsc::UnboundedReceiver<Check>> {
        if self.inner.reaper.swap(true, Ordering::AcqRel) {
            return None;
        }
        self.inner
            .receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Start the reaper (and the startup sweep) for `state`'s watchers unless
/// one already runs.
pub(crate) fn spawn_reaper(state: ServerState) {
    let Some(mut checks) = state.watchers.claim_receiver() else {
        return;
    };
    let sweeping = state.clone();
    tokio::spawn(async move { sweep(&sweeping).await });
    tokio::spawn(async move {
        while let Some(check) = checks.recv().await {
            let state = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(state.ephemeral_grace.of(check.wait)).await;
                reap(&state, check).await;
            });
        }
    });
}

/// Queue a startup check of every unused ephemeral session left over.
async fn sweep(state: &ServerState) {
    let rows = match state.engine.store().list_sessions().await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("ephemeral session sweep could not list sessions: {error}");
            return;
        }
    };
    for row in rows {
        if row.events > SWEEP_MAX_EVENTS {
            continue;
        }
        if state
            .engine
            .read_projection_shared(row.session)
            .await
            .is_ok_and(|projection| projection.session.ephemeral)
        {
            state.watchers.schedule(row.session, Wait::Startup);
        }
    }
}

/// Run one due check: drop the session when it is still unused, idle, and
/// unwatched.
async fn reap(state: &ServerState, check: Check) {
    let session = check.session;
    if !state.watchers.current(&check) || !still_ephemeral(state, &check).await {
        return;
    }
    // Hold the admission slot: no prompt, command, shell turn, or Workflow
    // run is admitted while the session is re-read and deleted. Quiet: no
    // busy notice for a session about to go.
    let Some(_reserved) = state.reserve_quiet(session) else {
        // A turn runs; its first message keeps the session. Look again later
        // in case it had none.
        state.watchers.schedule(session, Wait::Unwatched);
        return;
    };
    let Ok(projection) = state.engine.read_projection_shared(session).await else {
        return;
    };
    if !projection.session.ephemeral || !state.watchers.current(&check) {
        return;
    }
    match state
        .engine
        .store()
        .delete_session_at(session, EventSeq(projection.last_seq))
        .await
    {
        Ok(true) => {
            state.watchers.forget(&check);
            tracing::info!(%session, "dropped an unused ephemeral session");
            // A Project's session count changed. A temporary session's
            // scratch directory stays on disk (ADR-0024: never deleted).
            if projection.session.project.is_some() {
                state.notify_projects_updated();
            }
            if projection.session.parent.is_none() {
                state
                    .session_list
                    .publish(SessionListNotice::Deleted { session });
            }
        }
        // Written since the read: check again (a use clears the mark).
        Ok(false) => state.watchers.schedule(session, Wait::Unwatched),
        Err(error) => {
            tracing::warn!(%session, "could not drop an unused ephemeral session: {error}");
        }
    }
}

/// Whether `check`'s session still exists and is unused; forgets it when
/// not.
async fn still_ephemeral(state: &ServerState, check: &Check) -> bool {
    let ephemeral = state
        .engine
        .read_projection_shared(check.session)
        .await
        .is_ok_and(|projection| projection.session.ephemeral);
    if !ephemeral {
        state.watchers.forget(check);
    }
    ephemeral
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_last_leave_makes_a_check_due_and_a_new_watch_voids_it() {
        let watchers = SessionWatchers::default();
        let mut receiver = watchers.claim_receiver().unwrap();
        assert!(watchers.claim_receiver().is_none(), "one reaper");
        let session = SessionId::new();

        let first = watchers.watch(session);
        let second = watchers.watch(session);
        assert_eq!(watchers.watching(session), 2);
        drop(first);
        assert!(receiver.try_recv().is_err(), "still watched: nothing due");
        drop(second);
        let due = receiver.try_recv().unwrap();
        assert_eq!(due.wait, Wait::Unwatched);
        assert!(watchers.current(&due));

        // Back within the grace: the pending check no longer stands.
        let back = watchers.watch(session);
        assert!(!watchers.current(&due));
        drop(back);
        let newer = receiver.try_recv().unwrap();
        assert!(!watchers.current(&due), "an older check stays void");
        assert!(watchers.current(&newer));
        watchers.forget(&newer);
        assert_eq!(watchers.watching(session), 0);
    }

    #[test]
    fn a_creation_check_stands_until_someone_watches() {
        let watchers = SessionWatchers::default();
        let mut receiver = watchers.claim_receiver().unwrap();
        let session = SessionId::new();
        watchers.schedule(session, Wait::Unclaimed);
        let due = receiver.try_recv().unwrap();
        assert!(watchers.current(&due));
        let viewer = watchers.watch(session);
        assert!(!watchers.current(&due));
        drop(viewer);
    }
}
