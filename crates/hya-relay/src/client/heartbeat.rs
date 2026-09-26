//! Application heartbeats and dead-peer detection for one relay leg.
//!
//! [`with_heartbeat`] wraps a transport so that it
//!
//! - sends a probe (`heartbeat{seq, pong: false}`) whenever nothing was sent,
//!   or nothing was received, for one [`HeartbeatConfig::interval`], so
//!   idle-cutting intermediaries see traffic both ways (the proxy answers
//!   every probe) and a one-way upload still hears from the peer;
//! - answers the peer's probes with a pong and swallows pongs, so the user
//!   of the transport never sees heartbeat frames;
//! - fails the stream when no frame at all arrived for
//!   [`HeartbeatConfig::dead_peer_after`] (the proxy answers every probe, so
//!   silence means the path is dead).
//!
//! Probing and dead-peer detection stop once either direction of the leg has
//! ended (this side sent `close{}` or closed the sink, or the peer sent
//! `close{}`): after that the proxy may legitimately stop answering. A pong
//! that cannot be sent is dropped; it never ends the other direction.
//!
//! The wrapper runs a reader and a writer task, so heartbeats flow even when
//! the user is not polling. Frames accepted by the sink are delivered in
//! order; when the wrapper is dropped the writer still drains them, then
//! closes the inner transport cleanly if this side had ended its direction,
//! or drops it (an abort) otherwise.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::{CancellationToken, DropGuard, PollSender};

use crate::proto::{Chunk, Heartbeat, HostFrame, ProxyToHost, chunk, host_frame, proxy_to_host};
use crate::transport::{BoxedTransport, TransportError};

/// Default [`HeartbeatConfig::interval`].
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Frames the user may queue before the sink applies backpressure.
const OUTGOING_CAPACITY: usize = 16;
/// Frames read ahead of the user.
const INCOMING_CAPACITY: usize = 16;
/// Pongs waiting to be sent; more are dropped (the peer only needs one).
const PONG_CAPACITY: usize = 4;

/// Heartbeat timing of a relay leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatConfig {
    /// Send a probe when nothing was sent for this long. `Duration::ZERO`
    /// disables probes.
    pub interval: Duration,
    /// Fail the leg when nothing at all was received for this long.
    /// `Duration::ZERO` disables dead-peer detection.
    pub dead_peer_after: Duration,
}

impl HeartbeatConfig {
    /// Probe every `interval`; declare the peer dead after three intervals
    /// of silence.
    #[must_use]
    pub fn every(interval: Duration) -> Self {
        Self {
            interval,
            dead_peer_after: interval.saturating_mul(3),
        }
    }

    /// No probes and no dead-peer detection (probes from the peer are still
    /// answered).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            interval: Duration::ZERO,
            dead_peer_after: Duration::ZERO,
        }
    }

    /// Replace the dead-peer timeout.
    #[must_use]
    pub fn dead_peer_after(mut self, after: Duration) -> Self {
        self.dead_peer_after = after;
        self
    }
}

impl Default for HeartbeatConfig {
    /// A probe every 15 s, dead after 45 s of silence.
    fn default() -> Self {
        Self::every(DEFAULT_HEARTBEAT_INTERVAL)
    }
}

/// A message this side sends on a relay leg.
pub trait OutgoingFrame: Send + 'static {
    /// A liveness probe.
    fn probe(seq: u64) -> Self;
    /// The answer to a probe.
    fn pong(seq: u64) -> Self;
    /// Whether the frame ends this side's direction (`close{}`).
    fn ends_direction(&self) -> bool;
}

/// A message this side receives on a relay leg.
pub trait IncomingFrame: Send + 'static {
    /// The heartbeat this frame carries, if it is one.
    fn heartbeat(&self) -> Option<Heartbeat>;
    /// Whether the frame ends the peer's direction (`close{}`).
    fn ends_direction(&self) -> bool;
}

impl OutgoingFrame for Chunk {
    fn probe(seq: u64) -> Self {
        Chunk {
            frame: Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })),
        }
    }

    fn pong(seq: u64) -> Self {
        Chunk {
            frame: Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: true })),
        }
    }

    fn ends_direction(&self) -> bool {
        matches!(self.frame, Some(chunk::Frame::Close(_)))
    }
}

impl IncomingFrame for Chunk {
    fn heartbeat(&self) -> Option<Heartbeat> {
        match self.frame {
            Some(chunk::Frame::Heartbeat(heartbeat)) => Some(heartbeat),
            _ => None,
        }
    }

    fn ends_direction(&self) -> bool {
        matches!(self.frame, Some(chunk::Frame::Close(_)))
    }
}

