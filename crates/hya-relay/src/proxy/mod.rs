//! The binding-independent relay proxy core (`hya proxy`).
//!
//! [`ProxyCore`] runs the rendezvous state machine over the transport
//! abstraction: the gRPC and WebSocket bindings only adapt their streams to
//! [`ProxyControlTransport`] / [`ChunkTransport`] and hand them to
//! [`ProxyCore::serve_host`], [`ProxyCore::serve_open`], and
//! [`ProxyCore::serve_accept`] together with an opaque [`PeerInfo`].
//!
//! - **Host.** The proxy sends `challenge{nonce}` (32 fresh random bytes);
//!   the host answers `register{ed25519_pubkey, signature}` over
//!   [`register_signing_message`](crate::proto::register_signing_message),
//!   checked with Ed25519 `verify_strict`. The room id is derived from the
//!   key ([`RoomId::from_ed25519`](crate::link::RoomId::from_ed25519)) and
//!   returned in `registered`. A later valid registration for the same room
//!   replaces the current host: the old control stream ends with
//!   `ALREADY_EXISTS` and the old room's streams with `UNAVAILABLE`. When a
//!   control stream ends the room is evicted and all its streams end with
//!   `UNAVAILABLE`.
//! - **Open.** The first frame names the room. The proxy allocates a
//!   128-bit random stream id, sends `incoming{stream_id}` on the host's
//!   control stream, and waits for an `Accept` whose first frame names that
//!   id. Once spliced it sends `opened` to the opener, then relays both
//!   directions.
//! - **Splice.** `data` is forwarded unchanged (never inspected); `close`
//!   is forwarded and ends that direction (half-close); heartbeats are
//!   answered on the leg they arrive on and never forwarded; an `error`
//!   frame is forwarded to the other leg and ends the stream.
//! - **Errors.** Every failure the core reports is a final `error` frame
//!   (`RelayError`) followed by closing the sending direction. The gRPC
//!   binding turns that final frame into the stream status; the WebSocket
//!   binding sends it as is.

mod host;
mod limits;
mod stream;

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rand_core::{OsRng, RngCore, TryRngCore};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use limits::{
    DEFAULT_ACCEPT_TIMEOUT, DEFAULT_EARLY_DATA_LIMIT, DEFAULT_HANDSHAKE_TIMEOUT,
    DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_CHUNK_DATA, DEFAULT_MAX_ROOMS, DEFAULT_MAX_STREAMS_PER_PEER,
    DEFAULT_MAX_STREAMS_PER_ROOM, DEFAULT_STREAM_RATE_BURST_BYTES,
    DEFAULT_STREAM_RATE_BYTES_PER_SEC, ProxyLimits,
};

use crate::link::RoomId;
use crate::proto::{RelayError, RelayErrorCode};
use crate::transport::{ChunkTransport, ProxyControlTransport};

/// Opaque identity of the client behind a relay stream, used only for
/// per-client limits.
///
/// The binding decides what it is — typically the remote IP, or a
/// forwarded client IP when the operator trusts forwarding headers. The
/// core only compares identities for equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerInfo(String);

impl PeerInfo {
    /// Wrap a binding-chosen identity string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identity string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PeerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A point-in-time count of proxy state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProxyStats {
    /// Registered rooms.
    pub rooms: usize,
    /// Streams waiting for `Accept` or spliced.
    pub streams: usize,
    /// Streams still waiting for `Accept`.
    pub pending_streams: usize,
}

/// The relay proxy state machine, shared by every binding.
///
/// Cheap to clone; all clones share one set of rooms and streams.
#[derive(Clone)]
pub struct ProxyCore {
    inner: Arc<Inner>,
}

impl fmt::Debug for ProxyCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyCore")
            .field("limits", &self.inner.limits)
            .field("stats", &self.stats())
            .finish()
    }
}

impl ProxyCore {
    /// A proxy core with no rooms.
    #[must_use]
    pub fn new(limits: ProxyLimits) -> Self {
        Self {
            inner: Arc::new(Inner {
                limits,
                state: Mutex::new(State::default()),
                shutdown: CancellationToken::new(),
                tracker: TaskTracker::new(),
            }),
        }
    }

    /// The limits this core enforces.
    #[must_use]
    pub fn limits(&self) -> &ProxyLimits {
        &self.inner.limits
    }

    /// Current room and stream counts.
    #[must_use]
    pub fn stats(&self) -> ProxyStats {
        let state = self.inner.state();
        ProxyStats {
            rooms: state.rooms.len(),
            streams: state.streams,
            pending_streams: state.pending.len(),
        }
    }

