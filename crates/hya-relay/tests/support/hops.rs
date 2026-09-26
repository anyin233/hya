//! Tiny in-process intermediaries that model what real HTTPS hops do to the
//! relay's traffic.
//!
//! - [`HttpHop`]: an HTTP reverse proxy (hyper on both sides). By default it
//!   speaks HTTP/1.1 and h2c to clients, forwards HTTP/2 requests to the
//!   relay over h2c and HTTP/1.1 requests (including WebSocket upgrades)
//!   over HTTP/1.1. Options model an HTTP/1.1-only hop, a hop that drops
//!   HTTP/2 trailers, a hop that routes by path prefix, and a hop that
//!   rewrites the Host / `:authority`.
//! - [`TcpHop`]: a byte pipe that cuts connections after an idle period or
//!   after a maximum lifetime.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;
use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Uri, Version};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::Instant;

type HopBody = BoxBody<Bytes, hyper::Error>;

/// Behavior of an [`HttpHop`].
#[derive(Clone, Debug, Default)]
pub struct HttpHopOptions {
    /// Serve HTTP/1.1 only (an h2c preface is a protocol error), like a
    /// default `cloudflared` origin connection or many ingresses.
    pub http1_only: bool,
    /// Drop HTTP/2 trailers (and with them `grpc-status`) from responses.
    pub strip_trailers: bool,
    /// Only forward paths below this prefix; everything else is `404`.
    pub route_prefix: Option<String>,
    /// Replace the Host header / `:authority` toward the relay.
    pub rewrite_host: Option<String>,
}

/// A running HTTP hop.
pub struct HttpHop {
    pub addr: SocketAddr,
    /// Requests forwarded to the relay.
    pub forwarded: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for HttpHop {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpHop {
    pub async fn start(upstream: SocketAddr, options: HttpHopOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let forwarded = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new((options, forwarded.clone(), upstream));
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = tcp.set_nodelay(true);
                    let http1_only = shared.0.http1_only;
                    let service = service_fn(move |request| {
                        let shared = shared.clone();
                        async move {
                            let (options, forwarded, upstream) = &*shared;
                            Ok::<_, Infallible>(
                                forward(request, options, forwarded, *upstream).await,
                            )
                        }
                    });
                    let io = TokioIo::new(tcp);
                    if http1_only {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, service)
                            .with_upgrades()
                            .await;
                    } else {
                        let _ = auto::Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(io, service)
                            .await;
                    }
                });
            }
        });
        Self {
            addr,
            forwarded,
            task,
        }
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response<HopBody> {
    let body = http_body_util::Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never| match never {})
        .boxed();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
}

async fn forward(
    mut request: Request<Incoming>,
    options: &HttpHopOptions,
    forwarded: &AtomicUsize,
    upstream: SocketAddr,
) -> Response<HopBody> {
    if let Some(prefix) = &options.route_prefix
        && !request.uri().path().starts_with(&format!("{prefix}/"))
    {
        return plain(StatusCode::NOT_FOUND, "no route");
    }
    let authority = options
        .rewrite_host
        .clone()
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(ToString::to_string)
                .or_else(|| {
                    request
                        .headers()
                        .get(header::HOST)
                        .and_then(|h| h.to_str().ok())
                        .map(str::to_owned)
                })
        })
        .unwrap_or_else(|| upstream.to_string());
    let Ok(tcp) = TcpStream::connect(upstream).await else {
        return plain(StatusCode::BAD_GATEWAY, "upstream down");
    };
    let _ = tcp.set_nodelay(true);
    forwarded.fetch_add(1, Ordering::SeqCst);
    let path = request
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_owned();

    if request.version() == Version::HTTP_2 {
        let Ok((mut sender, connection)) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp)).await
        else {
            return plain(StatusCode::BAD_GATEWAY, "upstream h2 failed");
        };
        tokio::spawn(connection);
        *request.uri_mut() = Uri::builder()
            .scheme("http")
            .authority(authority.as_str())
            .path_and_query(path.as_str())
            .build()
            .unwrap();
        request.headers_mut().remove(header::HOST);
        return match sender.send_request(request).await {
            Ok(response) => {
                let strip = options.strip_trailers;
                response.map(move |body| {
                    if strip {
                        StripTrailers(body).boxed()
                    } else {
                        body.boxed()
                    }
                })
            }
            Err(_) => plain(StatusCode::BAD_GATEWAY, "upstream request failed"),
        };
    }

    // HTTP/1.1, with WebSocket upgrades passed through.
    let Ok((mut sender, connection)) =
        hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await
    else {
        return plain(StatusCode::BAD_GATEWAY, "upstream h1 failed");
    };
    tokio::spawn(connection.with_upgrades());
    let downstream_upgrade = hyper::upgrade::on(&mut request);
    *request.uri_mut() = path.parse().unwrap();
    request
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_str(&authority).unwrap());
    let mut response = match sender.send_request(request).await {
        Ok(response) => response,
        Err(_) => return plain(StatusCode::BAD_GATEWAY, "upstream request failed"),
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            let (Ok(down), Ok(up)) = (downstream_upgrade.await, upstream_upgrade.await) else {
                return;
            };
            let mut down = TokioIo::new(down);
            let mut up = TokioIo::new(up);
            let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
        });
    }
    response.map(BodyExt::boxed)
}

