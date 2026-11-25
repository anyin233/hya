//! Live JSONL streaming for headless `exec --json`.
//!
//! Events are persisted per-envelope by the engine regardless; this module
//! streams the same durable envelopes to stdout as they are broadcast, so an
//! abnormally terminated run still leaves a usable partial trajectory on
//! stdout. Envelopes are filtered to the exec session (optionally its descendants) and
//! to durable seqs (`seq == 0` frames are live-only and never persisted),
//! deduplicated by seq — concurrent writers (turn loop, resident batches,
//! mailbox) can publish out of seq order, so arrival order is usually but not
//! strictly ascending — with an unbounded healthy startup catch-up
//! (`ReplayPass::Initial`) and `read_timeout()`-bounded lag and final replays.
//! The resulting stream contains what was broadcast plus what the final read's
//! SQL snapshot and acknowledged queue tail show (an event whose producer was
//! cancelled and whose SQLite COMMIT lands after that final snapshot is not in
//! it); if either a lag replay or the final replay hits a storage fault or
//! timeout, one warning prints to stderr after stdout flushes.

use std::collections::HashSet;

use hya_proto::{Envelope, Event, SessionId};
use hya_store::{SessionStore, StoreError};
use tokio::sync::{broadcast, oneshot};

#[derive(Clone)]
pub(crate) enum StreamScope {
    Single(SessionId),
    Tree {
        root: SessionId,
        sessions: HashSet<SessionId>,
        outsiders: HashSet<SessionId>,
        store: Box<SessionStore>,
    },
}

impl StreamScope {
    fn accepts(&self, envelope: &Envelope) -> bool {
        if envelope.seq.0 == 0 {
            return false;
        }
        match self {
            Self::Single(id) => envelope.event.session() == Some(*id),
            Self::Tree { sessions, .. } => envelope
                .event
                .session()
                .is_some_and(|id| sessions.contains(&id)),
        }
    }

    // A scoped replay already checked the authoritative parentage, including
    // events missed on the bus. Seed membership before processing live frames.
    fn replayed(&mut self, envelope: &Envelope) {
        if let Self::Tree { sessions, .. } = self
            && let Some(id) = envelope.event.session()
        {
            sessions.insert(id);
        }
    }

    async fn observe_live(&mut self, env: &Envelope) -> Result<(), StoreError> {
        let Self::Tree {
            sessions,
            outsiders,
            store,
            ..
        } = self
        else {
            return Ok(());
        };
        let Some(id) = env.event.session() else {
            return Ok(());
        };
        if env.seq.0 == 0 || sessions.contains(&id) || outsiders.contains(&id) {
            return Ok(());
        }
        let mut path = HashSet::new();
        let mut cursor = id;
        let timeout = store.read_timeout();
        let member = tokio::time::timeout(timeout, async {
            loop {
                if sessions.contains(&cursor) {
                    return Ok::<_, StoreError>(Some(true));
                }
                if outsiders.contains(&cursor) || !path.insert(cursor) {
                    return Ok(Some(false));
                }
                let first = store.replay_page(cursor, 0, 1).await?;
                match first.first().map(|env| &env.event) {
                    Some(Event::SessionCreated {
                        parent: Some(parent),
                        ..
                    }) => cursor = *parent,
                    Some(Event::SessionCreated { parent: None, .. }) => return Ok(Some(false)),
                    _ => return Ok(None),
                }
            }
        })
        .await
        .map_err(|_| StoreError::StorageTimeout {
            operation: "tree child creation",
            timeout,
        })?;
        match member? {
            Some(true) => sessions.extend(path),
            Some(false) => outsiders.extend(path),
            None => {}
        }
        Ok(())
    }

    pub(crate) fn tree(root: SessionId, store: SessionStore) -> Self {
        Self::Tree {
            root,
            sessions: HashSet::from([root]),
            outsiders: HashSet::new(),
            store: Box::new(store),
        }
    }