    /// Serve one host control stream (`Host` / `ws/host`) until it ends.
    ///
    /// The returned future owns everything it needs; bindings spawn it.
    pub fn serve_host(
        &self,
        transport: ProxyControlTransport,
        peer: PeerInfo,
    ) -> impl Future<Output = ()> + Send + 'static {
        self.inner
            .tracker
            .track_future(host::run_host(self.inner.clone(), transport, peer))
    }

    /// Serve one opener stream (`Open` / `ws/open`) until it ends.
    pub fn serve_open(
        &self,
        transport: ChunkTransport,
        peer: PeerInfo,
    ) -> impl Future<Output = ()> + Send + 'static {
        self.inner
            .tracker
            .track_future(stream::run_open(self.inner.clone(), transport, peer))
    }

    /// Serve one host data stream (`Accept` / `ws/accept`).
    ///
    /// The future completes once the stream is handed to its waiting opener
    /// (which then drives the splice) or rejected.
    pub fn serve_accept(
        &self,
        transport: ChunkTransport,
        peer: PeerInfo,
    ) -> impl Future<Output = ()> + Send + 'static {
        self.inner
            .tracker
            .track_future(stream::run_accept(self.inner.clone(), transport, peer))
    }

    /// Close every room and stream with `UNAVAILABLE`, refuse new ones, and
    /// wait until every served stream has finished.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.tracker.close();
        self.inner.tracker.wait().await;
    }
}

/// Shared proxy state.
pub(crate) struct Inner {
    pub(crate) limits: ProxyLimits,
    state: Mutex<State>,
    pub(crate) shutdown: CancellationToken,
    tracker: TaskTracker,
}

#[derive(Default)]
struct State {
    rooms: HashMap<String, Arc<Room>>,
    pending: HashMap<String, PendingOpen>,
    peer_streams: HashMap<PeerInfo, usize>,
    streams: usize,
    next_generation: u64,
}

/// A registered room, owned by one host control stream.
pub(crate) struct Room {
    id: String,
    generation: u64,
    /// Cancelled when the room goes away (host gone, replaced, shutdown);
    /// a child of [`Inner::shutdown`].
    pub(crate) token: CancellationToken,
    replaced: AtomicBool,
    /// Stream ids to announce on the control stream as `incoming`.
    incoming: mpsc::Sender<String>,
    /// Streams admitted to this room; changed only under the state lock.
    streams: AtomicUsize,
}

/// An opener waiting for its host's `Accept`.
struct PendingOpen {
    deliver: oneshot::Sender<ChunkTransport>,
}

/// Holds a room registration; dropping it evicts the room (unless a newer
/// registration already replaced it) and closes the room's streams.
pub(crate) struct RoomRegistration {
    inner: Arc<Inner>,
    pub(crate) room: Arc<Room>,
}

impl Drop for RoomRegistration {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state();
            let current = state
                .rooms
                .get(&self.room.id)
                .is_some_and(|room| room.generation == self.room.generation);
            if current {
                state.rooms.remove(&self.room.id);
            }
        }
        self.room.token.cancel();
    }
}

