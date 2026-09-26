//! The relay client: both bindings toward a `hya proxy`, shared by the host
//! connector (inside `hya serve`) and the client bridge.
//!
//! [`RelayClient`] turns a [`RelayAddress`] into relay streams:
//! [`RelayClient::open`] (client side of a data stream, returned once the
//! proxy acknowledged it with `opened`; it presents the room's open token,
//! which a client made [from a link](RelayClient::from_link) derives from the
//! link's PSK), [`RelayClient::accept`] (host side
//! of a data stream), and [`RelayClient::host`] (the host control stream;
//! [`register_host`] runs the registration on it). Every stream comes back
//! as the binding-independent transport type, wrapped with application
//! heartbeats ([`heartbeat`]).
//!
//! **Bindings.** gRPC (one HTTP/2 connection per client, h2c in plaintext,
//! ALPN `h2` under TLS) or WebSocket (one HTTP/1.1 connection per stream,
//! ALPN `http/1.1`). The path prefix of the address applies to both.
//!
//! **Negotiation.** With [`Transport::Auto`] the client probes gRPC once per
//! relay address and process: it opens a stream to a random offline room,
//! which a working path answers with the gRPC status `NOT_FOUND`. Anything
//! else (no HTTP/2, an HTTP status instead of gRPC, a stream that ends
//! without trailers, no answer within the probe timeout) makes it probe the
//! WebSocket binding the same way and use that. The decision and its reason
//! ([`BindingChoice`]) are remembered per (scheme, host, port, prefix) for
//! the process lifetime. `Grpc`/`Ws` pin a binding without probing.
//!
//! **TLS.** rustls with the operating-system roots, the webpki roots, and an
//! optional extra CA file; the server name is the address host.
//! `hya+insecure://` addresses are plaintext.
//!
//! **Message size.** Every frame the client accepts is bounded by
//! [`MAX_CLIENT_MESSAGE_SIZE`] (one Noise record plus framing), on the
//! WebSocket (message and frame size) and gRPC (decode size) bindings alike,
//! so a hostile relay or peer cannot make the client buffer more.

mod backoff;
mod connect;
mod grpc;
pub mod heartbeat;
mod ws;

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use tokio::time::timeout;

pub use backoff::{Backoff, ReconnectPolicy, RetryKind};
pub use heartbeat::{DEFAULT_HEARTBEAT_INTERVAL, HeartbeatConfig, with_heartbeat};

use self::connect::Tls;
use self::grpc::GrpcService;
use self::heartbeat::{IncomingFrame, OutgoingFrame};
use crate::keys::OpenToken;
use crate::link::{RelayAddress, RelayLink, RoomId, Transport, WsRoute};
use crate::proto::{
    Accept, Chunk, HostFrame, Open, ProxyToHost, Register, RelayErrorCode, UpdateOpenToken, chunk,
    host_frame, proxy_to_host, register_signing_message, update_open_token_signing_message,
};
use crate::server::duplex::FinalError;
use crate::transport::{BoxedTransport, ChunkTransport, HostControlTransport, TransportError};

/// Default [`ClientConfig::connect_timeout`].
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default [`ClientConfig::probe_timeout`].
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default [`ClientConfig::open_timeout`].
pub const DEFAULT_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest relay message (WebSocket message or frame, gRPC message) the
/// client accepts: one maximal Noise record
/// ([`MAX_NOISE_MESSAGE`](crate::tunnel::MAX_NOISE_MESSAGE)) plus 1 KiB of
/// protobuf and binding framing.
pub const MAX_CLIENT_MESSAGE_SIZE: usize = crate::tunnel::MAX_NOISE_MESSAGE + 1024;
/// HTTP/2 keepalive ack timeout when dead-peer detection is off.
const H2_KEEPALIVE_ACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Settings of a [`RelayClient`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Binding choice: `Auto` negotiates, `Grpc`/`Ws` pin (default `Auto`).
    pub transport: Transport,
    /// A PEM file of extra trusted CA certificates (private CAs).
    pub extra_ca_pem: Option<PathBuf>,
    /// Heartbeat and dead-peer timing of every stream (default: probe every
    /// 15 s, dead after 45 s of silence). The gRPC connection also sends
    /// HTTP/2 keepalive pings at the heartbeat interval.
    pub heartbeat: HeartbeatConfig,
    /// Deadline for TCP, TLS, and the HTTP/2 or WebSocket handshake of one
    /// connection, and for the proxy's first answer on a new stream
    /// (default 10 s).
    pub connect_timeout: Duration,
    /// Deadline of each binding probe (default 5 s).
    pub probe_timeout: Duration,
    /// Deadline for [`RelayClient::open`] to get `opened` (default 30 s).
    pub open_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Auto,
            extra_ca_pem: None,
            heartbeat: HeartbeatConfig::default(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            open_timeout: DEFAULT_OPEN_TIMEOUT,
        }
    }
}