    pub(crate) async fn replay(
        &self,
        store: &SessionStore,
        pass: ReplayPass,
    ) -> Result<Vec<Envelope>, StoreError> {
        match (self, pass) {
            (Self::Single(id), ReplayPass::Initial) => store.replay(*id).await,
            (Self::Single(id), ReplayPass::Lag) => bounded_replay(store, *id).await,
            (Self::Tree { root, .. }, ReplayPass::Initial) => store.replay_tree(*root).await,
            (Self::Tree { root, .. }, ReplayPass::Lag) => bounded_replay_tree(store, *root).await,
        }
    }
}

pub(crate) fn write_envelope(
    out: &mut impl std::io::Write,
    envelope: &Envelope,
) -> std::io::Result<()> {
    let line = serde_json::to_string(envelope).map_err(|e| std::io::Error::other(e.to_string()))?;
    writeln!(out, "{line}")
}

/// Replay `session` bounded by the store's `read_timeout()` so a stalled read
/// surfaces `StoreError::StorageTimeout` rather than blocking `exec --json`.
pub(crate) async fn bounded_replay(
    store: &SessionStore,
    session: SessionId,
) -> Result<Vec<Envelope>, StoreError> {
    let timeout = store.read_timeout();
    tokio::time::timeout(timeout, store.replay(session))
        .await
        .map_err(|_| StoreError::StorageTimeout {
            operation: "replay",
            timeout,
        })?
}

/// Which pass invoked [`stream_until_done`]'s replay callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayPass {
    /// Startup catch-up before live streaming begins (uses unbounded healthy `SessionStore::replay`).
    Initial,
    /// Mid-stream lag resync (uses [`bounded_replay`] cut off at `read_timeout()`).
    Lag,
}

async fn bounded_replay_tree(
    store: &SessionStore,
    root: SessionId,
) -> Result<Vec<Envelope>, StoreError> {
    let timeout = store.read_timeout();
    tokio::time::timeout(timeout, store.replay_tree(root))
        .await
        .map_err(|_| StoreError::StorageTimeout {
            operation: "replay_tree",
            timeout,
        })?
}

/// Incremental JSONL writer deduplicating by durable seq.
pub(crate) struct JsonStreamPrinter<W: std::io::Write> {
    out: W,
    scope: StreamScope,
    printed: HashSet<u64>,
    first_lag_fault: Option<String>,
}

impl<W: std::io::Write> JsonStreamPrinter<W> {
    /// Start with nothing printed; live-only frames stay excluded by the
    /// durable-seq rule.
    pub(crate) fn new(out: W, scope: StreamScope) -> Self {
        Self {
            out,
            scope,
            printed: HashSet::new(),
            first_lag_fault: None,
        }
    }

    /// Print `envelope` when eligible and not yet printed. Returns whether
    /// the envelope was printed.
    pub(crate) fn print(&mut self, envelope: &Envelope) -> std::io::Result<bool> {
        if !self.scope.accepts(envelope) || !self.printed.insert(envelope.seq.0) {
            return Ok(false);
        }
        write_envelope(&mut self.out, envelope)?;
        Ok(true)
    }

    async fn live(&mut self, envelope: &Envelope) -> std::io::Result<()> {
        match self.scope.observe_live(envelope).await {
            Ok(()) => {
                self.print(envelope)?;
            }
            Err(error) if error.is_storage_fault() => {
                self.first_lag_fault
                    .get_or_insert_with(|| error.to_string());
            }
            Err(error) => return Err(std::io::Error::other(error)),
        }
        Ok(())
    }

    fn replayed(&mut self, envelope: &Envelope) -> std::io::Result<()> {
        self.scope.replayed(envelope);
        self.print(envelope)?;
        Ok(())
    }

