//! The end-to-end encrypted tunnel carried inside a relay data stream.
//!
//! [`NoiseStream`] runs `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s` over a
//! [`ChunkTransport`](crate::transport::ChunkTransport) (any
//! [`RelayTransport<Chunk, Chunk>`]) and then exposes the decrypted byte
//! stream as tokio [`AsyncRead`] + [`AsyncWrite`], so `hyper` (host) and a
//! TCP splice (client bridge) can run over it unchanged.
//!
//! - **Roles.** The client is the initiator: it knows the backend's static
//!   X25519 public key and the PSK from the relay link. The backend is the
//!   responder: it holds the static keypair and the PSK.
//! - **Prologue.** [`PROLOGUE_CONTEXT`] followed by the room id, so a session
//!   is bound to the room it was opened for.
//! - **Handshake.** Two messages, each one `Chunk.data`: the initiator sends
//!   `-> psk, e, es` as soon as it is constructed; the responder answers
//!   `<- e, ee`. A wrong PSK, server key, or room fails on the responder at
//!   the first message, before any application byte exists.
//! - **Records.** Every Noise transport message is exactly one `Chunk.data`
//!   (the protobuf field is the length prefix). Plaintext is split into
//!   records of at most [`TunnelConfig::max_record_plaintext`] bytes
//!   (default [`DEFAULT_RECORD_PLAINTEXT`], never above
//!   [`MAX_RECORD_PLAINTEXT`]).
//! - **Close.** Shutting down the write side sends an encrypted empty record
//!   (the authenticated close record) and then `Chunk.close{}`. The peer's
//!   read side then reports EOF while its write side stays usable
//!   (half-close). A `Chunk.close` or end of stream *without* the close
//!   record is a truncation and fails with [`TunnelError::Truncated`].
//! - **Failures.** A record that fails authentication, arrives out of order,
//!   or is malformed is fatal: no plaintext from it is delivered, and every
//!   later read and write fails. A peer `Chunk.error` (or transport status)
//!   surfaces as [`TunnelError::Relay`] carrying the relay code.
//! - **Liveness frames.** `Chunk.heartbeat` frames are invisible to the byte
//!   stream; a probe (`pong = false`) is answered with a pong echoing its
//!   `seq`. Any other frame kind (for example a proxy acknowledgement) is
//!   ignored.
//!
//! The tunnel has no built-in timeouts; callers wrap the handshake (and idle
//! reads, if they want them) in `tokio::time::timeout`.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use futures::task::{ArcWake, waker};
use futures::{FutureExt, SinkExt, StreamExt};
use snow::params::NoiseParams;
use snow::{Builder, TransportState};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::keys::{Psk, StaticKeypair};
use crate::link::{RelayLink, RoomId};
use crate::proto::{Chunk, Close, Heartbeat, RelayErrorCode, chunk};
use crate::transport::{RelayTransport, TransportError};

/// The Noise protocol name.
pub const NOISE_PATTERN: &str = "Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s";

/// Domain-separation prefix of the Noise prologue; the room id follows it.
pub const PROLOGUE_CONTEXT: &[u8] = b"hya.relay.v1/noise\0";

/// Largest Noise message (handshake or record ciphertext), in bytes.
pub const MAX_NOISE_MESSAGE: usize = 65535;

/// ChaChaPoly authentication tag length, in bytes.
pub const TAG_LEN: usize = 16;

/// Largest plaintext a single record can carry.
pub const MAX_RECORD_PLAINTEXT: usize = MAX_NOISE_MESSAGE - TAG_LEN;

/// Default plaintext bytes per record (16 KiB).
pub const DEFAULT_RECORD_PLAINTEXT: usize = 16 * 1024;

/// The Noise prologue for a session in `room_id`.
#[must_use]
pub fn prologue(room_id: &RoomId) -> Vec<u8> {
    let mut out = Vec::with_capacity(PROLOGUE_CONTEXT.len() + room_id.as_str().len());
    out.extend_from_slice(PROLOGUE_CONTEXT);
    out.extend_from_slice(room_id.as_str().as_bytes());
    out
}

