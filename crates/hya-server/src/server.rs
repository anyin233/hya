//! One server, both protocols: [`Server`] serves the `/v1` HTTP router and
//! the `hya.v1` gRPC binding on the same listener, from one server state.
//!
//! Each connection is served by hyper's auto builder (HTTP/1.1 with
//! upgrades for the PTY WebSocket, or HTTP/2 — `h2c` with prior knowledge
//! on a plain TCP port). A request whose `content-type` starts with
//! `application/grpc` goes to the tonic services behind [`GrpcHostLayer`];
//! everything else goes to the axum router, whose own guard applies the
//! same admission rules. The same service runs on the TCP listener, on any
//! extra listener (`HYA_GRPC_BIND`), and on every relay stream
//! (`relay_host`), so gRPC over the relay carries [`crate::Origin::Relay`]
//! into dispatch exactly like REST.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Extensions, Request, Response, header};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tower::{Layer as _, ServiceExt as _};

use crate::host::{GrpcHostGuard, GrpcHostLayer};
use crate::{AppState, V1Grpc};

/// Build the server for `app`: the HTTP router and the gRPC binding over
/// the one shared server state of `app` (the background drivers start
/// once, and one [`crate::StreamShutdown`] ends the live streams of both).
#[must_use]
pub fn build(app: AppState) -> Server {
    let hosts = app.allowed_hosts();
    let grpc = V1Grpc::new(app.clone());
    Server {
        router: crate::router(app),
        grpc: Some(GrpcHostLayer::new(hosts).layer(grpc.routes())),
    }
}

/// The HTTP router and the gRPC services of one server, served together
/// (see the module docs). Cheap to clone.
#[derive(Clone)]
pub struct Server {
    router: Router,
    grpc: Option<GrpcHostGuard<tonic::service::Routes>>,
}

/// An HTTP-only server (gRPC requests reach the router, which does not
/// route them).
impl From<Router> for Server {
    fn from(router: Router) -> Self {
        Self { router, grpc: None }
    }
}

/// Whether a request is gRPC (`content-type: application/grpc*`).
fn is_grpc<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/grpc"))
}

impl Server {
    /// The HTTP router (without the gRPC services).
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Answer one request: gRPC to the tonic services, anything else to
    /// the router.
    pub(crate) async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
        if is_grpc(&request)
            && let Some(grpc) = &self.grpc
        {
            let Ok(response) = grpc.clone().oneshot(request).await;
            return response.map(Body::new);
        }
        let Ok(response) = self.router.clone().oneshot(request).await;
        response
    }

    /// Serve one connection until it ends or `graceful` asks it to finish
    /// (then it completes its in-flight requests). `extensions` are added
    /// to every request (the peer address, or the relay origin).
    pub(crate) async fn serve_connection<I>(
        &self,
        io: I,
        extensions: Extensions,
        graceful: CancellationToken,
    ) where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let server = self.clone();
        let service = service_fn(move |mut request: Request<Incoming>| {
            request.extensions_mut().extend(extensions.clone());
            let server = server.clone();
            async move { Ok::<_, Infallible>(server.handle(request).await) }
        });
        let builder = auto::Builder::new(TokioExecutor::new());
        let connection = builder.serve_connection_with_upgrades(TokioIo::new(io), service);
        tokio::pin!(connection);
        tokio::select! {
            _ = connection.as_mut() => return,
            () = graceful.cancelled() => {}
        }
        connection.as_mut().graceful_shutdown();
        let _ = connection.await;
    }

    /// Serve `listener` until `shutdown` resolves, then stop accepting and
    /// wait for the open connections to finish their requests (like
    /// `axum::serve(..).with_graceful_shutdown(..)`).
    ///
    /// Every request carries its peer address (`ConnectInfo<SocketAddr>`,
    /// and tonic's `TcpConnectInfo` for gRPC), which the loopback-only
    /// rpcs check. End the live event streams (`StreamShutdown::close`)
    /// inside `shutdown`, or the graceful shutdown waits for their clients.
    pub async fn serve(
        &self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) {
        let local = listener.local_addr().ok();
        let graceful = CancellationToken::new();
        let connections = TaskTracker::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                accepted = listener.accept() => match accepted {
                    Ok((tcp, remote)) => {
                        let _ = tcp.set_nodelay(true);
                        let server = self.clone();
                        let graceful = graceful.clone();
                        connections.spawn(async move {
                            server
                                .serve_connection(tcp, peer_extensions(local, remote), graceful)
                                .await;
                        });
                    }
                    // Out of file descriptors and the like: back off
                    // instead of spinning (what `axum::serve` does).
                    Err(error) => {
                        tracing::warn!(%error, "accepting a connection failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
            }
        }
        drop(listener);
        graceful.cancel();
        connections.close();
        connections.wait().await;
    }
}

/// The per-request extensions of a TCP connection from `remote`.
fn peer_extensions(local: Option<SocketAddr>, remote: SocketAddr) -> Extensions {
    let mut extensions = Extensions::new();
    extensions.insert(ConnectInfo(remote));
    extensions.insert(tonic::transport::server::TcpConnectInfo {
        local_addr: local,
        remote_addr: Some(remote),
    });
    extensions
}
