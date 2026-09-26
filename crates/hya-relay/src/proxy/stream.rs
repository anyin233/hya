//! Data streams: `Open`, `Accept`, and the splice between them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

use super::limits::TokenBucket;
use super::{EarlyData, Inner, PeerInfo, Room, shutting_down};
use crate::link::RoomId;
use crate::proto::{Chunk, Close, Heartbeat, Opened, RelayError, RelayErrorCode, chunk};
use crate::transport::{ChunkTransport, TransportError};

/// How long the proxy tries to deliver a final error frame or close.
const ERROR_SEND_TIMEOUT: Duration = Duration::from_secs(5);

fn chunk(frame: chunk::Frame) -> Chunk {
    Chunk { frame: Some(frame) }
}

fn close_chunk() -> Chunk {
    chunk(chunk::Frame::Close(Close {}))
}

fn pong(seq: u64) -> Chunk {
    chunk(chunk::Frame::Heartbeat(Heartbeat { seq, pong: true }))
}

fn error(code: RelayErrorCode, message: &str) -> RelayError {
    RelayError::new(code, message)
}

fn oversized(inner: &Inner) -> RelayError {
    RelayError::new(
        RelayErrorCode::ResourceExhausted,
        format!(
            "chunk data exceeds the {} byte limit",
            inner.limits.max_chunk_data
        ),
    )
}

/// Send a final error frame and close the sending direction (best effort).
async fn fail(transport: &mut ChunkTransport, relay_error: RelayError) {
    let _ = timeout(ERROR_SEND_TIMEOUT, async {
        transport
            .send(chunk(chunk::Frame::Error(relay_error)))
            .await?;
        transport.close().await
    })
    .await;
}

/// Read the first frame of a data stream within the handshake timeout.
///
/// `Err(None)` means the peer went away (nothing to report).
async fn first_frame(
    inner: &Inner,
    transport: &mut ChunkTransport,
) -> Result<chunk::Frame, Option<RelayError>> {
    let next = tokio::select! {
        biased;
        () = inner.shutdown.cancelled() => return Err(Some(shutting_down())),
        next = timeout(inner.limits.handshake_timeout, transport.next()) => next,
    };
    match next {
        Err(_) => Err(Some(error(
            RelayErrorCode::DeadlineExceeded,
            "no handshake frame in time",
        ))),
        Ok(None | Some(Err(_))) => Err(None),
        Ok(Some(Ok(Chunk { frame: Some(frame) }))) => Ok(frame),
        Ok(Some(Ok(Chunk { frame: None }))) => Err(Some(error(
            RelayErrorCode::InvalidArgument,
            "empty handshake frame",
        ))),
    }
}

pub(crate) async fn run_accept(inner: Arc<Inner>, mut transport: ChunkTransport, _peer: PeerInfo) {
    if inner.shutdown.is_cancelled() {
        return fail(&mut transport, shutting_down()).await;
    }
    let stream_id = match first_frame(&inner, &mut transport).await {
        Ok(chunk::Frame::Accept(accept)) => accept.stream_id,
        Ok(_) => {
            let relay_error = error(
                RelayErrorCode::InvalidArgument,
                "the first frame of an Accept stream must be `accept`",
            );
            return fail(&mut transport, relay_error).await;
        }
        Err(Some(relay_error)) => return fail(&mut transport, relay_error).await,
        Err(None) => return,
    };
    if let Err(mut transport) = inner.deliver_accept(&stream_id, transport) {
        let relay_error = error(
            RelayErrorCode::NotFound,
            "unknown, expired, or already accepted stream id",
        );
        fail(&mut transport, relay_error).await;
    }
}

