//! The relay proxy server: the gRPC and WebSocket bindings of
//! `hya.relay.v1` on one listener, in front of a [`ProxyCore`].
//!
//! - **gRPC** (`<prefix>/hya.relay.v1.Relay/{Host,Accept,Open}`): the tonic
//!   `Relay` service. Each stream item is one message; a final `error` frame
//!   from the core becomes the stream's gRPC status.
//! - **WebSocket** (`GET <prefix>/hya.relay.v1/ws/{host,accept,open}`): one
//!   protobuf-encoded message per binary frame. A failure is a final binary
//!   frame carrying the `RelayError`, then a close with code
//!   [`WS_CLOSE_ERROR_BASE`] `+ code`. Text frames are rejected with
//!   [`WS_CLOSE_UNSUPPORTED_DATA`].
//! - **Routing.** One port serves HTTP/1.1 and HTTP/2 (h2c prior knowledge
//!   or ALPN `h2` under TLS). Requests with `content-type: application/grpc*`
//!   go to the gRPC service (after stripping the path prefix); everything
//!   else goes to the WebSocket routes, and any other path answers `404`
//!   with the plain-text body [`NOT_FOUND_BODY`].
//! - **Client identity.** [`PeerInfo`] is the remote socket IP, or with
//!   [`RelayServerConfig::trust_forwarded`] the first valid address from
//!   `CF-Connecting-IP`, `X-Real-IP`, or the leftmost `X-Forwarded-For`.
//!
//! [`RelayServer::bind`] binds the listener and returns the address plus the
//! serve future; it shuts down gracefully when the given signal completes.

mod duplex;
mod grpc;
mod peer;
mod tls;
mod ws;

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Request, Response, StatusCode, Uri, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tower::ServiceExt;

use crate::link::parse_prefix;
use crate::proto::relay_server::RelayServer as GrpcRelayServer;
use crate::proxy::{PeerInfo, ProxyCore, ProxyLimits};

/// WebSocket close code base for relay errors: a stream that fails with
/// `RelayErrorCode` `c` closes with `4000 + c` (for example `4005` for
/// `NOT_FOUND`, `4014` for `UNAVAILABLE`).
pub const WS_CLOSE_ERROR_BASE: u16 = 4000;

/// WebSocket close code for a text frame (the relay speaks binary only).
pub const WS_CLOSE_UNSUPPORTED_DATA: u16 = 1003;

/// Body of the `404` answer to any non-relay request. `hya relay doctor`
/// uses it to recognize a relay server behind an intermediary.
pub const NOT_FOUND_BODY: &str = "hya relay";

/// Default [`RelayServerConfig::drain_timeout`].
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for a TLS handshake on a new connection.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline for an HTTP/1.1 request head.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP/2 keepalive ping interval and ack timeout.
const H2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const H2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Pause after a failed `accept` (for example out of file descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// PEM files for the proxy's own TLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    /// Certificate chain (PEM, leaf first).
    pub cert: PathBuf,
    /// Private key (PEM: PKCS#8, PKCS#1, or SEC1).
    pub key: PathBuf,
}

/// Configuration of a [`RelayServer`].
#[derive(Debug, Clone)]
pub struct RelayServerConfig {
    bind: SocketAddr,
    path_prefix: String,
    tls: Option<TlsFiles>,
    trust_forwarded: bool,
    limits: ProxyLimits,
    drain_timeout: Duration,
}

impl RelayServerConfig {
    /// Plaintext, no prefix, default limits, listening on `bind`.
    #[must_use]
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            path_prefix: String::new(),
            tls: None,
            trust_forwarded: false,
            limits: ProxyLimits::default(),
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        }
    }

    /// Serve both bindings under a path prefix (`"/relay"`, `"a/b/"`; `""`
    /// or `"/"` for none): segments of `A-Za-z0-9-._~`, no `.`/`..`.
    ///
    /// # Errors
    /// [`RelayServerError::InvalidPathPrefix`] for any other shape.
    pub fn path_prefix(mut self, prefix: &str) -> Result<Self, RelayServerError> {
        let trimmed = prefix.strip_suffix('/').unwrap_or(prefix);
        self.path_prefix = parse_prefix(trimmed)
            .map_err(|_| RelayServerError::InvalidPathPrefix(prefix.to_owned()))?;
        Ok(self)
    }

    /// Terminate TLS with these PEM files (ALPN `h2` and `http/1.1`).
    #[must_use]
    pub fn tls(mut self, files: TlsFiles) -> Self {
        self.tls = Some(files);
        self
    }

    /// Identify clients by forwarding headers (`CF-Connecting-IP`, then
    /// `X-Real-IP`, then the leftmost `X-Forwarded-For`) instead of the
    /// socket address. Only safe behind a hop that sets them.
    #[must_use]
    pub fn trust_forwarded(mut self, trust: bool) -> Self {
        self.trust_forwarded = trust;
        self
    }

    /// Resource limits of the proxy core.
    #[must_use]
    pub fn limits(mut self, limits: ProxyLimits) -> Self {
        self.limits = limits;
        self
    }

    /// How long shutdown waits for streams and connections to finish before
    /// cutting them.
    #[must_use]
    pub fn drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    /// The normalized path prefix (`""` or `/seg[/seg…]`).
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.path_prefix
    }
}