/// A response body without its trailers.
struct StripTrailers(Incoming);

impl Body for StripTrailers {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        loop {
            match Pin::new(&mut self.0).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) if frame.is_trailers() => {}
                other => return other,
            }
        }
    }
}

/// Behavior of a [`TcpHop`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpHopOptions {
    /// Cut a connection after this long without a byte in either direction.
    pub idle_cut: Option<Duration>,
    /// Cut every connection this long after it was accepted.
    pub max_duration: Option<Duration>,
}

/// A running byte-pipe hop.
pub struct TcpHop {
    pub addr: SocketAddr,
    /// Connections this hop cut.
    pub cuts: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for TcpHop {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TcpHop {
    pub async fn start(upstream: SocketAddr, options: TcpHopOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cuts = Arc::new(AtomicUsize::new(0));
        let counter = cuts.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((down, _)) = listener.accept().await else {
                    continue;
                };
                let counter = counter.clone();
                tokio::spawn(async move {
                    let Ok(up) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    let _ = down.set_nodelay(true);
                    let _ = up.set_nodelay(true);
                    if pipe(down, up, options).await {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        Self { addr, cuts, task }
    }
}

/// Copy both ways; returns `true` when the hop cut the connection.
async fn pipe(down: TcpStream, up: TcpStream, options: TcpHopOptions) -> bool {
    let started = Instant::now();
    let last = Arc::new(std::sync::Mutex::new(Instant::now()));
    let (down_read, down_write) = down.into_split();
    let (up_read, up_write) = up.into_split();
    let copy = |mut from: tokio::net::tcp::OwnedReadHalf,
                mut to: tokio::net::tcp::OwnedWriteHalf,
                last: Arc<std::sync::Mutex<Instant>>| async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match from.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = to.shutdown().await;
                    return;
                }
                Ok(n) => {
                    *last.lock().unwrap() = Instant::now();
                    if to.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            }
        }
    };
    let mut upward = tokio::spawn(copy(down_read, up_write, last.clone()));
    let mut downward = tokio::spawn(copy(up_read, down_write, last.clone()));
    let mut up_done = false;
    let mut down_done = false;
    let cut = loop {
        let idle_deadline = options.idle_cut.map(|idle| *last.lock().unwrap() + idle);
        let max_deadline = options.max_duration.map(|max| started + max);
        let deadline = match (idle_deadline, max_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        tokio::select! {
            _ = &mut upward, if !up_done => up_done = true,
            _ = &mut downward, if !down_done => down_done = true,
            () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                let now = Instant::now();
                let idle = options.idle_cut.is_some_and(|idle| now >= *last.lock().unwrap() + idle);
                let expired = options.max_duration.is_some_and(|max| now >= started + max);
                if idle || expired {
                    break true;
                }
            }
        }
        if up_done && down_done {
            break false;
        }
    };
    upward.abort();
    downward.abort();
    cut
}