impl OutgoingFrame for HostFrame {
    fn probe(seq: u64) -> Self {
        HostFrame {
            frame: Some(host_frame::Frame::Heartbeat(Heartbeat { seq, pong: false })),
        }
    }

    fn pong(seq: u64) -> Self {
        HostFrame {
            frame: Some(host_frame::Frame::Heartbeat(Heartbeat { seq, pong: true })),
        }
    }

    fn ends_direction(&self) -> bool {
        false
    }
}

impl IncomingFrame for ProxyToHost {
    fn heartbeat(&self) -> Option<Heartbeat> {
        match self.frame {
            Some(proxy_to_host::Frame::Heartbeat(heartbeat)) => Some(heartbeat),
            _ => None,
        }
    }

    fn ends_direction(&self) -> bool {
        false
    }
}

/// State both tasks and the user handle share.
#[derive(Default)]
struct Shared {
    /// This side ended its direction (sent `close{}` or closed the sink).
    local_closed: AtomicBool,
    /// The peer ended its direction (`close{}` received).
    remote_closed: AtomicBool,
    /// The user closed the sink (as opposed to dropping it).
    user_closed: AtomicBool,
    /// Why the writer stopped, reported by the next send.
    write_error: Mutex<Option<TransportError>>,
    /// When the reader last received a frame.
    last_received: Mutex<Option<Instant>>,
}

impl Shared {
    fn set_error(&self, error: TransportError) {
        if let Ok(mut slot) = self.write_error.lock() {
            slot.get_or_insert(error);
        }
    }

    fn take_error(&self) -> TransportError {
        self.write_error
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .unwrap_or(TransportError::Closed)
    }

    fn received(&self) {
        if let Ok(mut slot) = self.last_received.lock() {
            *slot = Some(Instant::now());
        }
    }

    fn last_received(&self) -> Option<Instant> {
        self.last_received.lock().ok().and_then(|slot| *slot)
    }

    fn quiet(&self) -> bool {
        self.local_closed.load(Ordering::SeqCst) || self.remote_closed.load(Ordering::SeqCst)
    }
}

/// Wrap `inner` with heartbeats and dead-peer detection (see the module
/// docs). Must be called inside a tokio runtime.
pub fn with_heartbeat<Tx, Rx>(
    inner: BoxedTransport<Tx, Rx>,
    config: HeartbeatConfig,
) -> BoxedTransport<Tx, Rx>
where
    Tx: OutgoingFrame,
    Rx: IncomingFrame,
{
    let (sink, stream) = inner.split();
    let shared = Arc::new(Shared::default());
    let (out_tx, out_rx) = mpsc::channel(OUTGOING_CAPACITY);
    let (in_tx, in_rx) = mpsc::channel(INCOMING_CAPACITY);
    let (pong_tx, pong_rx) = mpsc::channel(PONG_CAPACITY);
    // Cancelled when the user drops the transport: stop reading.
    let reader_stop = CancellationToken::new();
    // Cancelled on a dead peer: stop everything and drop the inner transport.
    let abort = CancellationToken::new();
    tokio::spawn(write_loop(
        sink,
        out_rx,
        pong_rx,
        shared.clone(),
        config,
        abort.clone(),
    ));
    tokio::spawn(read_loop(
        stream,
        in_tx,
        pong_tx,
        shared.clone(),
        config,
        reader_stop.clone(),
        abort,
    ));
    Box::pin(Heartbeating {
        out: PollSender::new(out_tx),
        incoming: in_rx,
        shared,
        _reader_stop: reader_stop.drop_guard(),
    })
}

type InnerSink<Tx, Rx> = futures::stream::SplitSink<BoxedTransport<Tx, Rx>, Tx>;
type InnerStream<Tx, Rx> = futures::stream::SplitStream<BoxedTransport<Tx, Rx>>;

