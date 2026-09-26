//! Serving one relay stream: the decrypted tunnel is an ordinary byte
//! stream, so `hyper` serves the `/v1` router over it exactly as over a TCP
//! connection (HTTP/1.1 with upgrades for the PTY WebSocket, or HTTP/2),
//! with the [`Origin::Relay`] extension on every request.

use std::future::Future as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tower::ServiceExt as _;

use crate::Origin;

/// Serve HTTP over `io` until the client is done, `graceful` asks for a
/// graceful end (then at most `grace` more), or `hard` fires.
pub(crate) async fn serve<T>(
    io: T,
    router: Router,
    graceful: CancellationToken,
    hard: CancellationToken,
    grace: Duration,
) where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |mut request: hyper::Request<Incoming>| {
        request.extensions_mut().insert(Origin::Relay);
        let router = router.clone();
        async move { router.oneshot(request).await }
    });
    let builder = auto::Builder::new(TokioExecutor::new());
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(io), service);
    tokio::pin!(connection);
    tokio::select! {
        biased;
        () = hard.cancelled() => return,
        _ = connection.as_mut() => return,
        () = graceful.cancelled() => {}
    }
    connection.as_mut().graceful_shutdown();
    tokio::select! {
        _ = connection => {}
        () = hard.cancelled() => {}
        () = tokio::time::sleep(grace) => {}
    }
}

/// Counts a served relay stream while it lives (also after an HTTP
/// upgrade hands the byte stream to a WebSocket task).
pub(crate) struct ActiveGuard(Arc<AtomicUsize>);

impl ActiveGuard {
    pub(crate) fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A byte stream that fails every read and write once `cancel` fires, so a
/// key rotation or a disconnect also ends streams that an HTTP upgrade
/// handed to a task this module does not own (the PTY WebSocket). It holds
/// the stream's concurrency permit and active-stream count until dropped.
pub(crate) struct Killable<T> {
    inner: T,
    read_cancel: Pin<Box<WaitForCancellationFutureOwned>>,
    write_cancel: Pin<Box<WaitForCancellationFutureOwned>>,
    _active: ActiveGuard,
    _permit: OwnedSemaphorePermit,
}

impl<T> Killable<T> {
    pub(crate) fn new(
        inner: T,
        cancel: &CancellationToken,
        active: ActiveGuard,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            inner,
            read_cancel: Box::pin(cancel.clone().cancelled_owned()),
            write_cancel: Box::pin(cancel.clone().cancelled_owned()),
            _active: active,
            _permit: permit,
        }
    }
}

fn closed() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "the relay stream was closed by the backend",
    )
}

impl<T: AsyncRead + Unpin> AsyncRead for Killable<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.read_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Killable<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