    /// Flush the underlying writer.
    pub(crate) fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Drain the engine bus until `done` fires, printing eligible envelopes as
/// they arrive; on Lagged, resync through `replay` (durable log is the
/// authority). Returns the printer for a final tail flush.
pub(crate) async fn stream_until_done<W, F, Fut>(
    mut rx: broadcast::Receiver<Envelope>,
    scope: StreamScope,
    mut done: oneshot::Receiver<()>,
    mut replay: F,
    out: W,
) -> std::io::Result<JsonStreamPrinter<W>>
where
    W: std::io::Write,
    F: FnMut(ReplayPass) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Envelope>, StoreError>>,
{
    let mut printer = JsonStreamPrinter::new(out, scope);
    // Catch-up pass: envelopes persisted before this subscription existed
    // (notably `session_created`) print first, keeping stdout ordered.
    if let Ok(envelopes) = replay(ReplayPass::Initial).await {
        for envelope in &envelopes {
            printer.replayed(envelope)?;
        }
    }
    loop {
        tokio::select! {
            biased;
            result = rx.recv() => match result {
                Ok(envelope) => {
                    printer.live(&envelope).await?;
                }
                Err(broadcast::error::RecvError::Lagged(_missed)) => {
                    match replay(ReplayPass::Lag).await {
                        Ok(envelopes) => {
                            for envelope in &envelopes {
                                printer.replayed(envelope)?;
                            }
                        }
                        Err(error) if error.is_storage_fault() => {
                            if printer.first_lag_fault.is_none() {
                                printer.first_lag_fault = Some(error.to_string());
                            }
                        }
                        Err(_) => {
                            // Match origin/main: keep live streaming on non-storage lag error;
                            // final full-session replay surfaces it at turn end.
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = &mut done => break,
        }
    }
    // Drain anything buffered between the turn completing and `done` firing so
    // stdout never drops trailing durable events.
    while let Ok(envelope) = rx.try_recv() {
        printer.live(&envelope).await?;
    }
    Ok(printer)
}

/// Finish the `exec --json` stream after all turns drain by replaying the
/// session log through the printer's sequence set.
///
/// Always queries the durable log (whether or not the bus lagged). When either
/// a lag replay or the final replay hits a storage fault or timeout, flushes
/// stdout as pure JSONL and writes one warning line to `err`; returns `Ok(())`
/// when the final replay succeeded or hit a storage fault, and propagates
/// non-storage final replay errors after flushing stdout and emitting any
/// recorded lag warning.
pub(crate) async fn finish_json_stream<W, E, F, Fut>(
    mut printer: JsonStreamPrinter<W>,
    replay: F,
    mut err: E,
) -> anyhow::Result<()>
where
    W: std::io::Write,
    E: std::io::Write,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Envelope>, StoreError>>,
{
    let mut warning = printer.first_lag_fault.take();
    let mut fatal: Option<StoreError> = None;
    match replay().await {
        Ok(envelopes) => {
            for env in &envelopes {
                printer.replayed(env)?;
            }
        }
        Err(error) if error.is_storage_fault() => {
            warning.get_or_insert_with(|| error.to_string());
        }
        Err(error) => {
            fatal = Some(error);
        }
    }
    let flush_res = printer.flush();
    if let Some(reason) = warning {
        let _ = writeln!(
            err,
            "warning: json replay unavailable ({reason}); event stream may be incomplete"
        );
    }
    if let Some(error) = fatal {
        return Err(error.into());
    }
    flush_res?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use hya_proto::{Event, EventSeq, SessionId};

    fn envelope(seq: u64, session: SessionId) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event: Event::SessionCreated {
                session,
                parent: None,
                agent: "build".into(),
                model: "hya/offline".into(),
                workdir: ".".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        }
    }

    fn title(seq: u64, session: SessionId) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event: Event::SessionTitled {
                session,
                title: format!("title-{seq}"),
            },
        }
    }

    fn child(seq: u64, id: SessionId, parent: SessionId) -> Envelope {
        let mut created = envelope(seq, id);
        if let Event::SessionCreated { parent: field, .. } = &mut created.event {
            *field = Some(parent);
        }
        created
    }

    #[derive(Clone, Default)]
    struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn seqs(&self) -> Vec<u64> {
            self.text()
                .lines()
                .map(|line| serde_json::from_str::<Envelope>(line).unwrap().seq.0)
                .collect()
        }
    }

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct CleanDir(std::path::PathBuf);
    impl Drop for CleanDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    fn lock_wal_reads(db: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{}-shm", db.display()))?;
        let lock = libc::flock {
            l_type: libc::F_WRLCK as _,
            l_whence: libc::SEEK_SET as _,
            l_start: 120,
            l_len: 8,
            l_pid: 0,
        };
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(file)
    }

    #[test]
    fn printer_filters_deduplicates_and_handles_out_of_order_seq() {
        let session = SessionId::new();
        let other = SessionId::new();
        let mut printer =
            JsonStreamPrinter::new(SharedBuf::default(), StreamScope::Single(session));

        assert!(!printer.print(&envelope(0, session)).unwrap());
        assert!(!printer.print(&envelope(1, other)).unwrap());
        assert!(printer.print(&envelope(3, session)).unwrap());
        assert!(printer.print(&envelope(1, session)).unwrap());
        assert!(!printer.print(&envelope(3, session)).unwrap());
        assert!(!printer.print(&envelope(1, session)).unwrap());
        assert_eq!(printer.out.seqs(), vec![3, 1]);
    }

    #[tokio::test]
    async fn tree_stream_orders_descendants_and_recovers_missed_via_lag_and_final_replay()
    -> anyhow::Result<()> {
        let [root, member, grandchild, other, old] = std::array::from_fn(|_| SessionId::new());
        let store = SessionStore::connect_memory().await?;
        for (id, e) in [
            (root, envelope(1, root)),
            (member, child(2, member, root)),
            (grandchild, child(3, grandchild, member)),
            (old, envelope(4, old)),
        ] {
            store.append_event(id, &e.event).await?;
        }
        let (tx, rx) = broadcast::channel(16);
        for event in [
            child(2, member, root),
            child(3, grandchild, member),
            title(6, grandchild),
            title(4, member),
            title(6, grandchild),
            envelope(5, other),
            title(7, other),
            child(8, other, root),
            child(9, old, root),
            title(10, old),
        ] {
            tx.send(event)?;
        }
        drop(tx);
        let (_done, done_rx) = oneshot::channel();
        let out = SharedBuf::default();
        stream_until_done(
            rx,
            StreamScope::tree(root, store),
            done_rx,
            |_| std::future::ready(Ok(vec![envelope(1, root)])),
            out.clone(),
        )
        .await?;
        assert_eq!(out.seqs(), [1, 2, 3, 6, 4]);

        // Tree lag + final replay recover missed descendants.
        let (tx, rx) = broadcast::channel(1);
        tx.send(child(2, member, root))?;
        tx.send(title(4, member))?;
        drop(tx);
        let (_done, done_rx) = oneshot::channel();
        let out = SharedBuf::default();
        let printer = stream_until_done(
            rx,
            StreamScope::tree(root, SessionStore::connect_memory().await?),
            done_rx,
            |pass| {
                std::future::ready(Ok(match pass {
                    ReplayPass::Initial => vec![envelope(1, root)],
                    ReplayPass::Lag => {
                        vec![envelope(1, root), child(2, member, root), title(3, member)]
                    }
                }))
            },
            out.clone(),
        )
        .await?;
        finish_json_stream(
            printer,
            || {
                std::future::ready(Ok(vec![
                    title(4, member),
                    child(5, grandchild, member),
                    title(6, grandchild),
                ]))
            },
            Vec::new(),
        )
        .await?;
        assert_eq!(out.seqs(), [1, 2, 3, 4, 5, 6]);
        Ok(())
    }

    #[tokio::test]
    async fn single_stream_catch_up_live_lag_resync_and_final_unbroadcast_replay() {
        let session = SessionId::new();
        let (tx, rx) = broadcast::channel(8);
        let (done_tx, done_rx) = oneshot::channel();
        tx.send(title(3, session)).unwrap();
        tx.send(title(9, SessionId::new())).unwrap();
        let _ = done_tx.send(());

        let out = SharedBuf::default();
        let mut init_calls = 0;
        let printer = stream_until_done(
            rx,
            StreamScope::Single(session),
            done_rx,
            |pass| {
                assert_eq!(pass, ReplayPass::Initial);
                init_calls += 1;
                std::future::ready(Ok(vec![title(1, session)]))
            },
            out.clone(),
        )
        .await
        .unwrap();
        assert_eq!(init_calls, 1);
        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            || {
                std::future::ready(Ok(vec![
                    title(1, session),
                    title(2, session),
                    title(3, session),
                ]))
            },
            &mut stderr,
        )
        .await
        .unwrap();
        assert!(stderr.is_empty() && out.seqs() == vec![1, 3, 2]);

        // Burst lag recovers delayed lower sequence after live high.
        struct NotifyWrite<'a> {
            out: SharedBuf,
            hook: &'a mut dyn FnMut(),
        }
        impl std::io::Write for NotifyWrite<'_> {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                let n = self.out.write(b)?;
                if b.ends_with(b"\n") {
                    (self.hook)();
                }
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.out.flush()
            }
        }
        let (tx, rx) = broadcast::channel(1);
        let (done_tx, done_rx) = oneshot::channel();
        let (mut done_tx, mut calls, mut writes) = (Some(done_tx), 0_usize, 0_usize);
        let out = SharedBuf::default();
        let mut hook = || {
            writes += 1;
            if writes == 1 {
                let _ = tx.send(title(2, session));
                let _ = tx.send(title(4, session));
            }
        };
        let _ = tx.send(title(3, session));
        let disk = vec![title(2, session), title(3, session), title(4, session)];
        let printer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream_until_done(
                rx,
                StreamScope::Single(session),
                done_rx,
                |pass| {
                    calls += 1;
                    let rows = match pass {
                        ReplayPass::Initial => Vec::new(),
                        ReplayPass::Lag => {
                            if let Some(d) = done_tx.take() {
                                let _ = d.send(());
                            }
                            disk.clone()
                        }
                    };
                    std::future::ready(Ok(rows))
                },
                NotifyWrite {
                    out: out.clone(),
                    hook: &mut hook,
                },
            ),
        )
        .await
        .expect("lag replay triggers done")
        .unwrap();
        assert_eq!((calls, out.seqs()), (2, vec![3, 2, 4]));
        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            || std::future::ready(Ok(disk.clone())),
            &mut stderr,
        )
        .await
        .unwrap();
        assert!(stderr.is_empty() && out.seqs() == vec![3, 2, 4]);
    }

    #[tokio::test]
    async fn lag_and_final_storage_faults_warn_once_and_flush_before_stderr() {
        let timeout_err = |op: &'static str| StoreError::StorageTimeout {
            operation: op,
            timeout: std::time::Duration::from_secs(5),
        };
        for (final_op, expected) in [(None, vec![1, 3, 2]), (Some("final-replay"), vec![1, 3])] {
            let s = SessionId::new();
            let (bus, rx) = broadcast::channel(1);
            let (done, completed) = oneshot::channel();
            let mut done = Some(done);
            let out = SharedBuf::default();
            bus.send(title(2, s)).unwrap();
            bus.send(title(3, s)).unwrap();
            let printer = stream_until_done(
                rx,
                StreamScope::Single(s),
                completed,
                |pass| {
                    std::future::ready(match pass {
                        ReplayPass::Initial => Ok(vec![title(1, s)]),
                        ReplayPass::Lag => {
                            done.take().unwrap().send(()).unwrap();
                            Err(timeout_err("lag-replay"))
                        }
                    })
                },
                out.clone(),
            )
            .await
            .unwrap();
            let mut stderr = Vec::new();
            finish_json_stream(
                printer,
                || {
                    std::future::ready(match final_op {
                        Some(op) => Err(timeout_err(op)),
                        None => Ok(vec![title(1, s), title(2, s), title(3, s)]),
                    })
                },
                &mut stderr,
            )
            .await
            .unwrap();
            let warn = String::from_utf8(stderr).unwrap();
            assert_eq!((out.seqs(), warn.lines().count()), (expected, 1));
            assert!(warn.contains("incomplete") && warn.contains("lag-replay"));
        }

        // Flush order + logical error propagation still emits prior lag warning and flushes stdout.
        struct OrderWriter(
            std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
            &'static str,
            &'static str,
        );
        impl std::io::Write for OrderWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().push(self.1);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.lock().unwrap().push(self.2);
                Ok(())
            }
        }
        let s = SessionId::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut printer = JsonStreamPrinter::new(
            std::io::BufWriter::new(OrderWriter(events.clone(), "out-w", "out-f")),
            StreamScope::Single(s),
        );
        printer.print(&title(1, s)).unwrap();
        printer.first_lag_fault = Some("lag timeout".into());
        let err = finish_json_stream(
            printer,
            || async { Err(StoreError::SessionKeyData("corrupt key".into())) },
            OrderWriter(events.clone(), "err-w", "err-f"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("corrupt key"));
        let rec = events.lock().unwrap().clone();
        assert!(
            rec.iter().position(|t| *t == "out-f") < rec.iter().position(|t| *t == "err-w"),
            "{rec:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_replay_timeouts_unlinked_faults_and_tree_ancestry_retry() -> anyhow::Result<()>
    {
        let _dir =
            CleanDir(std::env::temp_dir().join(format!("hya-exec-stream-{}", SessionId::new())));
        std::fs::create_dir_all(&_dir.0)?;
        let db = _dir.0.join("hya.db");
        let [root, parent, member] = std::array::from_fn(|_| SessionId::new());
        let store = SessionStore::connect_with_timeouts(
            db.to_str().unwrap(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(40),
        )
        .await?;
        let created = child(3, member, parent);
        for env in [envelope(1, root), child(2, parent, root), created.clone()] {
            store
                .append_event(env.event.session().unwrap(), &env.event)
                .await?;
        }
        let cold_store = SessionStore::connect(db.to_str().unwrap()).await?;
        assert_eq!(bounded_replay(&store, root).await?.len(), 1);
        assert_eq!(store.replay_page(member, 0, 1).await?.len(), 1);

        #[cfg(target_os = "linux")]
        {
            let lock = lock_wal_reads(&db)?;
            let unlocker = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(80));
                drop(lock);
            });
            let started = std::time::Instant::now();
            let initial = StreamScope::Single(root)
                .replay(&store, ReplayPass::Initial)
                .await?;
            unlocker.join().unwrap();
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(60) && initial.len() == 1
            );

            let held = lock_wal_reads(&db)?;
            assert!(matches!(
                StreamScope::Single(root)
                    .replay(&store, ReplayPass::Lag)
                    .await
                    .unwrap_err(),
                StoreError::StorageTimeout {
                    operation: "replay",
                    ..
                }
            ));

            let out = SharedBuf::default();
            let mut printer = JsonStreamPrinter::new(out.clone(), StreamScope::Single(root));
            printer.print(&envelope(1, root))?;
            let mut stderr = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_millis(400),
                finish_json_stream(printer, || bounded_replay(&store, root), &mut stderr),
            )
            .await
            .expect("bounded by READ_TIMEOUT")?;
            let warn = String::from_utf8(stderr)?;
            assert!(
                warn.lines().count() == 1
                    && warn.contains("incomplete")
                    && warn.contains("timeout")
                    && out.seqs() == [1]
            );

            let mut tp = JsonStreamPrinter::new(
                SharedBuf::default(),
                StreamScope::tree(root, store.clone()),
            );
            tp.live(&created).await?;
            assert!(
                tp.first_lag_fault
                    .as_deref()
                    .is_some_and(|m| m.contains("timeout"))
            );
            drop(held);
            tp.live(&title(4, member)).await?;
            assert_eq!(tp.out.seqs(), [4]);
        }

        drop(store);
        std::fs::remove_file(&db)?;
        let out = SharedBuf::default();
        let mut printer = JsonStreamPrinter::new(out.clone(), StreamScope::Single(root));
        printer.print(&envelope(1, root))?;
        let mut stderr = Vec::new();
        finish_json_stream(printer, || bounded_replay(&cold_store, root), &mut stderr).await?;
        let warn = String::from_utf8(stderr)?;
        assert!(warn.lines().count() == 1 && warn.contains("incomplete") && out.seqs() == [1]);
        Ok(())
    }
}