/// Tunnel tuning knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TunnelConfig {
    max_record_plaintext: usize,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        TunnelConfig {
            max_record_plaintext: DEFAULT_RECORD_PLAINTEXT,
        }
    }
}

impl TunnelConfig {
    /// Set the largest plaintext per record.
    ///
    /// # Errors
    /// [`TunnelError::InvalidConfig`] unless `1 <= bytes <=
    /// MAX_RECORD_PLAINTEXT`.
    pub fn with_max_record_plaintext(self, bytes: usize) -> Result<Self, TunnelError> {
        if bytes == 0 || bytes > MAX_RECORD_PLAINTEXT {
            return Err(TunnelError::InvalidConfig(format!(
                "record size {bytes} must be between 1 and {MAX_RECORD_PLAINTEXT}"
            )));
        }
        Ok(TunnelConfig {
            max_record_plaintext: bytes,
        })
    }

    /// The largest plaintext per record.
    #[must_use]
    pub fn max_record_plaintext(&self) -> usize {
        self.max_record_plaintext
    }
}

/// Tunnel failure. Data-path failures reach callers as [`io::Error`]s whose
/// inner error (`io::Error::get_ref`) is a `TunnelError`. Messages never
/// contain key material or payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TunnelError {
    /// A [`TunnelConfig`] value is out of range.
    #[error("invalid tunnel config: {0}")]
    InvalidConfig(String),
    /// The Noise handshake failed: wrong PSK, server key, or room, or the
    /// peer closed the stream before completing it.
    #[error("noise handshake failed: {0}")]
    Handshake(String),
    /// The relay transport failed.
    #[error("relay transport failed: {0}")]
    Transport(String),
    /// The proxy or peer ended the stream with a relay error.
    #[error("relay error {code:?}: {message}")]
    Relay {
        /// Failure class.
        code: RelayErrorCode,
        /// Human-readable detail.
        message: String,
    },
    /// A record failed authentication (tampered, replayed, or out of order).
    #[error("tunnel record failed authentication")]
    Decrypt,
    /// The peer's direction ended without the authenticated close record.
    #[error("tunnel ended without a close record (truncated)")]
    Truncated,
    /// The peer violated the tunnel framing.
    #[error("tunnel protocol violation: {0}")]
    Protocol(String),
}

impl From<TransportError> for TunnelError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Status { code, message } => TunnelError::Relay { code, message },
            other => TunnelError::Transport(other.to_string()),
        }
    }
}

impl TunnelError {
    fn io_kind(&self) -> io::ErrorKind {
        match self {
            TunnelError::InvalidConfig(_) => io::ErrorKind::InvalidInput,
            TunnelError::Handshake(_) => io::ErrorKind::PermissionDenied,
            TunnelError::Transport(_) => io::ErrorKind::ConnectionAborted,
            TunnelError::Relay { code, .. } => match code {
                RelayErrorCode::NotFound => io::ErrorKind::NotFound,
                RelayErrorCode::PermissionDenied | RelayErrorCode::Unauthenticated => {
                    io::ErrorKind::PermissionDenied
                }
                RelayErrorCode::DeadlineExceeded => io::ErrorKind::TimedOut,
                RelayErrorCode::Cancelled | RelayErrorCode::Unavailable => {
                    io::ErrorKind::ConnectionReset
                }
                _ => io::ErrorKind::Other,
            },
            TunnelError::Decrypt | TunnelError::Protocol(_) => io::ErrorKind::InvalidData,
            TunnelError::Truncated => io::ErrorKind::UnexpectedEof,
        }
    }
}

impl From<TunnelError> for io::Error {
    fn from(error: TunnelError) -> Self {
        io::Error::new(error.io_kind(), error)
    }
}

fn setup_error(error: snow::Error) -> TunnelError {
    TunnelError::Handshake(format!("noise setup failed: {error}"))
}