pub(crate) async fn run_open(inner: Arc<Inner>, mut opener: ChunkTransport, peer: PeerInfo) {
    if inner.shutdown.is_cancelled() {
        return fail(&mut opener, shutting_down()).await;
    }
    let (room_id, open_token) = match first_frame(&inner, &mut opener).await {
        Ok(chunk::Frame::Open(open)) => (open.room_id, open.open_token),
        Ok(_) => {
            let relay_error = error(
                RelayErrorCode::InvalidArgument,
                "the first frame of an Open stream must be `open`",
            );
            return fail(&mut opener, relay_error).await;
        }
        Err(Some(relay_error)) => return fail(&mut opener, relay_error).await,
        Err(None) => return,
    };
    if RoomId::parse(&room_id).is_err() {
        let relay_error = error(RelayErrorCode::InvalidArgument, "malformed room id");
        return fail(&mut opener, relay_error).await;
    }
    let (slot, mut arrival) = match inner.admit_stream(&room_id, &open_token, &peer) {
        Ok(admitted) => admitted,
        Err(relay_error) => return fail(&mut opener, relay_error).await,
    };
    let room = slot.room.clone();

    // Wait for the host's Accept, buffering a bounded amount of early data.
    let limit = inner.limits.early_data_limit;
    let mut early: Vec<Chunk> = Vec::new();
    let mut early_bytes = 0usize;
    let mut early_data = inner.early_data();
    let mut opener_closed = false;
    let accept_deadline = sleep(inner.limits.accept_timeout);
    tokio::pin!(accept_deadline);
    let mut deadline_passed = false;
    let host_leg = loop {
        tokio::select! {
            biased;
            () = room.token.cancelled() => {
                return fail(&mut opener, inner.room_closed_error()).await;
            }
            leg = &mut arrival => match leg {
                Ok(leg) => break leg,
                Err(_) => return fail(&mut opener, inner.room_closed_error()).await,
            },
            () = &mut accept_deadline, if !deadline_passed => {
                deadline_passed = true;
                if inner.expire_pending(&slot.stream_id) {
                    let relay_error = error(
                        RelayErrorCode::Unavailable,
                        "the host did not accept the stream in time",
                    );
                    return fail(&mut opener, relay_error).await;
                }
                // An Accept claimed it just now; its leg is on the way.
            }
            next = opener.next(),
                if !opener_closed && early_bytes < limit && inner.early_data_available() =>
            match next {
                None => {
                    opener_closed = true;
                    early.push(close_chunk());
                }
                Some(Err(_)) => return,
                Some(Ok(Chunk { frame })) => match frame {
                    Some(chunk::Frame::Data(bytes)) => {
                        if bytes.len() > inner.limits.max_chunk_data {
                            return fail(&mut opener, oversized(&inner)).await;
                        }
                        early_bytes += bytes.len();
                        early_data.add(bytes.len());
                        early.push(chunk(chunk::Frame::Data(bytes)));
                    }
                    Some(chunk::Frame::Close(_)) => {
                        opener_closed = true;
                        early.push(close_chunk());
                    }
                    Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })) => {
                        if timeout(inner.limits.idle_timeout, opener.send(pong(seq))).await.is_err() {
                            return;
                        }
                    }
                    Some(chunk::Frame::Heartbeat(_)) => {}
                    Some(chunk::Frame::Error(_)) => return,
                    _ => {
                        let relay_error = error(
                            RelayErrorCode::InvalidArgument,
                            "unexpected frame on an Open stream",
                        );
                        return fail(&mut opener, relay_error).await;
                    }
                },
            },
        }
    };

    let opened = chunk(chunk::Frame::Opened(Opened {}));
    if !matches!(
        timeout(inner.limits.idle_timeout, opener.send(opened)).await,
        Ok(Ok(()))
    ) {
        let mut host_leg = host_leg;
        let relay_error = error(RelayErrorCode::Unavailable, "the opener went away");
        return fail(&mut host_leg, relay_error).await;
    }
    splice(
        &inner,
        &room,
        opener,
        host_leg,
        (early, early_data),
        opener_closed,
    )
    .await;
    drop(slot);
}

