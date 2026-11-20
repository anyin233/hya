//! Live JSONL streaming for headless `exec --json`.
//!
//! Events are persisted per-envelope by the engine regardless; this module
//! streams the same durable envelopes to stdout as they are broadcast, so an
//! abnormally terminated run still leaves a usable partial trajectory on
//! stdout (the run2 lesson). Envelopes are filtered to the exec session and
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

use hya_proto::{Envelope, SessionId};
use hya_store::{SessionStore, StoreError};
use tokio::sync::{broadcast, oneshot};

/// Whether `envelope` belongs on the exec JSONL stream for `session`: same
/// session and a durable seq (`0` is live-only).
#[must_use]
pub(crate) fn eligible(envelope: &Envelope, session: SessionId) -> bool {
    envelope.event.session() == Some(session) && envelope.seq.0 != 0
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

/// Dispatch a [`ReplayPass`] against `store`.
pub(crate) async fn replay_for_pass(
    store: &SessionStore,
    session: SessionId,
    pass: ReplayPass,
) -> Result<Vec<Envelope>, StoreError> {
    match pass {
        ReplayPass::Initial => store.replay(session).await,
        ReplayPass::Lag => bounded_replay(store, session).await,
    }
}

/// Incremental JSONL writer deduplicating by durable seq.
pub(crate) struct JsonStreamPrinter<W: std::io::Write> {
    out: W,
    printed: HashSet<u64>,
    first_lag_fault: Option<String>,
}

impl<W: std::io::Write> JsonStreamPrinter<W> {
    /// Start with nothing printed; live-only frames stay excluded by the
    /// durable-seq rule.
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            printed: HashSet::new(),
            first_lag_fault: None,
        }
    }

    /// Print `envelope` when eligible and not yet printed. Returns whether
    /// the envelope was printed.
    pub(crate) fn print(
        &mut self,
        envelope: &Envelope,
        session: SessionId,
    ) -> std::io::Result<bool> {
        if !eligible(envelope, session) || !self.printed.insert(envelope.seq.0) {
            return Ok(false);
        }
        write_envelope(&mut self.out, envelope)?;
        Ok(true)
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
    session: SessionId,
    mut done: oneshot::Receiver<()>,
    mut replay: F,
    out: W,
) -> std::io::Result<JsonStreamPrinter<W>>
where
    W: std::io::Write,
    F: FnMut(ReplayPass) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Envelope>, StoreError>>,
{
    let mut printer = JsonStreamPrinter::new(out);
    // Catch-up pass: envelopes persisted before this subscription existed
    // (notably `session_created`) print first, keeping stdout ordered.
    if let Ok(envelopes) = replay(ReplayPass::Initial).await {
        for envelope in &envelopes {
            printer.print(envelope, session)?;
        }
    }
    loop {
        tokio::select! {
            biased;
            result = rx.recv() => match result {
                Ok(envelope) => {
                    printer.print(&envelope, session)?;
                }
                Err(broadcast::error::RecvError::Lagged(_missed)) => {
                    match replay(ReplayPass::Lag).await {
                        Ok(envelopes) => {
                            for envelope in &envelopes {
                                printer.print(envelope, session)?;
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
        printer.print(&envelope, session)?;
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
    session: SessionId,
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
                printer.print(env, session)?;
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

    #[test]
    fn printer_filters_session_durable_seq_and_deduplicates() {
        let session = SessionId::new();
        let other = SessionId::new();
        let mut printer = JsonStreamPrinter::new(Vec::new());

        assert!(!printer.print(&envelope(0, session), session).unwrap());
        assert!(!printer.print(&envelope(1, other), session).unwrap());
        assert!(printer.print(&envelope(1, session), session).unwrap());
        // Duplicates are dropped regardless of arrival order.
        assert!(!printer.print(&envelope(1, session), session).unwrap());
        assert!(printer.print(&envelope(3, session), session).unwrap());

        let written = String::from_utf8(printer.out).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "only the two eligible envelopes print");
        for line in &lines {
            let parsed: Envelope = serde_json::from_str(line).unwrap();
            assert_eq!(parsed.event.session(), Some(session));
        }
        assert!(lines[0].contains("\"seq\":1"));
        assert!(lines[1].contains("\"seq\":3"));
    }

    #[test]
    fn printer_handles_out_of_order_arrival_from_concurrent_writers() {
        let session = SessionId::new();
        let mut printer = JsonStreamPrinter::new(Vec::new());
        // Concurrent writers (turn loop, resident batches, mailbox) can
        // publish a higher seq before a lower one; both must print.
        assert!(printer.print(&envelope(3, session), session).unwrap());
        assert!(printer.print(&envelope(2, session), session).unwrap());
        assert!(!printer.print(&envelope(3, session), session).unwrap());
        let written = String::from_utf8(printer.out).unwrap();
        assert_eq!(written.lines().count(), 2);
    }

    #[tokio::test]
    async fn stream_prints_live_frames_in_order_and_stops_on_done() {
        let session = SessionId::new();
        let bus = broadcast::channel::<Envelope>(16);
        let (done_tx, done_rx) = oneshot::channel::<()>();

        for seq in 1..=4_u64 {
            let _ = bus.0.send(envelope(seq, session));
        }
        let _ = bus.0.send(envelope(5, SessionId::new()));
        let _ = done_tx.send(());

        let replay_calls = std::cell::Cell::new(0);
        let printer = stream_until_done(
            bus.1,
            session,
            done_rx,
            |pass| {
                assert_eq!(pass, ReplayPass::Initial);
                replay_calls.set(replay_calls.get() + 1);
                async { Ok(Vec::new()) }
            },
            Vec::new(),
        )
        .await
        .unwrap();

        let written = String::from_utf8(printer.out).unwrap();
        let seqs: Vec<u64> = written
            .lines()
            .map(|line| serde_json::from_str::<Envelope>(line).unwrap().seq.0)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3, 4], "foreign sessions are filtered");
        assert_eq!(replay_calls.get(), 1, "only the initial catch-up ran");
    }

    #[tokio::test]
    async fn initial_catch_up_prints_pre_subscription_envelopes_in_order() {
        let session = SessionId::new();
        let (tx, rx) = broadcast::channel::<Envelope>(16);
        let (done_tx, done_rx) = oneshot::channel::<()>();

        // seq 1 was persisted before the subscription existed; seq 2 arrives
        // live on the bus.
        let catch_up = vec![envelope(1, session)];
        let _ = tx.send(envelope(2, session));
        let _ = done_tx.send(());

        let mut calls = 0;
        let printer = stream_until_done(
            rx,
            session,
            done_rx,
            |pass| {
                calls += 1;
                let envelopes = match pass {
                    ReplayPass::Initial => catch_up.clone(),
                    ReplayPass::Lag => Vec::new(),
                };
                async move { Ok(envelopes) }
            },
            Vec::new(),
        )
        .await
        .unwrap();

        let written = String::from_utf8(printer.out).unwrap();
        let seqs: Vec<u64> = written
            .lines()
            .map(|line| serde_json::from_str::<Envelope>(line).unwrap().seq.0)
            .collect();
        assert_eq!(seqs, vec![1, 2], "catch-up precedes live frames");
    }

    #[tokio::test]
    async fn lagged_receiver_resyncs_through_replay() {
        let session = SessionId::new();
        // Capacity 1: any burst lags a slow receiver.
        let (tx, rx) = broadcast::channel::<Envelope>(1);
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let mut done_tx = Some(done_tx);

        tx.send(envelope(2, session)).unwrap();
        tx.send(envelope(3, session)).unwrap();

        let replay_calls = std::cell::Cell::new(0);
        let printer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream_until_done(
                rx,
                session,
                done_rx,
                |pass| {
                    let call = replay_calls.get() + 1;
                    replay_calls.set(call);
                    let envelopes = match pass {
                        ReplayPass::Initial => vec![envelope(1, session)],
                        ReplayPass::Lag => {
                            if let Some(done) = done_tx.take() {
                                let _ = done.send(());
                            }
                            vec![
                                envelope(1, session),
                                envelope(2, session),
                                envelope(3, session),
                            ]
                        }
                    };
                    async move { Ok(envelopes) }
                },
                Vec::new(),
            ),
        )
        .await
        .expect("lag replay must run and trigger done")
        .unwrap();

        let written = String::from_utf8(printer.out).unwrap();
        let seqs: Vec<u64> = written
            .lines()
            .map(|line| serde_json::from_str::<Envelope>(line).unwrap().seq.0)
            .collect();
        assert_eq!(
            replay_calls.get(),
            2,
            "must run startup catch-up plus lag replay"
        );
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "replay recovers everything missed in order"
        );
    }

    #[tokio::test]
    async fn lag_replay_recovers_delayed_lower_sequence_after_live_high() {
        let session = SessionId::new();
        let (tx, rx) = broadcast::channel(1);
        let (done_tx, done_rx) = oneshot::channel();
        let mut done_tx = Some(done_tx);
        let mut calls = 0_usize;
        let mut writes = 0_usize;
        struct NotifyWrite<'a> {
            out: SharedBuf,
            hook: &'a mut dyn FnMut(),
        }
        impl std::io::Write for NotifyWrite<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let n = self.out.write(bytes)?;
                if bytes.ends_with(b"\n") {
                    (self.hook)();
                }
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.out.flush()
            }
        }
        let out = SharedBuf::default();
        let mut hook = || {
            writes += 1;
            if writes == 1 {
                // Simulate delayed seq 2 broadcast and a burst seq 4 that lags seq 2.
                let _ = tx.send(title(2, session));
                let _ = tx.send(title(4, session));
            }
        };
        let _ = tx.send(title(3, session));
        let printer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream_until_done(
                rx,
                session,
                done_rx,
                |pass| {
                    calls += 1;
                    let disk = match pass {
                        ReplayPass::Initial => Vec::new(),
                        ReplayPass::Lag => {
                            if let Some(done) = done_tx.take() {
                                let _ = done.send(());
                            }
                            vec![title(2, session), title(3, session), title(4, session)]
                        }
                    };
                    std::future::ready(Ok(disk))
                },
                NotifyWrite {
                    out: out.clone(),
                    hook: &mut hook,
                },
            ),
        )
        .await
        .expect("lag replay must run and trigger done")
        .unwrap();

        assert_eq!(
            calls, 2,
            "must recover through lag replay before finalization"
        );
        assert_eq!(
            out.seqs(),
            vec![3, 2, 4],
            "lag replay must recover [2, 4] after live 3 before finalization"
        );

        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            session,
            || {
                std::future::ready(Ok(vec![
                    title(2, session),
                    title(3, session),
                    title(4, session),
                ]))
            },
            &mut stderr,
        )
        .await
        .unwrap();
        assert!(stderr.is_empty());
        assert_eq!(out.seqs(), vec![3, 2, 4]);
    }

    #[tokio::test]
    async fn final_replay_emits_unbroadcast_row_without_lag() {
        let session = SessionId::new();
        let (tx, rx) = broadcast::channel(8);
        let (done_tx, done_rx) = oneshot::channel();
        let _ = tx.send(title(3, session));
        let _ = done_tx.send(());

        let out = SharedBuf::default();
        let printer = stream_until_done(
            rx,
            session,
            done_rx,
            |pass| {
                assert_eq!(pass, ReplayPass::Initial);
                std::future::ready(Ok(vec![title(1, session)]))
            },
            out.clone(),
        )
        .await
        .unwrap();
        let mut final_called = false;
        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            session,
            || {
                final_called = true;
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
        assert!(final_called, "final replay must always run at stream end");
        assert!(stderr.is_empty());
        let mut seqs = out.seqs();
        seqs.sort_unstable();
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "unbroadcast seq 2 must be printed once"
        );
    }

    #[tokio::test]
    async fn final_replay_storage_fault_and_timeout_warn_once_and_preserve_stdout() {
        struct CleanDir(std::path::PathBuf);
        impl Drop for CleanDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _dir =
            CleanDir(std::env::temp_dir().join(format!("hya-exec-stream-{}", SessionId::new())));
        std::fs::create_dir_all(&_dir.0).unwrap();
        let db = _dir.0.join("hya.db");

        let session = SessionId::new();
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            std::time::Duration::from_millis(40),
        )
        .await
        .unwrap();
        let (seq1, ts1) = store
            .append_event(session, &envelope(1, session).event)
            .await
            .unwrap();
        // Warm `read_pool` on a separate store handle too for unlinked cold failure below.
        let cold_store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        // Verify initial healthy replay sees seq 1.
        assert_eq!(bounded_replay(&store, session).await.unwrap().len(), 1);

        // 1. StorageTimeout when replay stalls past `read_timeout` (Linux OFD shm lock).
        #[cfg(target_os = "linux")]
        {
            struct ShmWriteLock(std::fs::File);
            impl Drop for ShmWriteLock {
                fn drop(&mut self) {
                    use std::os::unix::io::AsRawFd;
                    let lock = libc::flock {
                        l_type: libc::F_UNLCK as libc::c_short,
                        l_whence: libc::SEEK_SET as libc::c_short,
                        l_start: 120,
                        l_len: 8,
                        l_pid: 0,
                    };
                    unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
                }
            }
            let shm_path = format!("{}-shm", db.display());
            let shm = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&shm_path)
                .unwrap();
            {
                use std::os::unix::io::AsRawFd;
                let lock = libc::flock {
                    l_type: libc::F_WRLCK as libc::c_short,
                    l_whence: libc::SEEK_SET as libc::c_short,
                    l_start: 120,
                    l_len: 8,
                    l_pid: 0,
                };
                assert_eq!(
                    unsafe { libc::fcntl(shm.as_raw_fd(), libc::F_OFD_SETLK, &lock) },
                    0
                );
            }
            let _shm_guard = ShmWriteLock(shm);

            let (tx, rx) = broadcast::channel::<Envelope>(8);
            let (done_tx, done_rx) = oneshot::channel::<()>();
            let _ = tx.send(Envelope {
                seq: seq1,
                ts_millis: ts1,
                event: envelope(1, session).event,
            });
            let _ = tx.send(title(2, session));
            let _ = done_tx.send(());

            let out = SharedBuf::default();
            let printer = stream_until_done(
                rx,
                session,
                done_rx,
                |pass| {
                    assert_eq!(pass, ReplayPass::Initial);
                    async { Ok(Vec::new()) }
                },
                out.clone(),
            )
            .await
            .unwrap();

            let mut stderr = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_millis(400),
                finish_json_stream(
                    printer,
                    session,
                    || bounded_replay(&store, session),
                    &mut stderr,
                ),
            )
            .await
            .expect("stalled final replay must be bounded by READ_TIMEOUT")
            .expect("stalled final replay must warn once and preserve run result");

            let warn = String::from_utf8(stderr).unwrap();
            assert_eq!(warn.lines().count(), 1, "expected 1 warning, got: {warn:?}");
            assert!(
                warn.contains("incomplete") && warn.contains("timeout"),
                "unexpected warning: {warn:?}"
            );
            assert_eq!(out.seqs(), vec![1, 2], "stdout stays pure JSONL");
        }

        // 2. Storage I/O failure (`SQLITE_CANTOPEN`) on cold unlinked store -> warn once and succeed.
        drop(store);
        std::fs::remove_file(&db).unwrap();
        let (tx, rx) = broadcast::channel::<Envelope>(8);
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let _ = tx.send(Envelope {
            seq: seq1,
            ts_millis: ts1,
            event: envelope(1, session).event,
        });
        let _ = done_tx.send(());
        let out = SharedBuf::default();
        let printer = stream_until_done(
            rx,
            session,
            done_rx,
            |pass| {
                assert_eq!(pass, ReplayPass::Initial);
                async { Ok(Vec::new()) }
            },
            out.clone(),
        )
        .await
        .unwrap();
        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            session,
            || bounded_replay(&cold_store, session),
            &mut stderr,
        )
        .await
        .expect("storage read fault at final replay must warn once and succeed");
        let warn = String::from_utf8(stderr).unwrap();
        assert_eq!(warn.lines().count(), 1, "expected 1 warning, got: {warn:?}");
        assert!(warn.contains("incomplete"));
        assert_eq!(out.seqs(), vec![1]);

        // 3. Non-storage errors (e.g. corrupt key / JSON decode) still propagate.
        let printer = JsonStreamPrinter::new(Vec::new());
        let mut stderr = Vec::new();
        let err = finish_json_stream(
            printer,
            session,
            || async { Err(StoreError::SessionKeyData("corrupt key".into())) },
            &mut stderr,
        )
        .await
        .unwrap_err();
        assert!(stderr.is_empty());
        assert!(err.to_string().contains("corrupt key"));
    }

    async fn lag_fault_then_finish(
        lag_op: &'static str,
        final_op: Option<&'static str>,
    ) -> (Vec<u64>, String) {
        let session = SessionId::new();
        let (bus, receiver) = broadcast::channel(1);
        let (done, completed) = oneshot::channel();
        let mut done = Some(done);
        let out = SharedBuf::default();
        bus.send(title(2, session)).unwrap();
        bus.send(title(3, session)).unwrap();
        let mut replays = 0;
        let printer = stream_until_done(
            receiver,
            session,
            completed,
            |pass| {
                replays += 1;
                std::future::ready(match pass {
                    ReplayPass::Initial => Ok(vec![title(1, session)]),
                    ReplayPass::Lag => {
                        done.take().unwrap().send(()).unwrap();
                        Err(StoreError::StorageTimeout {
                            operation: lag_op,
                            timeout: std::time::Duration::from_secs(5),
                        })
                    }
                })
            },
            out.clone(),
        )
        .await
        .unwrap();
        assert_eq!(replays, 2, "the lag replay must execute");
        let mut stderr = Vec::new();
        finish_json_stream(
            printer,
            session,
            || {
                std::future::ready(match final_op {
                    Some(op) => Err(StoreError::StorageTimeout {
                        operation: op,
                        timeout: std::time::Duration::from_secs(5),
                    }),
                    None => Ok(vec![
                        title(1, session),
                        title(2, session),
                        title(3, session),
                    ]),
                })
            },
            &mut stderr,
        )
        .await
        .unwrap();
        (out.seqs(), String::from_utf8(stderr).unwrap())
    }

    #[tokio::test]
    async fn lag_fault_then_final_replay_success_warns_once() {
        let (seqs, stderr) = lag_fault_then_finish("lag-replay", None).await;
        println!("lag timed out, final succeeded: stdout seqs={seqs:?}, stderr={stderr:?}");
        assert_eq!(seqs, vec![1, 3, 2]);
        assert_eq!(stderr.lines().count(), 1);
        assert!(stderr.contains("incomplete") && stderr.contains("lag-replay"));
    }

    #[tokio::test]
    async fn lag_and_final_replay_faults_warn_once() {
        let (seqs, stderr) = lag_fault_then_finish("lag-replay", Some("final-replay")).await;
        println!("lag and final timed out: stdout seqs={seqs:?}, stderr={stderr:?}");
        assert_eq!(seqs, vec![1, 3]);
        assert_eq!(stderr.lines().count(), 1);
        assert!(stderr.contains("incomplete") && stderr.contains("lag-replay"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn replay_for_pass_bounds_only_lag_with_read_timeout() {
        struct CleanDir(std::path::PathBuf);
        impl Drop for CleanDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct ShmLock(std::fs::File);
        impl ShmLock {
            fn acquire(db: &std::path::Path) -> Self {
                use std::os::unix::io::AsRawFd as _;
                let shm_path = format!("{}-shm", db.display());
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&shm_path)
                    .unwrap();
                let lock = libc::flock {
                    l_type: libc::F_WRLCK as libc::c_short,
                    l_whence: libc::SEEK_SET as libc::c_short,
                    l_start: 120,
                    l_len: 8,
                    l_pid: 0,
                };
                assert_eq!(
                    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) },
                    0
                );
                Self(file)
            }
        }
        impl Drop for ShmLock {
            fn drop(&mut self) {
                use std::os::unix::io::AsRawFd as _;
                let lock = libc::flock {
                    l_type: libc::F_UNLCK as libc::c_short,
                    l_whence: libc::SEEK_SET as libc::c_short,
                    l_start: 120,
                    l_len: 8,
                    l_pid: 0,
                };
                unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
            }
        }
        let _dir =
            CleanDir(std::env::temp_dir().join(format!("hya-exec-pass-{}", SessionId::new())));
        std::fs::create_dir_all(&_dir.0).unwrap();
        let db = _dir.0.join("hya.db");
        let store = SessionStore::connect_with_timeouts(
            db.to_str().unwrap(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &envelope(1, session).event)
            .await
            .unwrap();
        // Warm `read_pool` on healthy disk first.
        assert_eq!(store.replay(session).await.unwrap().len(), 1);

        // Stall 90ms (> 40ms read_timeout, << 5s write_timeout): Initial waits and succeeds.
        let lock = ShmLock::acquire(&db);
        let unlocker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(90));
            drop(lock);
        });
        let started = std::time::Instant::now();
        let initial = replay_for_pass(&store, session, ReplayPass::Initial)
            .await
            .expect("initial healthy replay must wait past 40ms read_timeout");
        let elapsed = started.elapsed();
        unlocker.join().unwrap();
        assert!(
            elapsed >= std::time::Duration::from_millis(70),
            "initial replay did not observe stall: elapsed={elapsed:?}"
        );
        assert_eq!(initial.len(), 1);

        // Held stall (> 40ms): Lag cuts off at read_timeout.
        let _held = ShmLock::acquire(&db);
        let lag = replay_for_pass(&store, session, ReplayPass::Lag)
            .await
            .expect_err("lag replay is fallback-guarded and cuts off at read_timeout");
        assert!(matches!(
            lag,
            StoreError::StorageTimeout {
                operation: "replay",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn lag_warning_and_stdout_flush_still_emit_when_final_replay_fails_logically() {
        let session = SessionId::new();
        let (tx, rx) = broadcast::channel(1);
        tx.send(envelope(1, session)).unwrap();
        tx.send(envelope(2, session)).unwrap();
        let (_done_tx, done_rx) = oneshot::channel();
        let out = SharedBuf::default();
        let stream_task = tokio::spawn({
            let out = out.clone();
            async move {
                stream_until_done(
                    rx,
                    session,
                    done_rx,
                    |pass| async move {
                        match pass {
                            ReplayPass::Initial => Ok(Vec::new()),
                            ReplayPass::Lag => Err(StoreError::StorageTimeout {
                                operation: "replay",
                                timeout: std::time::Duration::from_millis(50),
                            }),
                        }
                    },
                    std::io::BufWriter::new(out),
                )
                .await
            }
        });
        tx.send(title(3, session)).unwrap();
        drop(tx);
        let printer = stream_task.await.unwrap().unwrap();
        let mut stderr = Vec::new();
        let logical = StoreError::SessionKeyData("corrupt key in final replay".into());
        let res = finish_json_stream(
            printer,
            session,
            || async move { Err(logical) },
            &mut stderr,
        )
        .await;
        let err = res.expect_err("non-storage final error must still propagate");
        assert!(
            err.to_string().contains("corrupt key in final replay"),
            "{err}"
        );
        let stderr_text = String::from_utf8(stderr).unwrap();
        assert!(
            stderr_text.contains("warning: json replay unavailable"),
            "lag warning must still be emitted to stderr: {stderr_text:?}"
        );
        let stdout_text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(
            stdout_text.contains("title-3"),
            "live streamed stdout must still be flushed: {stdout_text:?}"
        );
    }

    #[tokio::test]
    async fn finish_json_stream_flushes_stdout_before_writing_warning() {
        struct OrderWriter {
            events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
            write_tag: &'static str,
            flush_tag: &'static str,
        }
        impl std::io::Write for OrderWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.events.lock().unwrap().push(self.write_tag);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.events.lock().unwrap().push(self.flush_tag);
                Ok(())
            }
        }

        let session = SessionId::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut printer = JsonStreamPrinter::new(std::io::BufWriter::new(OrderWriter {
            events: std::sync::Arc::clone(&events),
            write_tag: "stdout-write",
            flush_tag: "stdout-flush",
        }));
        printer.print(&title(1, session), session).unwrap();
        assert!(
            events.lock().unwrap().is_empty(),
            "buffered stdout must not flush before finish_json_stream"
        );

        let err_writer = OrderWriter {
            events: std::sync::Arc::clone(&events),
            write_tag: "stderr-write",
            flush_tag: "stderr-flush",
        };
        finish_json_stream(
            printer,
            session,
            || async {
                Err(StoreError::StorageTimeout {
                    operation: "replay",
                    timeout: std::time::Duration::from_secs(5),
                })
            },
            err_writer,
        )
        .await
        .unwrap();

        let recorded = events.lock().unwrap().clone();
        let flush_idx = recorded
            .iter()
            .position(|tag| *tag == "stdout-flush")
            .expect("stdout must flush");
        let warn_idx = recorded
            .iter()
            .position(|tag| *tag == "stderr-write")
            .expect("warning must write to stderr");
        assert!(
            flush_idx < warn_idx,
            "stdout flush must precede stderr warning write: {recorded:?}"
        );
    }
}
