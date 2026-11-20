//! Ordered append queue and single writer task for bounded synchronous commits.
//!
//! File-backed read-write stores route appends through one writer task running
//! one `BEGIN IMMEDIATE` SQLite transaction per operation in submission order,
//! with event sequence numbers assigned in process.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use hya_proto::{Envelope, Event, EventSeq, SessionId, now_millis};
use sqlx::SqlitePool;
use tokio::sync::{Notify, oneshot};

use crate::StoreError;
use crate::projection_cache::{Folded, PreparedFold, ProjectionCache};

/// Maximum wait for opening and migrating a file-backed store before falling back to memory.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum synchronous wait for a healthy write commit before degrading.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum wait for fallback-guarded store reads regardless of write health.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum wait for the append queue to drain to disk on process exit.
pub const EXIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

const RETRY_BASE: Duration = Duration::from_millis(25);
const RETRY_MAX: Duration = Duration::from_millis(250);

/// Outcome of draining the append queue when a store-owning command exits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitDrainReport {
    /// No degradation or fallback occurred; all writes were already on disk.
    Clean,
    /// The store degraded during the run, then flushed every queued op before exit.
    Recovered {
        /// Database file path.
        path: PathBuf,
        /// First storage fault or timeout reason recorded during the run.
        reason: String,
    },
    /// Unflushed events or blobs remained queued at exit (timed out or halted).
    Unpersisted {
        /// Database file path.
        path: PathBuf,
        /// Number of unpersisted event rows remaining in the queue.
        events: u64,
        /// Total bytes of unpersisted event JSON payloads and queued blobs.
        bytes: u64,
        /// Primary reason persistence stopped or timed out.
        reason: String,
    },
    /// Opening the requested path failed or timed out, so the process ran in memory.
    InMemoryFallback {
        /// Requested database path.
        path: PathBuf,
        /// Number of in-memory event rows created during the run (`None` if querying failed).
        events: Option<u64>,
        /// Total bytes of in-memory event JSON payloads and blobs (`None` if querying failed).
        bytes: Option<u64>,
        /// Reason opening fell back to memory.
        reason: String,
    },
}

impl ExitDrainReport {
    /// Format a single stderr status line when the store experienced degradation,
    /// loss, or an in-memory fallback. Returns `None` for [`Self::Clean`].
    #[must_use]
    pub fn format_stderr_line(&self) -> Option<String> {
        match self {
            Self::Clean => None,
            Self::Recovered { path, reason } => Some(format!(
                "hya: store {} recovered after storage fault ({reason}); all events persisted",
                path.display()
            )),
            Self::Unpersisted {
                path,
                events,
                bytes,
                reason,
            } => Some(format!(
                "hya: store {} left {events} event(s) ({bytes} bytes, blobs included) unpersisted at exit: {reason}",
                path.display()
            )),
            Self::InMemoryFallback {
                path,
                events,
                bytes,
                reason,
            } => {
                let counts = match (events, bytes) {
                    (Some(events), Some(bytes)) => {
                        format!("{events} event(s), {bytes} bytes, blobs included")
                    }
                    _ => "unknown event/byte count".to_string(),
                };
                Some(format!(
                    "hya: store {} ran in memory; nothing was persisted ({counts}): {reason}",
                    path.display()
                ))
            }
        }
    }
}

/// Current durability health of a [`crate::SessionStore`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreHealth {
    /// Reads and appends commit synchronously to the backing database.
    Healthy,
    /// Appends return from memory while the writer task retries disk persistence.
    Degraded {
        /// First fault or timeout reason that triggered degradation.
        reason: String,
    },
}

enum WaitOutcome {
    Committed,
    Degraded,
    LogicalError(StoreError),
}

struct QueuedOp {
    blobs: Vec<(SessionId, String, Vec<u8>)>,
    events: Vec<(SessionId, Envelope, String)>,
}

struct QueueEntry {
    id: u64,
    op: Arc<QueuedOp>,
    cache_applied: bool,
    waiter: Option<oneshot::Sender<WaitOutcome>>,
}

const MAX_TRACKED_SESSIONS: usize = 4096;

#[derive(Default)]
struct SessionSeqMap {
    seqs: HashMap<SessionId, u64>,
    floor: u64,
}