async fn write_loop<Tx: OutgoingFrame, Rx>(
    mut sink: InnerSink<Tx, Rx>,
    mut out_rx: mpsc::Receiver<Tx>,
    mut pong_rx: mpsc::Receiver<u64>,
    shared: Arc<Shared>,
    config: HeartbeatConfig,
    abort: CancellationToken,
) {
    let started = Instant::now();
    let mut last_sent = started;
    let mut last_probe = started;
    let mut seq = 0u64;
    let mut pongs_open = true;
    loop {
        // Probe when either direction was quiet for an interval, at most
        // once per interval.
        let probe_at = (!config.interval.is_zero() && !shared.quiet())
            .then(|| probe_due(&shared, started, last_sent, last_probe, config.interval));
        tokio::select! {
            biased;
            () = abort.cancelled() => {
                shared.set_error(TransportError::Transport(
                    "the relay peer stopped responding".to_owned(),
                ));
                return;
            }
            pong = pong_rx.recv(), if pongs_open => match pong {
                Some(seq) => {
                    if !shared.local_closed.load(Ordering::SeqCst) {
                        // A failed pong is not fatal: a broken leg shows up
                        // on the next data frame or on the reader.
                        let _ = sink.send(Tx::pong(seq)).await;
                        last_sent = Instant::now();
                    }
                }
                None => pongs_open = false,
            },
            frame = out_rx.recv() => match frame {
                Some(frame) => {
                    if frame.ends_direction() {
                        shared.local_closed.store(true, Ordering::SeqCst);
                    }
                    if let Err(error) = sink.send(frame).await {
                        shared.set_error(error);
                        return;
                    }
                    last_sent = Instant::now();
                }
                None => {
                    if shared.user_closed.load(Ordering::SeqCst)
                        || shared.local_closed.load(Ordering::SeqCst)
                    {
                        shared.local_closed.store(true, Ordering::SeqCst);
                        let _ = sink.close().await;
                    }
                    // Otherwise the user dropped the transport mid-stream:
                    // dropping the inner transport aborts it.
                    return;
                }
            },
            () = sleep_until(probe_at.unwrap_or_else(Instant::now)), if probe_at.is_some() => {
                // Either side may have ended its direction, or the reader
                // heard from the peer, while we slept.
                if shared.quiet() || Instant::now() < probe_due(&shared, started, last_sent, last_probe, config.interval) {
                    continue;
                }
                seq = seq.wrapping_add(1);
                let _ = sink.send(Tx::probe(seq)).await;
                last_sent = Instant::now();
                last_probe = last_sent;
            }
        }
    }
}

/// When the next probe is due.
fn probe_due(
    shared: &Shared,
    started: Instant,
    last_sent: Instant,
    last_probe: Instant,
    interval: Duration,
) -> Instant {
    let last_heard = shared.last_received().unwrap_or(started);
    (last_sent.min(last_heard) + interval).max(last_probe + interval)
}

async fn read_loop<Tx, Rx: IncomingFrame>(
    mut stream: InnerStream<Tx, Rx>,
    in_tx: mpsc::Sender<Result<Rx, TransportError>>,
    pong_tx: mpsc::Sender<u64>,
    shared: Arc<Shared>,
    config: HeartbeatConfig,
    stop: CancellationToken,
    abort: CancellationToken,
) {
    loop {
        // Only read what the user can take (backpressure).
        let permit = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            permit = in_tx.reserve() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        // Silence is measured from the last frame, or from when the user
        // made room for one: time spent waiting for the user does not count.
        let dead_at = (!config.dead_peer_after.is_zero() && !shared.quiet())
            .then(|| Instant::now() + config.dead_peer_after);
        let next = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = sleep_until(dead_at.unwrap_or_else(Instant::now)), if dead_at.is_some() => {
                permit.send(Err(TransportError::Transport(format!(
                    "no frame from the relay for {:?}; the connection is presumed dead",
                    config.dead_peer_after
                ))));
                abort.cancel();
                return;
            }
            next = stream.next() => next,
        };
        shared.received();
        match next {
            None => return,
            Some(Err(error)) => {
                permit.send(Err(error));
                return;
            }
            Some(Ok(frame)) => match frame.heartbeat() {
                Some(Heartbeat { seq, pong: false }) => {
                    let _ = pong_tx.try_send(seq);
                }
                Some(Heartbeat { pong: true, .. }) => {}
                None => {
                    if frame.ends_direction() {
                        shared.remote_closed.store(true, Ordering::SeqCst);
                    }
                    permit.send(Ok(frame));
                }
            },
        }
    }
}

/// The user's handle: a sink into the writer task and a stream from the
/// reader task.
struct Heartbeating<Tx: Send + 'static, Rx> {
    out: PollSender<Tx>,
    incoming: mpsc::Receiver<Result<Rx, TransportError>>,
    shared: Arc<Shared>,
    _reader_stop: DropGuard,
}

impl<Tx: Send + 'static, Rx> Sink<Tx> for Heartbeating<Tx, Rx> {
    type Error = TransportError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.out.poll_reserve(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_)) => Poll::Ready(Err(self.shared.take_error())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Tx) -> Result<(), Self::Error> {
        if self.out.send_item(item).is_err() {
            return Err(self.shared.take_error());
        }
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.shared.user_closed.store(true, Ordering::SeqCst);
        self.out.close();
        Poll::Ready(Ok(()))
    }
}

impl<Tx: Send + 'static, Rx> Stream for Heartbeating<Tx, Rx> {
    type Item = Result<Rx, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx)
    }
}