/// One leg's sending half, shared by both pumps (data one way, pongs the
/// other), plus whether its direction was closed.
struct SharedSink {
    sink: Mutex<SplitSink<ChunkTransport, Chunk>>,
    closed: AtomicBool,
}

impl SharedSink {
    fn new(sink: SplitSink<ChunkTransport, Chunk>) -> Self {
        Self {
            sink: Mutex::new(sink),
            closed: AtomicBool::new(false),
        }
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = timeout(ERROR_SEND_TIMEOUT, async {
            self.sink.lock().await.close().await
        })
        .await;
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// How a pump (one direction of a splice) failed.
enum Fault {
    /// Report to both legs.
    Both(RelayError),
    /// Report to the destination leg only (the source is gone or failed).
    ToDestination(RelayError),
    /// Report to the source leg only (the destination is gone).
    ToSource(RelayError),
}

/// Send one frame on a shared sink within the idle timeout.
async fn send_shared(inner: &Inner, sink: &SharedSink, frame: Chunk) -> Result<(), Option<()>> {
    match timeout(inner.limits.idle_timeout, async {
        sink.sink.lock().await.send(frame).await
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        // Gone.
        Ok(Err(_)) => Err(None),
        // Stalled.
        Err(_) => Err(Some(())),
    }
}

fn stalled() -> Fault {
    Fault::Both(error(
        RelayErrorCode::DeadlineExceeded,
        "a stream peer stopped reading",
    ))
}

fn gone(sent_to_destination: bool) -> Fault {
    let relay_error = error(RelayErrorCode::Unavailable, "the other side went away");
    if sent_to_destination {
        Fault::ToSource(relay_error)
    } else {
        Fault::ToDestination(relay_error)
    }
}

/// Forward frames from `source` to `destination` until the source closes
/// its direction (`Ok`) or something fails.
async fn pump(
    inner: &Inner,
    mut source: SplitStream<ChunkTransport>,
    source_sink: &SharedSink,
    destination: &SharedSink,
) -> Result<(), Fault> {
    let limits = &inner.limits;
    let mut bucket = TokenBucket::new(
        limits.stream_rate_bytes_per_sec,
        limits.stream_rate_burst_bytes,
    );
    loop {
        let next = match timeout(limits.idle_timeout, source.next()).await {
            Ok(next) => next,
            Err(_) => {
                return Err(Fault::Both(error(
                    RelayErrorCode::DeadlineExceeded,
                    "stream idle",
                )));
            }
        };
        let frame = match next {
            // A clean end without `close` still ends the direction.
            None => None,
            Some(Err(TransportError::Status { code, message })) => {
                return Err(Fault::ToDestination(RelayError::new(code, message)));
            }
            Some(Err(_)) => return Err(gone(false)),
            Some(Ok(Chunk { frame })) => frame,
        };
        match frame {
            Some(chunk::Frame::Data(bytes)) => {
                if bytes.len() > limits.max_chunk_data {
                    return Err(Fault::Both(oversized(inner)));
                }
                if let Some(bucket) = bucket.as_mut() {
                    let wait = bucket.charge(bytes.len());
                    if !wait.is_zero() {
                        sleep(wait).await;
                    }
                }
                forward(inner, destination, chunk(chunk::Frame::Data(bytes))).await?;
            }
            None | Some(chunk::Frame::Close(_)) => {
                forward(inner, destination, close_chunk()).await?;
                destination.close().await;
                return Ok(());
            }
            Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })) => {
                // After this leg's sending direction is closed a probe can no
                // longer be answered; drop it instead of treating the failed
                // pong as the source going away. A pong that fails to send
                // is not fatal either: a leg that is really gone shows up on
                // its stream.
                if !source_sink.is_closed()
                    && let Err(Some(())) = send_shared(inner, source_sink, pong(seq)).await
                {
                    return Err(stalled());
                }
            }
            Some(chunk::Frame::Heartbeat(_)) => {}
            Some(chunk::Frame::Error(relay_error)) => {
                return Err(Fault::ToDestination(relay_error));
            }
            Some(chunk::Frame::Open(_) | chunk::Frame::Accept(_) | chunk::Frame::Opened(_)) => {
                return Err(Fault::Both(error(
                    RelayErrorCode::InvalidArgument,
                    "handshake frame on a spliced stream",
                )));
            }
        }
    }
}