impl SessionSeqMap {
    fn get(&self, session: SessionId) -> u64 {
        self.seqs.get(&session).copied().unwrap_or(self.floor)
    }

    fn record(&mut self, session: SessionId, seq: u64) {
        if let Some(prev) = self.seqs.get_mut(&session) {
            *prev = (*prev).max(seq);
            return;
        }
        if self.seqs.len() >= MAX_TRACKED_SESSIONS
            && let Some((evict_id, evict_seq)) = self
                .seqs
                .iter()
                .min_by_key(|(_, s)| *s)
                .map(|(k, s)| (*k, *s))
        {
            self.floor = self.floor.max(evict_seq);
            self.seqs.remove(&evict_id);
        }
        self.seqs.insert(session, seq.max(self.floor));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn read(path: &Path) -> Result<Self, StoreError> {
        let meta = std::fs::metadata(path).map_err(|err| StoreError::from(sqlx::Error::Io(err)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(Self {
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            Ok(Self {})
        }
    }
}

#[derive(Default)]
struct QueueState {
    next_seq: u64,
    next_op_id: u64,
    last_completed_op_id: u64,
    raw_tx_pending_or_open: usize,
    committed_seqs: SessionSeqMap,
    degraded: Option<String>,
    ever_degraded: Option<String>,
    flush_halted: Option<String>,
    identity_halted: Option<String>,
    queue: VecDeque<QueueEntry>,
}

struct Inner {
    db_path: PathBuf,
    db_identity: FileIdentity,
    writer_pool: SqlitePool,
    projections: Arc<ProjectionCache>,
    write_timeout: Duration,
    stopped: AtomicBool,
    state: Mutex<QueueState>,
    wake: Notify,
    queue_progress: Notify,
    raw_tx_done: Notify,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct DropStop(Arc<Inner>);

/// Active raw-transaction barrier held by [`crate::StoreTransaction`].
pub(crate) struct RawTxGuard {
    inner: Arc<Inner>,
    max_seq: u64,
    recorded_sessions: HashMap<SessionId, u64>,
    confirmed_commit: bool,
}

impl RawTxGuard {
    pub(crate) fn record_event(&mut self, session: SessionId, seq: u64) {
        self.max_seq = self.max_seq.max(seq);
        self.recorded_sessions
            .entry(session)
            .and_modify(|max| *max = (*max).max(seq))
            .or_insert(seq);
    }

    pub(crate) fn finish_commit(mut self, committed: &[(SessionId, Envelope)]) {
        self.confirmed_commit = true;
        if !committed.is_empty() {
            let mut st = self.inner.state();
            for (session, env) in committed {
                st.committed_seqs.record(*session, env.seq.0);
                self.inner.projections.apply_envelope(*session, env, false);
            }
        }
    }
}

impl Drop for RawTxGuard {
    fn drop(&mut self) {
        let mut st = self.inner.state();
        st.next_seq = st.next_seq.max(self.max_seq);
        if !self.confirmed_commit && !self.recorded_sessions.is_empty() {
            for (&session, &seq) in &self.recorded_sessions {
                st.committed_seqs.record(session, seq);
            }
            self.inner
                .projections
                .remove_all(self.recorded_sessions.keys().copied());
        }
        st.raw_tx_pending_or_open = st.raw_tx_pending_or_open.saturating_sub(1);
        if st.raw_tx_pending_or_open == 0 {
            self.inner.raw_tx_done.notify_waiters();
        }
    }
}

/// Snapshot of per-session commit watermark and acknowledged queued tail captured at `begin_fold`.
pub(crate) struct FoldSnapshot {
    pub(crate) was_degraded: bool,
    t0_committed: u64,
    pre_tail: Vec<Envelope>,
}

/// Outcome of publishing a prepared disk fold against queue state.
pub(crate) enum FoldOutcome {
    Cached {
        folded: Folded,
        persisted_seq: u64,
        unpersisted: u64,
        allow_snapshot: bool,
    },
    Uncached {
        folded: Folded,
        persisted_seq: u64,
        unpersisted: u64,
    },
}

impl Drop for DropStop {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
        self.0.queue_progress.notify_waiters();
        self.0.raw_tx_done.notify_waiters();
    }
}

/// Ordered append-queue handle shared by every [`crate::SessionStore`] clone.
#[derive(Clone)]
pub(crate) struct WriteQueue(Arc<DropStop>);

impl WriteQueue {
    /// Initialize high-water sequence state and start the single writer task.
    pub(crate) async fn start(
        db_path: PathBuf,
        writer_pool: SqlitePool,
        projections: Arc<ProjectionCache>,
        write_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let id_path = db_path.clone();
        let db_identity =
            crate::run_on_detached_thread(move || FileIdentity::read(&id_path)).await?;
        let next_seq = read_disk_high_water(&writer_pool).await?;
        let inner = Arc::new(Inner {
            db_path,
            db_identity,
            writer_pool,
            projections,
            write_timeout,
            stopped: AtomicBool::new(false),
            state: Mutex::new(QueueState {
                next_seq,
                ..Default::default()
            }),
            wake: Notify::new(),
            queue_progress: Notify::new(),
            raw_tx_done: Notify::new(),
        });
        tokio::spawn(run_writer_task(Arc::downgrade(&inner)));
        Ok(Self(Arc::new(DropStop(inner))))
    }

    pub(crate) fn write_timeout(&self) -> Duration {
        self.0.0.write_timeout
    }

    pub(crate) fn health(&self) -> StoreHealth {
        match &self.0.0.state().degraded {
            Some(reason) => StoreHealth::Degraded {
                reason: reason.clone(),
            },
            None => StoreHealth::Healthy,
        }
    }

    pub(crate) fn is_degraded(&self) -> bool {
        self.0.0.state().degraded.is_some()
    }

    /// Verify the database file still holds its opened identity before allowing a disk read.
    ///
    /// Once a file-identity failure has halted disk access, every disk read refuses with
    /// [`StoreError::StoreDegraded`] instead of reading a replacement file.
    pub(crate) fn check_disk_readable(&self) -> Result<(), StoreError> {
        let inner = &self.0.0;
        {
            let st = inner.state();
            if let Some(reason) = &st.identity_halted {
                return Err(StoreError::StoreDegraded {
                    reason: reason.clone(),
                });
            }
        }
        if let Err(fault) = verify_file_identity(&inner.db_path, inner.db_identity) {
            match fault {
                IdentityFault::Halted(reason) => {
                    let (reason, waiters) = {
                        let mut st = inner.state();
                        let waiters = halt_identity_locked(inner, &mut st, &reason);
                        let reason = st
                            .identity_halted
                            .clone()
                            .unwrap_or_else(|| sanitize_reason(&reason));
                        (reason, waiters)
                    };
                    release_degraded_waiters(waiters);
                    return Err(StoreError::StoreDegraded { reason });
                }
                // Transient metadata errors (e.g. EACCES, EIO) do not permanently halt disk reads.
                IdentityFault::Transient(_) => {}
            }
        }
        Ok(())
    }

    /// Wait up to `timeout` for queued appends to flush to disk, returning an
    /// exit durability summary for stderr reporting. Returns immediately when
    /// flushing is permanently halted.
    pub(crate) async fn drain_for_exit(&self, timeout: Duration) -> ExitDrainReport {
        let inner = &self.0.0;
        inner.wake.notify_one();
        let deadline = Instant::now() + timeout;
        loop {
            let notified = inner.queue_progress.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let empty_ever_degraded = {
                let st = inner.state();
                if st.queue.is_empty() && st.flush_halted.is_none() {
                    Some(st.ever_degraded.clone())
                } else if let Some(reason) = st.flush_halted.clone() {
                    let (events, bytes) = queued_unpersisted_totals(&st);
                    return ExitDrainReport::Unpersisted {
                        path: inner.db_path.clone(),
                        events,
                        bytes,
                        reason,
                    };
                } else {
                    None
                }
            };
            if let Some(ever_degraded) = empty_ever_degraded {
                return check_empty_drain_identity(inner, ever_degraded);
            }
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() || tokio::time::timeout(rem, notified).await.is_err() {
                let empty_ever_degraded = {
                    let st = inner.state();
                    if st.queue.is_empty() && st.flush_halted.is_none() {
                        Some(st.ever_degraded.clone())
                    } else {
                        let (events, bytes) = queued_unpersisted_totals(&st);
                        let reason = st
                            .flush_halted
                            .clone()
                            .or_else(|| st.degraded.clone())
                            .or_else(|| st.ever_degraded.clone())
                            .unwrap_or_else(|| format!("exit drain timed out after {timeout:?}"));
                        return ExitDrainReport::Unpersisted {
                            path: inner.db_path.clone(),
                            events,
                            bytes,
                            reason,
                        };
                    }
                };
                if let Some(ever_degraded) = empty_ever_degraded {
                    return check_empty_drain_identity(inner, ever_degraded);
                }
            }
        }
    }

    /// Wait for queued appends ahead to complete, then acquire an exclusive writer tx and guard.
    pub(crate) async fn acquire_raw_transaction(
        &self,
    ) -> Result<(sqlx::Transaction<'static, sqlx::Sqlite>, RawTxGuard), StoreError> {
        let inner = &self.0.0;
        let deadline = Instant::now() + inner.write_timeout;
        let (guard, barrier_op_id) = {
            let mut st = inner.state();
            st.raw_tx_pending_or_open = st.raw_tx_pending_or_open.saturating_add(1);
            let barrier_op_id = st
                .queue
                .back()
                .map_or(st.last_completed_op_id, |entry| entry.id);
            (
                RawTxGuard {
                    inner: Arc::clone(inner),
                    max_seq: 0,
                    recorded_sessions: HashMap::new(),
                    confirmed_commit: false,
                },
                barrier_op_id,
            )
        };
        loop {
            let notified = inner.queue_progress.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let ready = {
                let st = inner.state();
                if let Some(reason) = st.flush_halted.as_ref().or(st.degraded.as_ref()) {
                    return Err(StoreError::StoreDegraded {
                        reason: reason.clone(),
                    });
                }
                st.last_completed_op_id >= barrier_op_id
            };
            if ready {
                break;
            }
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() || tokio::time::timeout(rem, notified).await.is_err() {
                return Err(StoreError::StorageTimeout {
                    operation: "begin_transaction",
                    timeout: inner.write_timeout,
                });
            }
        }
        if let Err(fault) = verify_file_identity(&inner.db_path, inner.db_identity) {
            let reason = match fault {
                IdentityFault::Halted(reason) => {
                    let (reason, waiters) = {
                        let mut st = inner.state();
                        let waiters = halt_identity_locked(inner, &mut st, &reason);
                        let reason = st
                            .identity_halted
                            .clone()
                            .unwrap_or_else(|| sanitize_reason(&reason));
                        (reason, waiters)
                    };
                    release_degraded_waiters(waiters);
                    reason
                }
                // Keep queue state clean: the barrier drained the queue, so no
                // writer task run will wake up to clear `degraded` before later raw txs.
                IdentityFault::Transient(reason) => sanitize_reason(&reason),
            };
            return Err(StoreError::StoreDegraded { reason });
        }
        let tx = inner.writer_pool.begin_with("BEGIN IMMEDIATE").await?;
        Ok((tx, guard))
    }

    /// Snapshot acknowledged queued envelopes for `session` strictly after `after` in submission order.
    pub(crate) fn queued_envelopes_for(&self, session: SessionId, after: u64) -> Vec<Envelope> {
        collect_queued(&self.0.0.state(), session, after)
    }

    /// Return whether the acknowledged queue holds at least one event for `session`.
    pub(crate) fn has_queued_event(&self, session: SessionId) -> bool {
        let st = self.0.0.state();
        st.queue
            .iter()
            .filter(|entry| entry.cache_applied)
            .flat_map(|entry| entry.op.events.iter())
            .any(|(s, _, _)| *s == session)
    }

    /// Return the latest acknowledged queued blob content for `(session, hash)`, if any.
    pub(crate) fn queued_blob(&self, session: SessionId, hash: &str) -> Option<Vec<u8>> {
        let st = self.0.0.state();
        st.queue
            .iter()
            .rev()
            .filter(|entry| entry.cache_applied)
            .flat_map(|entry| entry.op.blobs.iter().rev())
            .find(|(s, h, _)| *s == session && h == hash)
            .map(|(_, _, content)| content.clone())
    }

    /// Snapshot `hash -> size` across acknowledged queued blobs for `session`.
    pub(crate) fn queued_blob_sizes(&self, session: SessionId) -> HashMap<String, u64> {
        let st = self.0.0.state();
        let mut sizes = HashMap::new();
        for entry in st.queue.iter().filter(|entry| entry.cache_applied) {
            for (s, hash, content) in &entry.op.blobs {
                if *s == session {
                    sizes
                        .entry(hash.clone())
                        .or_insert_with(|| u64::try_from(content.len()).unwrap_or(u64::MAX));
                }
            }
        }
        sizes
    }

    /// Capture queue state and any acknowledged queued tail for `session` before a disk fold.
    pub(crate) fn begin_fold(&self, session: SessionId) -> FoldSnapshot {
        let st = self.0.0.state();
        FoldSnapshot {
            was_degraded: st.degraded.is_some(),
            t0_committed: st.committed_seqs.get(session),
            pre_tail: collect_queued(&st, session, 0),
        }
    }

    /// Publish a prepared disk fold atomically with queue state and acknowledged queued tails.
    pub(crate) fn finish_fold(
        &self,
        session: SessionId,
        snapshot: FoldSnapshot,
        prepared: PreparedFold,
        persisted_seq: u64,
        unpersisted: u64,
    ) -> FoldOutcome {
        let inner = &self.0.0;
        let st = inner.state();
        let degraded = st.degraded.is_some();
        let mut tail = snapshot.pre_tail;
        let pre_max = tail.last().map_or(0, |e| e.seq.0);
        let covered_max = prepared.final_seq().max(pre_max);
        let now_committed = st.committed_seqs.get(session);
        if now_committed > snapshot.t0_committed && now_committed > covered_max {
            let folded = inner.projections.finish_without_caching(prepared, &tail);
            let (next_persisted, next_unpersisted) = if folded.rebuilt {
                (0, folded.applied)
            } else {
                (persisted_seq, unpersisted.saturating_add(folded.applied))
            };
            return FoldOutcome::Uncached {
                folded,
                persisted_seq: next_persisted,
                unpersisted: next_unpersisted,
            };
        }
        for env in collect_queued(&st, session, pre_max) {
            tail.push(env);
        }
        let (folded, p, u) =
            inner
                .projections
                .advance(session, prepared, persisted_seq, unpersisted, &tail);
        FoldOutcome::Cached {
            folded,
            persisted_seq: p,
            unpersisted: u,
            allow_snapshot: !degraded,
        }
    }

    /// Submit one blob+event append batch strictly in submission order.
    pub(crate) async fn submit(
        &self,
        blobs: Vec<(SessionId, String, Vec<u8>)>,
        events: Vec<(SessionId, Event)>,
    ) -> Result<Vec<Envelope>, StoreError> {
        if blobs.is_empty() && events.is_empty() {
            return Ok(Vec::new());
        }
        let mut serialized = Vec::with_capacity(events.len());
        for (session, event) in events {
            crate::validate_event_for_append(&event)?;
            let payload = serde_json::to_string(&event)?;
            serialized.push((session, event, payload));
        }
        let inner = &self.0.0;
        let (id, envelopes, rx) = loop {
            let notified = inner.raw_tx_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut st = inner.state();
                if st.raw_tx_pending_or_open == 0 {
                    st.next_op_id = st.next_op_id.saturating_add(1);
                    let id = st.next_op_id;
                    let mut prepared = Vec::with_capacity(serialized.len());
                    let mut envelopes = Vec::with_capacity(serialized.len());
                    for (session, event, payload) in serialized {
                        st.next_seq = st.next_seq.saturating_add(1);
                        let envelope = Envelope {
                            seq: EventSeq(st.next_seq),
                            ts_millis: now_millis(),
                            event,
                        };
                        envelopes.push(envelope.clone());
                        prepared.push((session, envelope, payload));
                    }
                    let degraded = st.degraded.is_some();
                    let (tx, rx) = (!degraded).then(oneshot::channel).unzip();
                    let op = Arc::new(QueuedOp {
                        blobs,
                        events: prepared,
                    });
                    if degraded {
                        apply_cache(&inner.projections, &op, true);
                    }
                    st.queue.push_back(QueueEntry {
                        id,
                        op,
                        cache_applied: degraded,
                        waiter: tx,
                    });
                    break (id, envelopes, rx);
                }
            }
            notified.await;
        };
        inner.wake.notify_one();
        let Some(mut receiver) = rx else {
            return Ok(envelopes);
        };
        if let Ok(res) = tokio::time::timeout(inner.write_timeout, &mut receiver).await {
            return resolve_outcome(res, envelopes);
        }
        let (won, waiters) = {
            let mut st = inner.state();
            let took = st
                .queue
                .iter_mut()
                .find(|entry| entry.id == id)
                .and_then(|entry| entry.waiter.take())
                .is_some();
            if took {
                let msg = format!(
                    "write exceeded {:?} synchronous commit limit",
                    inner.write_timeout
                );
                (true, degrade_locked(inner, &mut st, &msg))
            } else {
                (false, Vec::new())
            }
        };
        waiters
            .into_iter()
            .for_each(|tx| drop(tx.send(WaitOutcome::Degraded)));
        if won {
            Ok(envelopes)
        } else {
            resolve_outcome(receiver.await, envelopes)
        }
    }
}

fn resolve_outcome<T>(
    res: Result<WaitOutcome, oneshot::error::RecvError>,
    value: T,
) -> Result<T, StoreError> {
    match res {
        Ok(WaitOutcome::Committed | WaitOutcome::Degraded) => Ok(value),
        Ok(WaitOutcome::LogicalError(error)) => Err(error),
        Err(_) => Err(StoreError::from(sqlx::Error::WorkerCrashed)),
    }
}

fn sanitize_reason(reason: &str) -> String {
    reason.replace(['\n', '\r'], " ").trim().to_string()
}

fn degrade_locked(
    inner: &Inner,
    st: &mut QueueState,
    reason: &str,
) -> Vec<oneshot::Sender<WaitOutcome>> {
    let msg = sanitize_reason(reason);
    st.degraded.get_or_insert_with(|| msg.clone());
    st.ever_degraded.get_or_insert(msg);
    inner.queue_progress.notify_waiters();
    let mut waiters = Vec::new();
    for queued in &mut st.queue {
        if !queued.cache_applied {
            queued.cache_applied = true;
            apply_cache(&inner.projections, &queued.op, true);
        }
        waiters.extend(queued.waiter.take());
    }
    waiters
}

fn halt_flusher_locked(
    inner: &Inner,
    st: &mut QueueState,
    reason: &str,
) -> Vec<oneshot::Sender<WaitOutcome>> {
    let msg = sanitize_reason(reason);
    let waiters = degrade_locked(inner, st, &msg);
    st.flush_halted.get_or_insert(msg);
    inner.queue_progress.notify_waiters();
    waiters
}

fn halt_identity_locked(
    inner: &Inner,
    st: &mut QueueState,
    reason: &str,
) -> Vec<oneshot::Sender<WaitOutcome>> {
    let msg = sanitize_reason(reason);
    st.identity_halted.get_or_insert_with(|| msg.clone());
    halt_flusher_locked(inner, st, &msg)
}

fn release_degraded_waiters(waiters: Vec<oneshot::Sender<WaitOutcome>>) {
    waiters
        .into_iter()
        .for_each(|tx| drop(tx.send(WaitOutcome::Degraded)));
}

fn empty_drain_ok_report(inner: &Inner, ever_degraded: Option<String>) -> ExitDrainReport {
    match ever_degraded {
        Some(reason) => ExitDrainReport::Recovered {
            path: inner.db_path.clone(),
            reason,
        },
        None => ExitDrainReport::Clean,
    }
}

fn check_empty_drain_identity(inner: &Inner, ever_degraded: Option<String>) -> ExitDrainReport {
    match verify_file_identity(&inner.db_path, inner.db_identity) {
        Ok(()) | Err(IdentityFault::Transient(_)) => empty_drain_ok_report(inner, ever_degraded),
        Err(IdentityFault::Halted(reason)) => {
            let (events, bytes, reason, waiters) = {
                let mut st = inner.state();
                let waiters = halt_identity_locked(inner, &mut st, &reason);
                let (events, bytes) = queued_unpersisted_totals(&st);
                let reason = st
                    .flush_halted
                    .clone()
                    .unwrap_or_else(|| sanitize_reason(&reason));
                (events, bytes, reason, waiters)
            };
            release_degraded_waiters(waiters);
            ExitDrainReport::Unpersisted {
                path: inner.db_path.clone(),
                events,
                bytes,
                reason,
            }
        }
    }
}

enum IdentityFault {
    Halted(String),
    Transient(String),
}

fn verify_file_identity(path: &Path, expected: FileIdentity) -> Result<(), IdentityFault> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if meta.dev() != expected.dev || meta.ino() != expected.ino {
                    return Err(IdentityFault::Halted(format!(
                        "database file {} was replaced (device/inode changed)",
                        path.display()
                    )));
                }
            }
            #[cfg(not(unix))]
            let _ = expected;
            if meta.len() == 0 {
                return Err(IdentityFault::Halted(format!(
                    "database file {} was truncated to 0 bytes",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(err) => {
            let reason = format!(
                "database file {} is missing or inaccessible: {err}",
                path.display()
            );
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) {
                Err(IdentityFault::Halted(reason))
            } else {
                Err(IdentityFault::Transient(reason))
            }
        }
    }
}

fn apply_cache(cache: &ProjectionCache, op: &QueuedOp, init_on_create: bool) {
    for (session, env, _) in &op.events {
        cache.apply_envelope(*session, env, init_on_create);
    }
}

fn collect_queued(st: &QueueState, session: SessionId, after: u64) -> Vec<Envelope> {
    st.queue
        .iter()
        .filter(|entry| entry.cache_applied)
        .flat_map(|entry| entry.op.events.iter())
        .filter(|(s, env, _)| *s == session && env.seq.0 > after)
        .map(|(_, env, _)| env.clone())
        .collect()
}

fn queued_unpersisted_totals(st: &QueueState) -> (u64, u64) {
    let mut events: u64 = 0;
    let mut bytes: u64 = 0;
    for entry in &st.queue {
        events = events.saturating_add(u64::try_from(entry.op.events.len()).unwrap_or(u64::MAX));
        for (_, _, payload) in &entry.op.events {
            bytes = bytes.saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
        }
        for (_, _, content) in &entry.op.blobs {
            bytes = bytes.saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
        }
    }
    (events, bytes)
}

enum OpOutcome {
    Committed,
    Halted(String),
}

async fn degrade_and_backoff(inner: Arc<Inner>, reason: &str, backoff: &mut Duration) -> bool {
    let waiters = degrade_locked(&inner, &mut inner.state(), reason);
    release_degraded_waiters(waiters);
    if inner.stopped.load(Ordering::Acquire) {
        return false;
    }
    drop(inner);
    tokio::time::sleep(*backoff).await;
    *backoff = (*backoff * 2).min(RETRY_MAX);
    true
}

async fn run_writer_task(weak: Weak<Inner>) {
    let mut backoff = RETRY_BASE;
    while let Some(inner) = weak.upgrade() {
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        let (op_id, op) = {
            let notified = inner.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let front = {
                let st = inner.state();
                st.flush_halted
                    .is_none()
                    .then(|| st.queue.front().map(|e| (e.id, Arc::clone(&e.op))))
                    .flatten()
            };
            let Some(item) = front else {
                notified.await;
                continue;
            };
            item
        };
        if let Err(fault) = verify_file_identity(&inner.db_path, inner.db_identity) {
            match fault {
                IdentityFault::Halted(reason) => {
                    let waiters = halt_identity_locked(&inner, &mut inner.state(), &reason);
                    release_degraded_waiters(waiters);
                }
                IdentityFault::Transient(reason) => {
                    if !degrade_and_backoff(inner, &reason, &mut backoff).await {
                        return;
                    }
                }
            }
            continue;
        }
        match execute_op_on_disk(&inner.writer_pool, &op).await {
            Ok(OpOutcome::Committed) => {
                backoff = RETRY_BASE;
                let waiter = {
                    let mut st = inner.state();
                    for (session, env, _) in &op.events {
                        st.committed_seqs.record(*session, env.seq.0);
                    }
                    st.last_completed_op_id = st.last_completed_op_id.max(op_id);
                    inner.queue_progress.notify_waiters();
                    let waiter = if st.queue.front().is_some_and(|h| h.id == op_id)
                        && let Some(mut entry) = st.queue.pop_front()
                    {
                        if !entry.cache_applied {
                            apply_cache(&inner.projections, &entry.op, false);
                        }
                        entry.waiter.take()
                    } else {
                        None
                    };
                    if st.queue.is_empty() && st.flush_halted.is_none() {
                        st.degraded = None;
                    }
                    waiter
                };
                if let Some(tx) = waiter {
                    let _ = tx.send(WaitOutcome::Committed);
                }
            }
            Ok(OpOutcome::Halted(reason)) => {
                let waiters = halt_flusher_locked(&inner, &mut inner.state(), &reason);
                release_degraded_waiters(waiters);
            }
            Err(error) if !error.is_storage_fault() => {
                let waiter = {
                    let mut st = inner.state();
                    match st.queue.front_mut().filter(|h| h.id == op_id) {
                        Some(front) if front.waiter.is_some() => {
                            let tx = front.waiter.take();
                            let _ = st.queue.pop_front();
                            st.last_completed_op_id = st.last_completed_op_id.max(op_id);
                            inner.queue_progress.notify_waiters();
                            tx
                        }
                        _ => {
                            let msg = sanitize_reason(&error.to_string());
                            let _ = halt_flusher_locked(&inner, &mut st, &msg);
                            None
                        }
                    }
                };
                if let Some(tx) = waiter {
                    let _ = tx.send(WaitOutcome::LogicalError(error));
                }
            }
            Err(error) => {
                if !degrade_and_backoff(inner, &error.to_string(), &mut backoff).await {
                    return;
                }
            }
        }
    }
}

fn is_event_log_pk_conflict(error: &StoreError) -> bool {
    let StoreError::Sqlite(sqlx_err) = error else {
        return false;
    };
    let sqlx::Error::Database(db) = sqlx_err.as_ref() else {
        return false;
    };
    db.code()
        .and_then(|c| c.parse::<u32>().ok())
        .is_some_and(|c| c == 1555 || ((c & 0xff) == 19 && db.message().contains("event_log.seq")))
}

async fn verify_events_already_landed(
    pool: &SqlitePool,
    op: &QueuedOp,
) -> Result<bool, StoreError> {
    for (session, envelope, payload) in &op.events {
        let seq = i64::try_from(envelope.seq.0).map_err(|_| {
            StoreError::from(sqlx::Error::Protocol("sequence exceeds i64 range".into()))
        })?;
        let row: Option<(Vec<u8>, String, i64)> =
            sqlx::query_as("SELECT session_id, payload, ts FROM event_log WHERE seq = ?")
                .bind(seq)
                .fetch_optional(pool)
                .await?;
        let Some((disk_session, disk_payload, disk_ts)) = row else {
            return Ok(false);
        };
        if disk_session != session.storage_key()
            || disk_payload != *payload
            || disk_ts != envelope.ts_millis
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn execute_op_on_disk(pool: &SqlitePool, op: &QueuedOp) -> Result<OpOutcome, StoreError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (session, hash, content) in &op.blobs {
        sqlx::query(
            "INSERT OR IGNORE INTO file_blob (session_id, hash, size, content) VALUES (?, ?, ?, ?)",
        )
        .bind(session.storage_key())
        .bind(hash)
        .bind(i64::try_from(content.len()).unwrap_or(i64::MAX))
        .bind(content.as_slice())
        .execute(&mut *tx)
        .await?;
    }
    for (session, envelope, payload) in &op.events {
        match crate::insert_event_in_tx(
            &mut tx,
            *session,
            &envelope.event,
            Some((envelope.seq.0, envelope.ts_millis, payload)),
        )
        .await
        {
            Ok(_) => {}
            Err(error) if is_event_log_pk_conflict(&error) => {
                let conflicted_seq = envelope.seq.0;
                drop(tx);
                return if verify_events_already_landed(pool, op).await? {
                    Ok(OpOutcome::Committed)
                } else {
                    Ok(OpOutcome::Halted(format!(
                        "foreign writer conflict on event_log.seq {conflicted_seq}"
                    )))
                };
            }
            Err(error) => return Err(error),
        }
    }
    tx.commit().await?;
    Ok(OpOutcome::Committed)
}

async fn read_disk_high_water(pool: &SqlitePool) -> Result<u64, StoreError> {
    let mut conn = pool.acquire().await?;
    let max_event: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM event_log")
        .fetch_one(&mut *conn)
        .await?;
    let max_auto: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'event_log'), 0)",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(max_event.max(max_auto).max(0) as u64)
}

pub(crate) async fn run_with_timeout<F, T>(
    operation: &'static str,
    timeout: Duration,
    fut: F,
) -> Result<T, StoreError>
where
    F: Future<Output = Result<T, StoreError>>,
{
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| StoreError::StorageTimeout { operation, timeout })?
}