/// A relay binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// gRPC over HTTP/2.
    Grpc,
    /// WebSocket over HTTP/1.1.
    Ws,
}

impl Binding {
    /// `grpc` or `ws` (the matching `t=` value).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Binding::Grpc => "grpc",
            Binding::Ws => "ws",
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What went wrong on a path to the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailureKind {
    /// TCP connect failed or timed out.
    Connect,
    /// The TLS handshake failed (certificate, name, protocol).
    Tls,
    /// The path does not carry HTTP/2: TLS did not negotiate `h2`, or the
    /// HTTP/2 connection failed after connecting (an HTTP/1.1-only hop
    /// answering or closing on the h2c preface).
    NoHttp2,
    /// A hop answered with an HTTP error instead of the relay's answer.
    HopRejected,
    /// The gRPC stream ended without a status: a hop drops HTTP/2 trailers.
    TrailersStripped,
    /// No answer within the deadline: a hop buffers or the relay hangs.
    Timeout,
    /// The relay answered, but the path is not a relay route: the path
    /// prefix does not match the relay's `--path-prefix`.
    WrongPath,
    /// Any other unexpected answer.
    Unexpected,
}

/// Why a binding does not work on this path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeFailure {
    /// Failure class.
    pub kind: ProbeFailureKind,
    /// Human-readable detail (no secrets).
    pub detail: String,
}

impl fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Why [`BindingChoice::binding`] was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceReason {
    /// The link or the configuration pinned it (`t=grpc` / `t=ws`).
    Pinned,
    /// `auto`: the gRPC probe succeeded.
    GrpcWorks,
    /// `auto`: the gRPC probe failed like this, the WebSocket probe worked.
    GrpcFailed(ProbeFailure),
}

impl fmt::Display for ChoiceReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChoiceReason::Pinned => f.write_str("pinned by configuration"),
            ChoiceReason::GrpcWorks => f.write_str("gRPC works on this path"),
            ChoiceReason::GrpcFailed(failure) => {
                write!(
                    f,
                    "gRPC does not work on this path ({failure}); using WebSocket"
                )
            }
        }
    }
}

/// The binding a client uses for a relay address, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingChoice {
    /// The binding.
    pub binding: Binding,
    /// Why.
    pub reason: ChoiceReason,
}

/// A relay client failure.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// `open`: the room has no host (the proxy said `NOT_FOUND`).
    #[error("relay room is offline: {0}")]
    RoomOffline(String),
    /// The proxy said `UNAVAILABLE` (host did not accept in time, room went
    /// away, proxy shutting down).
    #[error("relay unavailable: {0}")]
    Unavailable(String),
    /// Any other status from the proxy.
    #[error("relay error {code:?}: {message}")]
    Relay {
        /// The relay error code.
        code: RelayErrorCode,
        /// The proxy's message.
        message: String,
    },
    /// The pinned (or remembered) binding cannot reach the relay.
    #[error("cannot reach the relay over {binding}: {failure}")]
    Connect {
        /// The binding that was tried.
        binding: Binding,
        /// What went wrong.
        failure: ProbeFailure,
    },
    /// `auto`: neither binding works on this path.
    #[error("no relay binding works: gRPC: {grpc}; WebSocket: {ws}")]
    NoBinding {
        /// Why gRPC failed.
        grpc: ProbeFailure,
        /// Why WebSocket failed.
        ws: ProbeFailure,
    },
    /// The stream failed below the relay protocol.
    #[error("relay transport failed: {0}")]
    Transport(String),
    /// A deadline passed.
    #[error("relay timeout: {0}")]
    Timeout(String),
    /// The proxy sent something the protocol does not allow here.
    #[error("relay protocol violation: {0}")]
    Protocol(String),
    /// The client configuration is unusable (for example the CA file).
    #[error("relay client configuration: {0}")]
    Config(String),
}