async fn forward(inner: &Inner, destination: &SharedSink, frame: Chunk) -> Result<(), Fault> {
    match send_shared(inner, destination, frame).await {
        Ok(()) => Ok(()),
        Err(None) => Err(gone(true)),
        Err(Some(())) => Err(stalled()),
    }
}

/// Relay both directions between the opener and the host leg.
async fn splice(
    inner: &Inner,
    room: &Room,
    opener: ChunkTransport,
    host_leg: ChunkTransport,
    (early, early_data): (Vec<Chunk>, EarlyData),
    opener_closed: bool,
) {
    let (opener_sink, opener_stream) = opener.split();
    let (host_sink, host_stream) = host_leg.split();
    let opener_sink = SharedSink::new(opener_sink);
    let host_sink = SharedSink::new(host_sink);

    // Early data first, then the live pumps.
    let mut early_fault = None;
    for frame in early {
        let is_close = matches!(frame.frame, Some(chunk::Frame::Close(_)));
        if let Err(fault) = forward(inner, &host_sink, frame).await {
            early_fault = Some(fault);
            break;
        }
        if is_close {
            host_sink.close().await;
        }
    }
    // Delivered (or failed): no longer buffered.
    drop(early_data);

    // `Fault` from the opener→host pump is reported relative to that
    // direction; map it onto (to_opener, to_host).
    let mut outcome: Option<(Option<RelayError>, Option<RelayError>)> =
        early_fault.map(from_opener);
    if outcome.is_none() {
        let mut up = Box::pin(pump(inner, opener_stream, &opener_sink, &host_sink));
        let mut down = Box::pin(pump(inner, host_stream, &host_sink, &opener_sink));
        let mut up_done = opener_closed;
        let mut down_done = false;
        while outcome.is_none() && !(up_done && down_done) {
            tokio::select! {
                biased;
                () = room.token.cancelled() => {
                    let relay_error = inner.room_closed_error();
                    outcome = Some((Some(relay_error.clone()), Some(relay_error)));
                }
                result = &mut up, if !up_done => match result {
                    Ok(()) => up_done = true,
                    Err(fault) => outcome = Some(from_opener(fault)),
                },
                result = &mut down, if !down_done => match result {
                    Ok(()) => down_done = true,
                    Err(fault) => {
                        let (to_host, to_opener) = from_opener(fault);
                        outcome = Some((to_opener, to_host));
                    }
                },
            }
        }
        // Release any sink lock a stalled pump holds.
        drop(up);
        drop(down);
    }

    let (to_opener, to_host) = outcome.unwrap_or((None, None));
    finish(&opener_sink, to_opener).await;
    finish(&host_sink, to_host).await;
}

/// Map a fault of the opener→host direction to `(to_opener, to_host)`.
fn from_opener(fault: Fault) -> (Option<RelayError>, Option<RelayError>) {
    match fault {
        Fault::Both(relay_error) => (Some(relay_error.clone()), Some(relay_error)),
        Fault::ToDestination(relay_error) => (None, Some(relay_error)),
        Fault::ToSource(relay_error) => (Some(relay_error), None),
    }
}

/// Deliver a final error (if any) and close a leg's sending direction.
async fn finish(sink: &SharedSink, relay_error: Option<RelayError>) {
    sink.closed.store(true, Ordering::SeqCst);
    let _ = timeout(ERROR_SEND_TIMEOUT, async {
        let mut sink = sink.sink.lock().await;
        if let Some(relay_error) = relay_error {
            sink.send(chunk(chunk::Frame::Error(relay_error))).await?;
        }
        sink.close().await
    })
    .await;
}
