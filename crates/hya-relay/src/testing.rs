//! A tiny in-process HTTP/1.1-only hop, for tests outside this crate that
//! need a real intermediary in front of a [`crate::server::RelayServer`]
//! (for example `hya relay doctor`'s "gRPC blocked by an HTTP/1.1-only hop"
//! case, mirroring the `hya-relay` conformance suite's own hops in
//! `tests/support/hops.rs`, which stays crate-private).
//!
//! Gated behind the `testing` feature; not part of the stable API.

#![doc(hidden)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::net::SocketAddr;

use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

type HopBody = BoxBody<Bytes, hyper::Error>;

/// A running HTTP/1.1-only reverse proxy in front of `upstream`: it accepts
/// only HTTP/1.1 connections (an h2c preface fails the connection), and
/// forwards every request — including WebSocket upgrades — to `upstream`
/// over HTTP/1.1. Models a default `cloudflared` origin connection or an
/// HTTP/1.1-only ingress: gRPC (which needs HTTP/2) cannot cross it, but the
/// WebSocket binding can.
pub struct Http1OnlyHop {
    /// The hop's own listen address; point a client at this instead of the
    /// relay's own address.
    pub addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Http1OnlyHop {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Http1OnlyHop {
    /// Start the hop on an ephemeral `127.0.0.1` port in front of `upstream`.
    ///
    /// # Errors
    /// The ephemeral listener could not be bound.
    pub async fn start(upstream: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let _ = tcp.set_nodelay(true);
                    let service = service_fn(move |request| forward(request, upstream));
                    let io = TokioIo::new(tcp);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .with_upgrades()
                        .await;
                });
            }
        });
        Ok(Self { addr, task })
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response<HopBody> {
    let body = http_body_util::Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never: Infallible| match never {})
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
    upstream: SocketAddr,
) -> Result<Response<HopBody>, Infallible> {
    let Ok(tcp) = TcpStream::connect(upstream).await else {
        return Ok(plain(StatusCode::BAD_GATEWAY, "upstream down"));
    };
    let _ = tcp.set_nodelay(true);
    let path = request
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str())
        .to_owned();
    let authority = request
        .uri()
        .authority()
        .map(ToString::to_string)
        .or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| upstream.to_string());
    let Ok((mut sender, connection)) =
        hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await
    else {
        return Ok(plain(StatusCode::BAD_GATEWAY, "upstream h1 failed"));
    };
    tokio::spawn(connection.with_upgrades());
    let downstream_upgrade = hyper::upgrade::on(&mut request);
    let Ok(uri) = path.parse() else {
        return Ok(plain(StatusCode::BAD_GATEWAY, "bad path"));
    };
    *request.uri_mut() = uri;
    if let Ok(value) = HeaderValue::from_str(&authority) {
        request.headers_mut().insert(header::HOST, value);
    }
    let mut response = match sender.send_request(request).await {
        Ok(response) => response,
        Err(_) => return Ok(plain(StatusCode::BAD_GATEWAY, "upstream request failed")),
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
    Ok(response.map(BodyExt::boxed))
}