impl ClientError {
    /// The relay error code behind this error, if the proxy sent one.
    #[must_use]
    pub fn code(&self) -> Option<RelayErrorCode> {
        match self {
            ClientError::RoomOffline(_) => Some(RelayErrorCode::NotFound),
            ClientError::Unavailable(_) => Some(RelayErrorCode::Unavailable),
            ClientError::Relay { code, .. } => Some(*code),
            _ => None,
        }
    }

    fn from_transport(error: TransportError) -> Self {
        match error {
            TransportError::Status { code, message } => Self::from_status(code, message),
            TransportError::Closed => ClientError::Transport("the stream was closed".to_owned()),
            TransportError::Decode(error) => ClientError::Protocol(error.to_string()),
            TransportError::Transport(message) => ClientError::Transport(message),
        }
    }

    fn from_status(code: RelayErrorCode, message: String) -> Self {
        match code {
            RelayErrorCode::NotFound => ClientError::RoomOffline(message),
            RelayErrorCode::Unavailable => ClientError::Unavailable(message),
            code => ClientError::Relay { code, message },
        }
    }
}

type MemoKey = RelayAddress;

/// Bindings chosen by `auto`, per relay address, for the process lifetime.
static REMEMBERED: LazyLock<Mutex<HashMap<MemoKey, BindingChoice>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn remembered(address: &RelayAddress) -> Option<BindingChoice> {
    REMEMBERED
        .lock()
        .ok()
        .and_then(|memo| memo.get(address).cloned())
}

/// A client of one relay address. Cheap to clone; clones share the gRPC
/// connection.
#[derive(Clone)]
pub struct RelayClient {
    inner: Arc<Inner>,
}

struct Inner {
    address: RelayAddress,
    /// The room and open token of the link this client was made from.
    link_token: Option<(RoomId, OpenToken)>,
    config: ClientConfig,
    tls: Option<Tls>,
    grpc: tokio::sync::Mutex<Option<GrpcService>>,
    probing: tokio::sync::Mutex<()>,
}