fn frame(frame: chunk::Frame) -> Chunk {
    Chunk { frame: Some(frame) }
}

/// Receive the next handshake message, answering heartbeat probes and
/// skipping frames the tunnel does not use.
async fn next_handshake_message<T>(transport: &mut T) -> Result<Vec<u8>, TunnelError>
where
    T: RelayTransport<Chunk, Chunk> + Unpin,
{
    loop {
        let item = transport.next().await.ok_or_else(|| {
            TunnelError::Handshake("peer ended the stream during the handshake".into())
        })?;
        match item?.frame {
            Some(chunk::Frame::Data(bytes)) => {
                if bytes.len() > MAX_NOISE_MESSAGE {
                    return Err(TunnelError::Handshake("oversized handshake message".into()));
                }
                return Ok(bytes);
            }
            Some(chunk::Frame::Close(_)) => {
                return Err(TunnelError::Handshake(
                    "peer closed the stream during the handshake".into(),
                ));
            }
            Some(chunk::Frame::Error(error)) => {
                return Err(TransportError::from(error).into());
            }
            Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })) => {
                transport
                    .send(frame(chunk::Frame::Heartbeat(Heartbeat {
                        seq,
                        pong: true,
                    })))
                    .await?;
            }
            _ => {}
        }
    }
}

fn noise_params() -> Result<NoiseParams, TunnelError> {
    NOISE_PATTERN.parse().map_err(setup_error)
}

/// Which half of the stream is polling the shared sink.
#[derive(Clone, Copy)]
enum Side {
    Read,
    Write,
}

/// Both halves (reader answering probes, writer sending records) poll the
/// same sink, possibly from different tasks (`tokio::io::split`). A sink
/// keeps one waker, so the sink is always polled with this combined waker,
/// which wakes whichever halves are waiting on it.
#[derive(Default)]
struct SinkWakers {
    read: Mutex<Option<Waker>>,
    write: Mutex<Option<Waker>>,
}

impl SinkWakers {
    fn register(&self, side: Side, waker: &Waker) {
        let slot = match side {
            Side::Read => &self.read,
            Side::Write => &self.write,
        };
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if !slot
            .as_ref()
            .is_some_and(|current| current.will_wake(waker))
        {
            *slot = Some(waker.clone());
        }
    }
}