/// Holds one admitted stream's slot in its room and client counts.
pub(crate) struct StreamSlot {
    inner: Arc<Inner>,
    pub(crate) room: Arc<Room>,
    peer: PeerInfo,
    pub(crate) stream_id: String,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut state = self.inner.state();
        state.pending.remove(&self.stream_id);
        self.room.streams.fetch_sub(1, Ordering::Relaxed);
        state.streams = state.streams.saturating_sub(1);
        if let Some(count) = state.peer_streams.get_mut(&self.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.peer_streams.remove(&self.peer);
            }
        }
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register (or replace) the room owned by `room_id`.
    pub(crate) fn register_room(
        self: &Arc<Self>,
        room_id: &RoomId,
    ) -> Result<(RoomRegistration, mpsc::Receiver<String>), RelayError> {
        let mut state = self.state();
        if self.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let previous = state.rooms.get(room_id.as_str()).cloned();
        if previous.is_none() && state.rooms.len() >= self.limits.max_rooms {
            return Err(RelayError::new(
                RelayErrorCode::ResourceExhausted,
                "the proxy serves the maximum number of rooms",
            ));
        }
        state.next_generation += 1;
        let (tx, rx) = mpsc::channel(self.limits.max_streams_per_room.max(1));
        let room = Arc::new(Room {
            id: room_id.as_str().to_owned(),
            generation: state.next_generation,
            token: self.shutdown.child_token(),
            replaced: AtomicBool::new(false),
            incoming: tx,
            streams: AtomicUsize::new(0),
        });
        state.rooms.insert(room.id.clone(), room.clone());
        drop(state);
        if let Some(old) = previous {
            old.replaced.store(true, Ordering::SeqCst);
            old.token.cancel();
        }
        Ok((
            RoomRegistration {
                inner: self.clone(),
                room,
            },
            rx,
        ))
    }

    /// Admit a new stream to `room_id`, announce it to the host, and return
    /// its slot plus the receiver its host leg arrives on.
    pub(crate) fn admit_stream(
        self: &Arc<Self>,
        room_id: &str,
        peer: &PeerInfo,
    ) -> Result<(StreamSlot, oneshot::Receiver<ChunkTransport>), RelayError> {
        let mut state = self.state();
        if self.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let room = state
            .rooms
            .get(room_id)
            .cloned()
            .ok_or_else(|| RelayError::new(RelayErrorCode::NotFound, "room is offline"))?;
        if room.streams.load(Ordering::Relaxed) >= self.limits.max_streams_per_room {
            return Err(RelayError::new(
                RelayErrorCode::ResourceExhausted,
                "the room has the maximum number of streams",
            ));
        }
        if state.peer_streams.get(peer).copied().unwrap_or(0) >= self.limits.max_streams_per_peer {
            return Err(RelayError::new(
                RelayErrorCode::ResourceExhausted,
                "this client has the maximum number of streams",
            ));
        }
        let stream_id = loop {
            let id = new_stream_id();
            if !state.pending.contains_key(&id) {
                break id;
            }
        };
        room.incoming
            .try_send(stream_id.clone())
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RelayError::new(
                    RelayErrorCode::ResourceExhausted,
                    "the room has too many unannounced streams",
                ),
                mpsc::error::TrySendError::Closed(_) => {
                    RelayError::new(RelayErrorCode::NotFound, "room is offline")
                }
            })?;
        let (deliver, arrival) = oneshot::channel();
        state
            .pending
            .insert(stream_id.clone(), PendingOpen { deliver });
        room.streams.fetch_add(1, Ordering::Relaxed);
        *state.peer_streams.entry(peer.clone()).or_default() += 1;
        state.streams += 1;
        Ok((
            StreamSlot {
                inner: self.clone(),
                room,
                peer: peer.clone(),
                stream_id,
            },
            arrival,
        ))
    }

    /// Remove a pending open; `true` when it was still waiting (so no
    /// `Accept` claimed it).
    pub(crate) fn expire_pending(&self, stream_id: &str) -> bool {
        self.state().pending.remove(stream_id).is_some()
    }

    /// Hand an accepted host leg to its waiting opener.
    ///
    /// Returns the transport back when the id is unknown, expired, already
    /// accepted, or the opener is gone.
    pub(crate) fn deliver_accept(
        &self,
        stream_id: &str,
        transport: ChunkTransport,
    ) -> Result<(), ChunkTransport> {
        let pending = self.state().pending.remove(stream_id);
        match pending {
            Some(pending) => pending.deliver.send(transport),
            None => Err(transport),
        }
    }

    /// The error for streams of a room whose token was cancelled.
    pub(crate) fn room_closed_error(&self) -> RelayError {
        if self.shutdown.is_cancelled() {
            shutting_down()
        } else {
            RelayError::new(RelayErrorCode::Unavailable, "room went offline")
        }
    }

    /// The error for a host whose room token was cancelled.
    pub(crate) fn host_closed_error(&self, room: &Room) -> RelayError {
        if self.shutdown.is_cancelled() {
            shutting_down()
        } else if room.replaced.load(Ordering::SeqCst) {
            RelayError::new(
                RelayErrorCode::AlreadyExists,
                "room was registered by a newer control stream",
            )
        } else {
            RelayError::new(RelayErrorCode::Unavailable, "room closed")
        }
    }
}

pub(crate) fn shutting_down() -> RelayError {
    RelayError::new(RelayErrorCode::Unavailable, "the proxy is shutting down")
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    OsRng.unwrap_err().fill_bytes(&mut bytes);
    bytes
}

/// A fresh 128-bit stream id, lowercase hex (32 characters).
fn new_stream_id() -> String {
    data_encoding::HEXLOWER.encode(&random_bytes::<16>())
}