/// Failure to start a [`RelayServer`].
#[derive(Debug, thiserror::Error)]
pub enum RelayServerError {
    /// The path prefix is not a valid prefix.
    #[error("invalid path prefix {0:?}: use segments of A-Za-z0-9-._~ (no . or ..)")]
    InvalidPathPrefix(String),
    /// The listener could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// The requested address.
        addr: SocketAddr,
        /// The bind failure.
        #[source]
        source: std::io::Error,
    },
    /// The TLS certificate or key could not be loaded.
    #[error("TLS setup failed: {0}")]
    Tls(String),
}

/// The serve future returned by [`RelayServer::bind`].
pub type RelayServe = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The relay proxy server (both bindings on one port).
#[derive(Debug)]
pub struct RelayServer;

impl RelayServer {
    /// Bind the listener and build the serve future.
    ///
    /// The future accepts connections until `shutdown` completes, then stops
    /// accepting, ends every relay stream with `UNAVAILABLE`
    /// ([`ProxyCore::shutdown`]), and drains connections for at most the
    /// drain timeout.
    ///
    /// # Errors
    /// [`RelayServerError`] when TLS files cannot be loaded or the address
    /// cannot be bound.
    pub async fn bind<F>(
        config: RelayServerConfig,
        shutdown: F,
    ) -> Result<(SocketAddr, RelayServe), RelayServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let acceptor = config.tls.as_ref().map(tls::acceptor).transpose()?;
        let listener =
            TcpListener::bind(config.bind)
                .await
                .map_err(|source| RelayServerError::Bind {
                    addr: config.bind,
                    source,
                })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| RelayServerError::Bind {
                addr: config.bind,
                source,
            })?;

        let core = ProxyCore::new(config.limits.clone());
        let graceful = CancellationToken::new();
        let hard_stop = CancellationToken::new();
        let ws_tasks = TaskTracker::new();
        let router = ws::router(
            &config.path_prefix,
            ws::WsState {
                core: core.clone(),
                tasks: ws_tasks.clone(),
                hard_stop: hard_stop.clone(),
                max_message_size: ws::max_message_size(&config.limits),
            },
        )
        .fallback(not_found);

        let mut builder = auto::Builder::new(TokioExecutor::new());
        builder
            .http1()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_READ_TIMEOUT);
        builder
            .http2()
            .timer(TokioTimer::new())
            .keep_alive_interval(H2_KEEPALIVE_INTERVAL)
            .keep_alive_timeout(H2_KEEPALIVE_TIMEOUT);

        let shared = Arc::new(Shared {
            grpc: GrpcRelayServer::new(grpc::GrpcRelay::new(core.clone())),
            core,
            prefix: config.path_prefix,
            trust_forwarded: config.trust_forwarded,
            router,
            builder,
            graceful,
            hard_stop,
            ws_tasks,
        });
        let serve = serve(listener, shared, acceptor, shutdown, config.drain_timeout);
        Ok((local_addr, Box::pin(serve)))
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ResBody = UnsyncBoxBody<Bytes, BoxError>;

struct Shared {
    core: ProxyCore,
    prefix: String,
    trust_forwarded: bool,
    grpc: GrpcRelayServer<grpc::GrpcRelay>,
    router: axum::Router,
    builder: auto::Builder<TokioExecutor>,
    /// Tells connections to finish their in-flight requests and close.
    graceful: CancellationToken,
    /// Cuts every remaining connection and WebSocket.
    hard_stop: CancellationToken,
    ws_tasks: TaskTracker,
}