impl ArcWake for SinkWakers {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        for slot in [&arc_self.read, &arc_self.write] {
            let waiting = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(waker) = waiting {
                waker.wake();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteState {
    Open,
    SendCloseFrame,
    Flushing,
    Closed,
}

/// An established Noise tunnel over a relay data stream, usable as a tokio
/// byte stream. See the [module docs](self) for the wire contract.
///
/// Reads and writes may be driven from one task or split across two
/// (`tokio::io::split`).
pub struct NoiseStream<T> {
    transport: T,
    noise: TransportState,
    max_record_plaintext: usize,
    /// Plaintext of the last authenticated record not yet handed out.
    read_buf: Vec<u8>,
    read_pos: usize,
    /// The peer's authenticated close record arrived.
    read_eof: bool,
    write_state: WriteState,
    /// A heartbeat probe seq still to be answered.
    pending_pong: Option<u64>,
    /// A pong was queued on the sink but not yet flushed.
    pong_unflushed: bool,
    /// Fatal failure: every later read and write fails with it.
    fatal: Option<TunnelError>,
    /// Sending failed (for example the peer went away); reads may still
    /// drain frames already received.
    write_error: Option<TunnelError>,
    wakers: Arc<SinkWakers>,
    sink_waker: Waker,
}

impl<T> fmt::Debug for NoiseStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoiseStream")
            .field("max_record_plaintext", &self.max_record_plaintext)
            .field("read_eof", &self.read_eof)
            .field("write_state", &self.write_state)
            .field("fatal", &self.fatal)
            .finish_non_exhaustive()
    }
}

impl<T> NoiseStream<T>
where
    T: RelayTransport<Chunk, Chunk> + Unpin,
{
    /// Client side: run the initiator handshake to the backend whose static
    /// public key is `server_public`, in room `room_id`.
    ///
    /// The first handshake message is sent immediately; the call completes
    /// once the responder's answer authenticates the backend.
    ///
    /// # Errors
    /// [`TunnelError::Handshake`] when the backend rejects or cannot prove
    /// its key (wrong key, PSK, or room close the stream), or a transport /
    /// relay error.
    pub async fn initiate(
        mut transport: T,
        room_id: &RoomId,
        server_public: &[u8; 32],
        psk: &Psk,
        config: TunnelConfig,
    ) -> Result<Self, TunnelError> {
        let prologue = prologue(room_id);
        let mut handshake = Builder::new(noise_params()?)
            .prologue(&prologue)
            .and_then(|b| b.remote_public_key(server_public))
            .and_then(|b| b.psk(0, psk.as_bytes()))
            .and_then(Builder::build_initiator)
            .map_err(setup_error)?;
        let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
        let len = handshake
            .write_message(&[], &mut buf)
            .map_err(setup_error)?;
        transport
            .send(frame(chunk::Frame::Data(buf[..len].to_vec())))
            .await?;
        let reply = next_handshake_message(&mut transport).await?;
        handshake
            .read_message(&reply, &mut buf)
            .map_err(|_| TunnelError::Handshake("backend failed authentication".into()))?;
        let noise = handshake.into_transport_mode().map_err(setup_error)?;
        Ok(Self::established(transport, noise, config))
    }

    /// Client side from a relay link: room, server key, and PSK come from
    /// `link`. See [`NoiseStream::initiate`].
    ///
    /// # Errors
    /// As [`NoiseStream::initiate`].
    pub async fn initiate_link(
        transport: T,
        link: &RelayLink,
        config: TunnelConfig,
    ) -> Result<Self, TunnelError> {
        let psk = Psk::from_bytes(*link.psk());
        Self::initiate(transport, link.room_id(), link.server_key(), &psk, config).await
    }

    /// Backend side: wait for the client's first handshake message, verify
    /// it against `keypair`, `psk`, and `room_id`, and answer.
    ///
    /// On failure the responder sends `Chunk.close{}` (best effort) and
    /// drops the transport; no application data is ever read.
    ///
    /// # Errors
    /// [`TunnelError::Handshake`] for a client with the wrong PSK, server
    /// key, or room, or a transport / relay error.
    pub async fn respond(
        mut transport: T,
        room_id: &RoomId,
        keypair: &StaticKeypair,
        psk: &Psk,
        config: TunnelConfig,
    ) -> Result<Self, TunnelError> {
        let prologue = prologue(room_id);
        let mut handshake = Builder::new(noise_params()?)
            .prologue(&prologue)
            .and_then(|b| b.local_private_key(keypair.secret()))
            .and_then(|b| b.psk(0, psk.as_bytes()))
            .and_then(Builder::build_responder)
            .map_err(setup_error)?;
        let hello = next_handshake_message(&mut transport).await?;
        let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
        if handshake.read_message(&hello, &mut buf).is_err() {
            // Best effort: never block on a stalled peer while rejecting it.
            let _ = transport
                .send(frame(chunk::Frame::Close(Close {})))
                .now_or_never();
            return Err(TunnelError::Handshake(
                "client failed authentication (wrong psk, server key, or room)".into(),
            ));
        }
        let len = handshake
            .write_message(&[], &mut buf)
            .map_err(setup_error)?;
        transport
            .send(frame(chunk::Frame::Data(buf[..len].to_vec())))
            .await?;
        let noise = handshake.into_transport_mode().map_err(setup_error)?;
        Ok(Self::established(transport, noise, config))
    }

    fn established(transport: T, noise: TransportState, config: TunnelConfig) -> Self {
        let wakers = Arc::new(SinkWakers::default());
        let sink_waker = waker(wakers.clone());
        NoiseStream {
            transport,
            noise,
            max_record_plaintext: config.max_record_plaintext,
            read_buf: Vec::new(),
            read_pos: 0,
            read_eof: false,
            write_state: WriteState::Open,
            pending_pong: None,
            pong_unflushed: false,
            fatal: None,
            write_error: None,
            wakers,
            sink_waker,
        }
    }

    /// Record a fatal failure (the first one wins) and return it as I/O.
    fn fail(&mut self, error: TunnelError) -> io::Error {
        let error = self.fatal.get_or_insert(error).clone();
        self.read_buf.clear();
        self.read_pos = 0;
        error.into()
    }

    /// Record a send failure (the first one wins) and return it as I/O.
    fn fail_write(&mut self, error: TunnelError) -> io::Error {
        self.write_error.get_or_insert(error).clone().into()
    }

    fn check_writable(&self) -> io::Result<()> {
        if let Some(error) = self.fatal.as_ref().or(self.write_error.as_ref()) {
            return Err(error.clone().into());
        }
        Ok(())
    }

    fn poll_sink_ready(&mut self, cx: &mut Context<'_>, side: Side) -> Poll<io::Result<()>> {
        self.wakers.register(side, cx.waker());
        let sink_waker = self.sink_waker.clone();
        let mut sink_cx = Context::from_waker(&sink_waker);
        match Pin::new(&mut self.transport).poll_ready(&mut sink_cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(self.fail_write(error.into()))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_sink_flush(&mut self, cx: &mut Context<'_>, side: Side) -> Poll<io::Result<()>> {
        self.wakers.register(side, cx.waker());
        let sink_waker = self.sink_waker.clone();
        let mut sink_cx = Context::from_waker(&sink_waker);
        match Pin::new(&mut self.transport).poll_flush(&mut sink_cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(self.fail_write(error.into()))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(&mut self, chunk: chunk::Frame) -> io::Result<()> {
        Pin::new(&mut self.transport)
            .start_send(frame(chunk))
            .map_err(|error| self.fail_write(error.into()))
    }

    /// Encrypt `plaintext` (at most one record) and queue it. The sink must
    /// be ready.
    fn send_record(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let mut record = vec![0u8; plaintext.len() + TAG_LEN];
        let len = match self.noise.write_message(plaintext, &mut record) {
            Ok(len) => len,
            Err(error) => {
                return Err(self.fail(TunnelError::Protocol(format!("encrypt failed: {error}"))));
            }
        };
        record.truncate(len);
        self.start_send(chunk::Frame::Data(record))
    }

    /// Queue a pending pong once the sink has room.
    fn poll_send_pong(&mut self, cx: &mut Context<'_>, side: Side) -> Poll<io::Result<()>> {
        if let Some(seq) = self.pending_pong {
            if self.write_error.is_some() {
                self.pending_pong = None;
                return Poll::Ready(Ok(()));
            }
            match self.poll_sink_ready(cx, side) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
            self.pending_pong = None;
            self.start_send(chunk::Frame::Heartbeat(Heartbeat { seq, pong: true }))?;
            self.pong_unflushed = true;
        }
        Poll::Ready(Ok(()))
    }

    /// Read side: answer probes without ever blocking or failing the read.
    fn drive_pong(&mut self, cx: &mut Context<'_>) {
        if self.poll_send_pong(cx, Side::Read).is_pending() {
            return;
        }
        if self.pong_unflushed
            && self.write_error.is_none()
            && self.poll_sink_flush(cx, Side::Read).is_ready()
        {
            self.pong_unflushed = false;
        }
    }

    /// Handle one received frame. Returns `Some(error)` when the read call
    /// must fail now; `None` to keep reading.
    fn on_frame(&mut self, chunk: Chunk) -> Option<io::Error> {
        match chunk.frame {
            Some(chunk::Frame::Data(bytes)) => {
                if bytes.len() > MAX_NOISE_MESSAGE {
                    return Some(self.fail(TunnelError::Protocol("oversized record".into())));
                }
                self.read_buf.clear();
                self.read_buf.resize(bytes.len(), 0);
                match self.noise.read_message(&bytes, &mut self.read_buf) {
                    Ok(len) => {
                        self.read_buf.truncate(len);
                        self.read_pos = 0;
                        if len == 0 {
                            self.read_eof = true;
                        }
                        None
                    }
                    Err(_) => Some(self.fail(TunnelError::Decrypt)),
                }
            }
            Some(chunk::Frame::Close(_)) => Some(self.fail(TunnelError::Truncated)),
            Some(chunk::Frame::Error(error)) => Some(self.fail(TransportError::from(error).into())),
            Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })) => {
                self.pending_pong = Some(seq);
                None
            }
            // Pongs, handshake frames, and unknown kinds carry no tunnel data.
            _ => None,
        }
    }
}

impl<T> AsyncRead for NoiseStream<T>
where
    T: RelayTransport<Chunk, Chunk> + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.read_pos < this.read_buf.len() {
                let available = &this.read_buf[this.read_pos..];
                let n = available.len().min(buf.remaining());
                buf.put_slice(&available[..n]);
                this.read_pos += n;
                if this.read_pos == this.read_buf.len() {
                    this.read_buf.clear();
                    this.read_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }
            if let Some(error) = &this.fatal {
                return Poll::Ready(Err(error.clone().into()));
            }
            if this.read_eof {
                return Poll::Ready(Ok(()));
            }
            this.drive_pong(cx);
            match Pin::new(&mut this.transport).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Err(this.fail(TunnelError::Truncated))),
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(this.fail(error.into())));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    if let Some(error) = this.on_frame(chunk) {
                        return Poll::Ready(Err(error));
                    }
                }
            }
        }
    }
}

