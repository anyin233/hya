//! Live JSONL streaming for headless `exec --json`.
//!
//! Events are persisted per-envelope by the engine regardless; this module
//! streams the same durable envelopes to stdout as they are broadcast, so an
//! abnormally terminated run still leaves a usable partial trajectory on
//! stdout (the run2 lesson). Envelopes are filtered to the exec session and
//! to durable seqs (`seq == 0` frames are live-only and never persisted), and
//! a final replay delta guarantees the stdout set is exactly the durable log
//! even when the bus lagged mid-turn.

use hya_proto::{Envelope, SessionId};
use tokio::sync::{broadcast, oneshot};

/// Whether `envelope` belongs on the exec JSONL stream for `session` beyond
/// `watermark`: same session, durable seq, strictly ascending.
#[must_use]
pub(crate) fn eligible(envelope: &Envelope, session: SessionId, watermark: u64) -> bool {
    envelope.event.session() == Some(session) && envelope.seq.0 > watermark
}

/// Incremental JSONL writer with a durable-seq watermark.
pub(crate) struct JsonStreamPrinter<W: std::io::Write> {
    out: W,
    watermark: u64,
}

impl<W: std::io::Write> JsonStreamPrinter<W> {
    /// Start at seq 0 (nothing printed yet; live-only frames stay excluded
    /// because their seq is also 0).
    pub(crate) fn new(out: W) -> Self {
        Self { out, watermark: 0 }
    }

    /// Highest durable seq printed so far.
    #[cfg(test)]
    pub(crate) fn watermark(&self) -> u64 {
        self.watermark
    }

    /// Print `envelope` when eligible; advance the watermark. Returns whether
    /// the envelope was printed.
    pub(crate) fn print(
        &mut self,
        envelope: &Envelope,
        session: SessionId,
    ) -> std::io::Result<bool> {
        if !eligible(envelope, session, self.watermark) {
            return Ok(false);
        }
        let line =
            serde_json::to_string(envelope).map_err(|e| std::io::Error::other(e.to_string()))?;
        writeln!(self.out, "{line}")?;
        self.watermark = envelope.seq.0;
        Ok(true)
    }

    /// Flush the underlying writer.
    pub(crate) fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Drain the engine bus until `done` fires, printing eligible envelopes as
/// they arrive; on Lagged, resync through `replay` (durable log is the
/// authority). Returns the printer (with watermark) for a final tail flush.
pub(crate) async fn stream_until_done<W, F, Fut>(
    mut rx: broadcast::Receiver<Envelope>,
    session: SessionId,
    mut done: oneshot::Receiver<()>,
    mut replay: F,
    out: W,
) -> std::io::Result<JsonStreamPrinter<W>>
where
    W: std::io::Write,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<Envelope>>>,
{
    let mut printer = JsonStreamPrinter::new(out);
    // Catch-up pass: envelopes persisted before this subscription existed
    // (notably `session_created`) print first, keeping stdout ordered.
    match replay().await {
        Ok(envelopes) => {
            for envelope in &envelopes {
                printer.print(envelope, session)?;
            }
        }
        Err(_) => {
            // Fall through to live streaming; the final tail flush after the
            // turn is the completeness authority.
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
                    match replay().await {
                        Ok(envelopes) => {
                            for envelope in &envelopes {
                                printer.print(envelope, session)?;
                            }
                        }
                        Err(_) => {
                            // The durable store is unreachable; keep streaming
                            // live frames rather than dying — the final tail
                            // flush after the turn surfaces the failure.
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
            },
        }
    }

    #[test]
    fn printer_filters_session_durable_seq_and_advances_watermark() {
        let session = SessionId::new();
        let other = SessionId::new();
        let mut printer = JsonStreamPrinter::new(Vec::new());

        assert!(!printer.print(&envelope(0, session), session).unwrap());
        assert!(!printer.print(&envelope(1, other), session).unwrap());
        assert!(printer.print(&envelope(1, session), session).unwrap());
        assert_eq!(printer.watermark(), 1);
        // Ascending-only: replays of old seqs are dropped.
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
            || {
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
            || {
                calls += 1;
                let envelopes = if calls == 1 {
                    catch_up.clone()
                } else {
                    Vec::new()
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

        let durable = vec![
            envelope(1, session),
            envelope(2, session),
            envelope(3, session),
        ];
        for envelope in &durable {
            let _ = tx.send(envelope.clone());
        }
        let _ = done_tx.send(());

        let replay_calls = std::cell::Cell::new(0);
        let durable_for_closure = durable.clone();
        let printer = stream_until_done(
            rx,
            session,
            done_rx,
            || {
                replay_calls.set(replay_calls.get() + 1);
                let envelopes = durable_for_closure.clone();
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
        assert_eq!(seqs, vec![1, 2, 3], "replay recovers everything missed");
        assert!(replay_calls.get() >= 1);
    }
}