impl fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayClient")
            .field("address", &self.inner.address)
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl RelayClient {
    /// A client for `address` (TLS for `hya://`/`https`, plaintext for
    /// `hya+insecure://`/`http`). Nothing is connected yet.
    ///
    /// # Errors
    /// [`ClientError::Config`] when the extra CA file cannot be read.
    pub fn new(address: RelayAddress, config: ClientConfig) -> Result<Self, ClientError> {
        let tls = if address.is_secure() {
            Some(Tls::new(config.extra_ca_pem.as_deref())?)
        } else {
            None
        };
        Ok(Self {
            inner: Arc::new(Inner {
                address,
                link_token: None,
                config,
                tls,
                grpc: tokio::sync::Mutex::new(None),
                probing: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// A client for the relay of `link`. A pinned `t=` in the link wins
    /// over `config.transport = Auto`; a pinned `config.transport` wins over
    /// the link.
    ///
    /// # Errors
    /// As [`RelayClient::new`].
    ///
    /// The client keeps the link's open token, so [`RelayClient::open`] to
    /// the link's room presents it.
    pub fn from_link(link: &RelayLink, mut config: ClientConfig) -> Result<Self, ClientError> {
        if config.transport == Transport::Auto {
            config.transport = link.transport();
        }
        let mut client = Self::new(link.address().clone(), config)?;
        if let Some(inner) = Arc::get_mut(&mut client.inner) {
            inner.link_token = Some((link.room_id().clone(), link.open_token()));
        }
        Ok(client)
    }

    /// The relay address.
    #[must_use]
    pub fn address(&self) -> &RelayAddress {
        &self.inner.address
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// The binding `auto` remembered for this address, if it probed already
    /// (in this process, by any client).
    #[must_use]
    pub fn remembered_binding(&self) -> Option<BindingChoice> {
        remembered(&self.inner.address)
    }

    /// Forget the remembered binding of this address, so the next stream
    /// probes again (for example after the path changed).
    pub fn forget_binding(&self) {
        if let Ok(mut memo) = REMEMBERED.lock() {
            memo.remove(&self.inner.address);
        }
    }

    /// The binding streams use: pinned, remembered, or negotiated now.
    ///
    /// # Errors
    /// [`ClientError::NoBinding`] when `auto` finds neither binding working.
    pub async fn binding(&self) -> Result<BindingChoice, ClientError> {
        let pinned = |binding| BindingChoice {
            binding,
            reason: ChoiceReason::Pinned,
        };
        match self.inner.config.transport {
            Transport::Grpc => return Ok(pinned(Binding::Grpc)),
            Transport::Ws => return Ok(pinned(Binding::Ws)),
            Transport::Auto => {}
        }
        if let Some(choice) = self.remembered_binding() {
            return Ok(choice);
        }
        let _probing = self.inner.probing.lock().await;
        if let Some(choice) = self.remembered_binding() {
            return Ok(choice);
        }
        let choice = match self.probe(Binding::Grpc).await {
            Ok(()) => BindingChoice {
                binding: Binding::Grpc,
                reason: ChoiceReason::GrpcWorks,
            },
            Err(grpc) => match self.probe(Binding::Ws).await {
                Ok(()) => BindingChoice {
                    binding: Binding::Ws,
                    reason: ChoiceReason::GrpcFailed(grpc),
                },
                Err(ws) => return Err(ClientError::NoBinding { grpc, ws }),
            },
        };
        if let Ok(mut memo) = REMEMBERED.lock() {
            memo.insert(self.inner.address.clone(), choice.clone());
        }
        Ok(choice)
    }

    /// Check one binding end to end: open a stream to a random (offline)
    /// room and expect the relay's `NOT_FOUND`, within the probe timeout.
    ///
    /// # Errors
    /// The classified [`ProbeFailure`].
    pub async fn probe(&self, binding: Binding) -> Result<(), ProbeFailure> {
        let deadline = self.inner.config.probe_timeout;
        let run = async {
            match binding {
                Binding::Grpc => self.probe_grpc().await,
                Binding::Ws => self.probe_ws().await,
            }
        };
        match timeout(deadline, run).await {
            Ok(result) => result,
            Err(_) => Err(ProbeFailure {
                kind: ProbeFailureKind::Timeout,
                detail: format!(
                    "no {binding} answer within {deadline:?}: a hop may buffer streaming responses"
                ),
            }),
        }
    }

    async fn probe_grpc(&self) -> Result<(), ProbeFailure> {
        let service = self.grpc_service().await?;
        let started = grpc::start(
            service,
            Some(open_frame(&random_room(), None)),
            |mut c, r| async move { c.open(r).await },
        )
        .await;
        let first = match started {
            Ok(transport) => {
                let mut transport: ChunkTransport = Box::pin(transport);
                transport.next().await
            }
            Err(error) => Some(Err(error)),
        };
        classify_probe(Binding::Grpc, first)
    }

    async fn probe_ws(&self) -> Result<(), ProbeFailure> {
        let socket = ws::connect_route(
            &self.inner.address,
            self.inner.tls.as_ref(),
            WsRoute::Open,
            &self.inner.config,
        )
        .await?;
        let mut transport: ChunkTransport = Box::pin(ws::WsTransport::<Chunk, Chunk>::new(socket));
        let first = match transport.send(open_frame(&random_room(), None)).await {
            Ok(()) => transport.next().await,
            Err(error) => Some(Err(error)),
        };
        classify_probe(Binding::Ws, first)
    }

    /// The shared gRPC connection (connected on first use).
    async fn grpc_service(&self) -> Result<GrpcService, ProbeFailure> {
        let mut slot = self.inner.grpc.lock().await;
        if let Some(service) = slot.as_ref() {
            return Ok(service.clone());
        }
        let service = grpc::channel(
            &self.inner.address,
            self.inner.tls.clone(),
            &self.inner.config,
        )
        .await?;
        *slot = Some(service.clone());
        Ok(service)
    }

    /// Start a stream on the chosen binding, `first` queued first.
    async fn stream<Tx, Rx, F, Fut>(
        &self,
        route: WsRoute,
        first: Option<Tx>,
        call: F,
    ) -> Result<BoxedTransport<Tx, Rx>, ClientError>
    where
        Tx: OutgoingFrame + prost::Message,
        Rx: IncomingFrame + prost::Message + Default + FinalError,
        F: FnOnce(
            grpc::GrpcClient,
            tonic::Request<tokio_stream::wrappers::ReceiverStream<Tx>>,
        ) -> Fut,
        Fut: Future<Output = Result<tonic::Response<tonic::Streaming<Rx>>, tonic::Status>>,
    {
        let choice = self.binding().await?;
        let connect_timeout = self.inner.config.connect_timeout;
        let raw: BoxedTransport<Tx, Rx> = match choice.binding {
            Binding::Grpc => {
                let service =
                    self.grpc_service()
                        .await
                        .map_err(|failure| ClientError::Connect {
                            binding: Binding::Grpc,
                            failure,
                        })?;
                let started = timeout(connect_timeout, grpc::start(service, first, call))
                    .await
                    .map_err(|_| {
                        ClientError::Timeout(format!("no gRPC response within {connect_timeout:?}"))
                    })?;
                Box::pin(started.map_err(ClientError::from_transport)?)
            }
            Binding::Ws => {
                let socket = ws::connect_route(
                    &self.inner.address,
                    self.inner.tls.as_ref(),
                    route,
                    &self.inner.config,
                )
                .await
                .map_err(|failure| ClientError::Connect {
                    binding: Binding::Ws,
                    failure,
                })?;
                let mut transport: BoxedTransport<Tx, Rx> =
                    Box::pin(ws::WsTransport::<Tx, Rx>::new(socket));
                if let Some(first) = first {
                    transport
                        .send(first)
                        .await
                        .map_err(ClientError::from_transport)?;
                }
                transport
            }
        };
        Ok(with_heartbeat(raw, self.inner.config.heartbeat))
    }

    /// Open a data stream to `room` (the client side) and wait for the
    /// proxy's `opened`; the returned transport carries the tunnel.
    ///
    /// Presents the open token of the link the client was made from
    /// ([`RelayClient::from_link`]) when `room` is that link's room, and no
    /// token otherwise (which the proxy answers like an offline room); see
    /// [`RelayClient::open_with_token`].
    ///
    /// # Errors
    /// [`ClientError::RoomOffline`] (`NOT_FOUND`: offline, or the wrong open
    /// token), [`ClientError::Unavailable`] (the host did not accept in
    /// time), [`ClientError::Timeout`], or a connection failure.
    pub async fn open(&self, room: &RoomId) -> Result<ChunkTransport, ClientError> {
        let token = self
            .inner
            .link_token
            .as_ref()
            .filter(|(linked, _)| linked == room)
            .map(|(_, token)| token);
        self.open_stream(room, token).await
    }

    /// [`RelayClient::open`] presenting `token` (from
    /// [`RelayLink::open_token`] or [`OpenToken::derive`]).
    ///
    /// # Errors
    /// As [`RelayClient::open`].
    pub async fn open_with_token(
        &self,
        room: &RoomId,
        token: &OpenToken,
    ) -> Result<ChunkTransport, ClientError> {
        self.open_stream(room, Some(token)).await
    }

    async fn open_stream(
        &self,
        room: &RoomId,
        token: Option<&OpenToken>,
    ) -> Result<ChunkTransport, ClientError> {
        let open_timeout = self.inner.config.open_timeout;
        let run = async {
            let mut transport = self
                .stream(
                    WsRoute::Open,
                    Some(open_frame(room, token)),
                    |mut c, r| async move { c.open(r).await },
                )
                .await?;
            // Heartbeats never reach here (the wrapper swallows them), so
            // the first frame is the answer.
            match transport.next().await {
                Some(Ok(Chunk {
                    frame: Some(chunk::Frame::Opened(_)),
                })) => Ok(transport),
                Some(Ok(other)) => Err(ClientError::Protocol(format!(
                    "expected `opened`, got {other:?}"
                ))),
                Some(Err(error)) => Err(ClientError::from_transport(error)),
                None => Err(ClientError::Transport(
                    "the relay ended the stream before `opened`".to_owned(),
                )),
            }
        };
        timeout(open_timeout, run)
            .await
            .map_err(|_| ClientError::Timeout(format!("no `opened` within {open_timeout:?}")))?
    }

    /// Accept the data stream `stream_id` announced by an `incoming` frame
    /// (the host side). Errors from the proxy (for example `NOT_FOUND` for an
    /// unknown id) arrive on the returned transport.
    ///
    /// # Errors
    /// A connection failure.
    pub async fn accept(&self, stream_id: &str) -> Result<ChunkTransport, ClientError> {
        let first = Chunk {
            frame: Some(chunk::Frame::Accept(Accept {
                stream_id: stream_id.to_owned(),
            })),
        };
        self.stream(WsRoute::Accept, Some(first), |mut c, r| async move {
            c.accept(r).await
        })
        .await
    }

    /// Start a host control stream. The proxy's first frame is the
    /// registration challenge; see [`register_host`].
    ///
    /// # Errors
    /// A connection failure.
    pub async fn host(&self) -> Result<HostControlTransport, ClientError> {
        self.stream::<HostFrame, ProxyToHost, _, _>(WsRoute::Host, None, |mut c, r| async move {
            c.host(r).await
        })
        .await
    }
}

fn open_frame(room: &RoomId, token: Option<&OpenToken>) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Open(Open {
            room_id: room.as_str().to_owned(),
            open_token: token
                .map(|token| token.as_bytes().to_vec())
                .unwrap_or_default(),
        })),
    }
}

/// A well-formed room id nobody registered (random key).
fn random_room() -> RoomId {
    RoomId::from_ed25519(&crate::proxy::random_bytes::<32>())
}

/// Classify the first answer to a probe; `NOT_FOUND` means the path works.
fn classify_probe(
    binding: Binding,
    first: Option<Result<Chunk, TransportError>>,
) -> Result<(), ProbeFailure> {
    match first {
        Some(Err(TransportError::Status {
            code: RelayErrorCode::NotFound,
            ..
        })) => Ok(()),
        Some(Err(TransportError::Status { code, message })) => {
            Err(classify_status(binding, code, &message))
        }
        Some(Err(TransportError::Transport(detail))) => Err(classify_transport(binding, &detail)),
        Some(Err(other)) => Err(ProbeFailure {
            kind: ProbeFailureKind::Unexpected,
            detail: format!("{binding}: {other}"),
        }),
        Some(Ok(frame)) => Err(ProbeFailure {
            kind: ProbeFailureKind::Unexpected,
            detail: format!("unexpected answer to the {binding} probe: {frame:?}"),
        }),
        None => Err(match binding {
            Binding::Grpc => ProbeFailure {
                kind: ProbeFailureKind::TrailersStripped,
                detail: "the gRPC stream ended without a status: a hop strips HTTP/2 trailers"
                    .to_owned(),
            },
            Binding::Ws => ProbeFailure {
                kind: ProbeFailureKind::Unexpected,
                detail: "the WebSocket closed without an answer".to_owned(),
            },
        }),
    }
}

/// Classify a relay status other than the expected `NOT_FOUND`.
fn classify_status(binding: Binding, code: RelayErrorCode, message: &str) -> ProbeFailure {
    if message == "not a relay path" {
        return ProbeFailure {
            kind: ProbeFailureKind::WrongPath,
            detail: format!(
                "the relay answered {binding} with \"{message}\": the path prefix does not match the relay's --path-prefix"
            ),
        };
    }
    ProbeFailure {
        kind: ProbeFailureKind::Unexpected,
        detail: format!("the relay answered the {binding} probe with {code:?}: {message}"),
    }
}

/// Classify a failure below the relay protocol.
fn classify_transport(binding: Binding, detail: &str) -> ProbeFailure {
    if let Some(http) = grpc::hop_http_status(detail) {
        return ProbeFailure {
            kind: ProbeFailureKind::HopRejected,
            detail: format!("a hop answered the gRPC request with HTTP {http} instead of gRPC"),
        };
    }
    // Below gRPC, after TCP (and TLS) succeeded: the HTTP/2 exchange itself
    // broke, which is what an HTTP/1.1-only hop does to the h2c preface.
    let kind = match binding {
        Binding::Grpc => ProbeFailureKind::NoHttp2,
        Binding::Ws => ProbeFailureKind::Unexpected,
    };
    ProbeFailure {
        kind,
        detail: format!("{binding}: {detail}"),
    }
}

/// A room registration on one host control stream.
#[derive(Debug, Clone)]
pub struct HostRegistration {
    room: RoomId,
    nonce: Vec<u8>,
}

impl HostRegistration {
    /// The registered room.
    #[must_use]
    pub fn room(&self) -> &RoomId {
        &self.room
    }

    /// The signed `update_open_token` frame that replaces the room's open
    /// token hash on this control stream (after a PSK rotation). The proxy
    /// answers `open_token_updated`.
    #[must_use]
    pub fn update_open_token_frame(
        &self,
        key: &SigningKey,
        open_token_hash: &[u8; 32],
    ) -> HostFrame {
        HostFrame {
            frame: Some(host_frame::Frame::UpdateOpenToken(UpdateOpenToken {
                open_token_hash: open_token_hash.to_vec(),
                signature: key
                    .sign(&update_open_token_signing_message(
                        &self.nonce,
                        open_token_hash,
                    ))
                    .to_bytes()
                    .to_vec(),
            })),
        }
    }
}

/// Answer the registration challenge on a fresh control stream with `key`,
/// registering `open_token_hash` (`sha256` of the room's open token,
/// [`OpenToken::hash`]). Returns the registration once the proxy confirmed
/// it, within `deadline`.
///
/// # Errors
/// [`ClientError::Relay`] with the proxy's code (for example
/// `UNAUTHENTICATED`, `RESOURCE_EXHAUSTED`), [`ClientError::Timeout`], or a
/// transport failure.
pub async fn register_host(
    transport: &mut HostControlTransport,
    key: &SigningKey,
    open_token_hash: &[u8; 32],
    deadline: Duration,
) -> Result<HostRegistration, ClientError> {
    let run = async {
        let nonce = match next_control(transport).await? {
            proxy_to_host::Frame::Challenge(challenge) => challenge.nonce,
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected a challenge, got {other:?}"
                )));
            }
        };
        let register = HostFrame {
            frame: Some(host_frame::Frame::Register(Register {
                ed25519_pubkey: key.verifying_key().as_bytes().to_vec(),
                signature: key
                    .sign(&register_signing_message(&nonce, open_token_hash))
                    .to_bytes()
                    .to_vec(),
                open_token_hash: open_token_hash.to_vec(),
            })),
        };
        transport
            .send(register)
            .await
            .map_err(ClientError::from_transport)?;
        match next_control(transport).await? {
            proxy_to_host::Frame::Registered(registered) => RoomId::parse(&registered.room_id)
                .map(|room| HostRegistration { room, nonce })
                .map_err(|_| ClientError::Protocol("malformed registered room id".to_owned())),
            other => Err(ClientError::Protocol(format!(
                "expected `registered`, got {other:?}"
            ))),
        }
    };
    timeout(deadline, run)
        .await
        .map_err(|_| ClientError::Timeout(format!("registration took longer than {deadline:?}")))?
}

async fn next_control(
    transport: &mut HostControlTransport,
) -> Result<proxy_to_host::Frame, ClientError> {
    loop {
        match transport.next().await {
            Some(Ok(ProxyToHost { frame: Some(frame) })) => match frame {
                proxy_to_host::Frame::Error(error) => {
                    return Err(ClientError::from_status(error.error_code(), error.message));
                }
                proxy_to_host::Frame::Heartbeat(_) => {}
                frame => return Ok(frame),
            },
            Some(Ok(ProxyToHost { frame: None })) => {}
            Some(Err(error)) => return Err(ClientError::from_transport(error)),
            None => {
                return Err(ClientError::Transport(
                    "the relay ended the control stream".to_owned(),
                ));
            }
        }
    }
}