impl<T> AsyncWrite for NoiseStream<T>
where
    T: RelayTransport<Chunk, Chunk> + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check_writable()?;
        if this.write_state != WriteState::Open {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "tunnel write side is shut down",
            )));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        std::task::ready!(this.poll_send_pong(cx, Side::Write))?;
        // Backpressure: encrypt only once the transport can take the record,
        // so at most the transport's own buffer is ever queued.
        std::task::ready!(this.poll_sink_ready(cx, Side::Write))?;
        let len = buf.len().min(this.max_record_plaintext);
        this.send_record(&buf[..len])?;
        Poll::Ready(Ok(len))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check_writable()?;
        std::task::ready!(this.poll_send_pong(cx, Side::Write))?;
        std::task::ready!(this.poll_sink_flush(cx, Side::Write))?;
        this.pong_unflushed = false;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.write_state == WriteState::Closed {
                return Poll::Ready(Ok(()));
            }
            this.check_writable()?;
            match this.write_state {
                WriteState::Open => {
                    std::task::ready!(this.poll_send_pong(cx, Side::Write))?;
                    std::task::ready!(this.poll_sink_ready(cx, Side::Write))?;
                    // The empty record is the authenticated end of stream.
                    this.send_record(&[])?;
                    this.write_state = WriteState::SendCloseFrame;
                }
                WriteState::SendCloseFrame => {
                    std::task::ready!(this.poll_sink_ready(cx, Side::Write))?;
                    this.start_send(chunk::Frame::Close(Close {}))?;
                    this.write_state = WriteState::Flushing;
                }
                WriteState::Flushing => {
                    std::task::ready!(this.poll_sink_flush(cx, Side::Write))?;
                    this.pong_unflushed = false;
                    this.write_state = WriteState::Closed;
                }
                WriteState::Closed => {}
            }
        }
    }
}

// `NoiseStream` must stay usable behind `Box<dyn AsyncRead + AsyncWrite +
// Send>` and across tasks.
const _: fn() = || {
    fn assert_send<X: Send>() {}
    assert_send::<NoiseStream<crate::transport::ChunkTransport>>();
};