async fn serve<F>(
    listener: TcpListener,
    shared: Arc<Shared>,
    acceptor: Option<TlsAcceptor>,
    shutdown: F,
    drain_timeout: Duration,
) where
    F: Future<Output = ()> + Send + 'static,
{
    let connections = TaskTracker::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, remote)) => {
                    connections.spawn(connection(shared.clone(), acceptor.clone(), tcp, remote));
                }
                Err(_) => sleep(ACCEPT_BACKOFF).await,
            },
        }
    }
    drop(listener);

    let deadline = Instant::now() + drain_timeout;
    // End every relay stream (UNAVAILABLE) while connections still carry
    // the final frames and statuses.
    let _ = timeout_at(deadline, shared.core.shutdown()).await;
    shared.graceful.cancel();
    connections.close();
    shared.ws_tasks.close();
    let _ = timeout_at(deadline, async {
        connections.wait().await;
        shared.ws_tasks.wait().await;
    })
    .await;
    shared.hard_stop.cancel();
    connections.wait().await;
    shared.ws_tasks.wait().await;
}

async fn connection(
    shared: Arc<Shared>,
    acceptor: Option<TlsAcceptor>,
    tcp: TcpStream,
    remote: SocketAddr,
) {
    let _ = tcp.set_nodelay(true);
    match acceptor {
        None => drive(&shared, tcp, remote).await,
        Some(acceptor) => {
            let accepted = tokio::select! {
                () = shared.graceful.cancelled() => return,
                accepted = timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)) => accepted,
            };
            if let Ok(Ok(stream)) = accepted {
                drive(&shared, stream, remote).await;
            }
        }
    }
}

/// Serve HTTP/1.1 (with upgrades) or HTTP/2 on one connection.
async fn drive<I>(shared: &Arc<Shared>, io: I, remote: SocketAddr)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service_shared = shared.clone();
    let service = service_fn(move |request| handle(service_shared.clone(), remote, request));
    let connection = shared
        .builder
        .serve_connection_with_upgrades(TokioIo::new(io), service);
    tokio::pin!(connection);
    tokio::select! {
        biased;
        _ = connection.as_mut() => return,
        () = shared.graceful.cancelled() => {}
    }
    connection.as_mut().graceful_shutdown();
    tokio::select! {
        _ = connection => {}
        () = shared.hard_stop.cancelled() => {}
    }
}

async fn handle(
    shared: Arc<Shared>,
    remote: SocketAddr,
    mut request: Request<Incoming>,
) -> Result<Response<ResBody>, Infallible> {
    let peer = peer::identify(remote.ip(), request.headers(), shared.trust_forwarded);
    request.extensions_mut().insert(peer);
    if !is_grpc(&request) {
        let Ok(response) = shared.router.clone().oneshot(request).await;
        return Ok(response.map(boxed));
    }
    let Some(uri) = strip_prefix(request.uri(), &shared.prefix) else {
        let status = tonic::Status::unimplemented("not a relay path");
        return Ok(status.into_http::<tonic::body::Body>().map(boxed));
    };
    *request.uri_mut() = uri;
    let Ok(response) = shared.grpc.clone().oneshot(request).await;
    Ok(response.map(boxed))
}

fn boxed<B>(body: B) -> ResBody
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    body.map_err(Into::into).boxed_unsync()
}

/// Whether a request is gRPC (`content-type: application/grpc*`).
fn is_grpc<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/grpc"))
}

/// Remove the path prefix, or `None` when the path is not below it.
fn strip_prefix(uri: &Uri, prefix: &str) -> Option<Uri> {
    if prefix.is_empty() {
        return Some(uri.clone());
    }
    let rest = uri.path().strip_prefix(prefix)?;
    if !rest.starts_with('/') {
        return None;
    }
    let path_and_query = match uri.query() {
        Some(query) => format!("{rest}?{query}"),
        None => rest.to_owned(),
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Uri::from_parts(parts).ok()
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        NOT_FOUND_BODY,
    )
}

/// The client identity attached to a request (always set by the server).
fn peer_of(extensions: &axum::http::Extensions) -> PeerInfo {
    extensions
        .get::<PeerInfo>()
        .cloned()
        .unwrap_or_else(|| PeerInfo::new("unknown"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_stripped_only_on_a_segment_boundary() {
        let uri: Uri = "/relay/hya.relay.v1.Relay/Host".parse().unwrap_or_default();
        assert_eq!(
            strip_prefix(&uri, "/relay").map(|u| u.path().to_owned()),
            Some("/hya.relay.v1.Relay/Host".to_owned())
        );
        let glued: Uri = "/relayx/hya.relay.v1.Relay/Host"
            .parse()
            .unwrap_or_default();
        assert_eq!(strip_prefix(&glued, "/relay"), None);
        let bare: Uri = "/relay".parse().unwrap_or_default();
        assert_eq!(strip_prefix(&bare, "/relay"), None);
    }
}
