//! The gRPC binding, client side: one HTTP/2 channel per relay client, the
//! path prefix added to every call, and calls that can be aborted.

use std::error::Error as _;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream, StreamExt};
use hyper::body::{Body as HttpBody, Bytes, Frame, SizeHint};
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::{CancellationToken, PollSender, WaitForCancellationFutureOwned};
use tonic::codegen::http;
use tonic::transport::Endpoint;
use tonic::{Response, Status, Streaming};
use tower::ServiceBuilder;
use tower::util::BoxCloneService;

use super::connect::{ALPN_H2, ConnectFailed, Tls, connect};
use super::{ClientConfig, ProbeFailure, ProbeFailureKind};
use crate::link::RelayAddress;
use crate::proto::relay_client::RelayClient as GeneratedClient;
use crate::proto::relay_error_code_from_grpc;
use crate::transport::TransportError;

/// Frames queued toward the proxy before the sink waits.
const OUTGOING_CAPACITY: usize = 16;
/// How long a cleanly closed, dropped call waits for the proxy to end it.
const CLOSE_LINGER: std::time::Duration = std::time::Duration::from_secs(10);

/// The tonic service every call goes through (prefix and abort applied).
pub(crate) type GrpcService = BoxCloneService<
    http::Request<tonic::body::Body>,
    http::Response<tonic::body::Body>,
    tonic::transport::Error,
>;

/// The generated client over [`GrpcService`].
pub(crate) type GrpcClient = GeneratedClient<GrpcService>;

/// Marker in a request's extensions: cancel it to reset the call.
#[derive(Clone)]
struct CallAbort(CancellationToken);

/// Connect the HTTP/2 channel (TCP, TLS with ALPN `h2`, then h2 or h2c).
pub(crate) async fn channel(
    address: &RelayAddress,
    tls: Option<Tls>,
    config: &ClientConfig,
) -> Result<GrpcService, ProbeFailure> {
    let endpoint = Endpoint::from_shared(address.origin()).map_err(|error| ProbeFailure {
        kind: ProbeFailureKind::Connect,
        detail: format!("invalid relay origin {}: {error}", address.origin()),
    })?;
    let mut endpoint = endpoint.connect_timeout(config.connect_timeout);
    let heartbeat = config.heartbeat;
    if !heartbeat.interval.is_zero() {
        let ack_timeout = if heartbeat.dead_peer_after.is_zero() {
            super::H2_KEEPALIVE_ACK_TIMEOUT
        } else {
            heartbeat.dead_peer_after
        };
        endpoint = endpoint
            .http2_keep_alive_interval(heartbeat.interval)
            .keep_alive_timeout(ack_timeout)
            .keep_alive_while_idle(true);
    }
    let target = address.clone();
    let connect_timeout = config.connect_timeout;
    let connector = tower::service_fn(move |_: http::Uri| {
        let target = target.clone();
        let tls = tls.clone();
        async move {
            connect(&target, tls.as_ref(), ALPN_H2, connect_timeout)
                .await
                .map(TokioIo::new)
                .map_err(ConnectFailed)
        }
    });
    let channel = endpoint
        .connect_with_connector(connector)
        .await
        .map_err(|error| connect_failure(&error))?;

    let prefix = address.prefix().to_owned();
    let service = ServiceBuilder::new()
        .map_request(move |request: http::Request<tonic::body::Body>| prepare(request, &prefix))
        .service(channel);
    Ok(BoxCloneService::new(service))
}

/// Put the prefix in front of the rpc path (tonic's endpoint drops any path)
/// and make the request body abortable.
fn prepare(
    mut request: http::Request<tonic::body::Body>,
    prefix: &str,
) -> http::Request<tonic::body::Body> {
    if !prefix.is_empty() {
        let path = format!("{prefix}{}", request.uri().path());
        let mut parts = request.uri().clone().into_parts();
        if let Ok(path) = path.parse() {
            parts.path_and_query = Some(path);
            if let Ok(uri) = http::Uri::from_parts(parts) {
                *request.uri_mut() = uri;
            }
        }
    }
    match request.extensions().get::<CallAbort>().cloned() {
        Some(CallAbort(token)) => {
            request.map(|body| tonic::body::Body::new(AbortableBody::new(body, token)))
        }
        None => request,
    }
}

/// The connector's own failure if there is one in the chain, else the
/// whole chain as a connect failure.
fn connect_failure(error: &(dyn std::error::Error + 'static)) -> ProbeFailure {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(ConnectFailed(failure)) = current.downcast_ref::<ConnectFailed>() {
            return failure.clone();
        }
        source = current.source();
    }
    ProbeFailure {
        kind: ProbeFailureKind::Connect,
        detail: error_chain(error),
    }
}

/// `error: cause: cause…` on one line.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// A status that the relay itself produced, as opposed to a transport
/// failure tonic reports as a status.
pub(crate) fn is_relay_status(status: &Status) -> bool {
    status.source().is_none() && hop_http_status(status.message()).is_none()
}

/// Tonic's message for an HTTP answer without `grpc-status`.
const MISSING_GRPC_STATUS: &str = "grpc-status header missing, mapped from HTTP status code ";
/// Tonic's suffix when a non-gRPC body failed to decode.
const NON_GRPC_BODY: &str = "while receiving response with status: ";

/// The HTTP status of a non-gRPC answer (from a hop, not the relay), if
/// tonic's message says so.
pub(crate) fn hop_http_status(message: &str) -> Option<String> {
    for marker in [MISSING_GRPC_STATUS, NON_GRPC_BODY] {
        if let Some((_, rest)) = message.split_once(marker) {
            return Some(rest.trim().to_owned());
        }
    }
    None
}

/// Map a call status onto the transport error model.
pub(crate) fn status_to_transport(status: &Status) -> TransportError {
    if is_relay_status(status) {
        TransportError::Status {
            code: relay_error_code_from_grpc(status.code()),
            message: status.message().to_owned(),
        }
    } else {
        TransportError::Transport(format!("gRPC: {}", status_chain(status)))
    }
}

/// A status's message plus its error source chain.
pub(crate) fn status_chain(status: &Status) -> String {
    let mut text = format!("{:?}: {}", status.code(), status.message());
    let mut source = status.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// Start a bidirectional call; `first` is queued before the call starts. A
/// failed call is mapped like a stream error ([`status_to_transport`]).
pub(crate) async fn start<Tx, Rx, F, Fut>(
    service: GrpcService,
    first: Option<Tx>,
    call: F,
) -> Result<GrpcTransport<Tx, Rx>, TransportError>
where
    Tx: Send + 'static,
    Rx: Send + 'static,
    F: FnOnce(GrpcClient, tonic::Request<ReceiverStream<Tx>>) -> Fut,
    Fut: Future<Output = Result<Response<Streaming<Rx>>, Status>>,
{
    let (tx, rx) = mpsc::channel(OUTGOING_CAPACITY);
    if let Some(first) = first {
        let _ = tx.try_send(first);
    }
    let abort = CancellationToken::new();
    let mut request = tonic::Request::new(ReceiverStream::new(rx));
    request.extensions_mut().insert(CallAbort(abort.clone()));
    let guard = abort.clone().drop_guard();
    let response = call(GeneratedClient::new(service), request)
        .await
        .map_err(|status| status_to_transport(&status))?;
    // The call is up; from here the transport decides when to abort.
    let _ = guard.disarm();
    Ok(GrpcTransport {
        sink: PollSender::new(tx),
        stream: Some(response.into_inner()),
        abort,
        closed: false,
        done: false,
    })
}

/// One gRPC call as a relay transport.
///
/// Closing the sink ends the request stream (half-close). Dropping the
/// transport without closing resets the call, so the proxy sees the
/// failure instead of a clean close.
pub(crate) struct GrpcTransport<Tx: Send + 'static, Rx: Send + 'static> {
    sink: PollSender<Tx>,
    /// `None` only while dropping.
    stream: Option<Streaming<Rx>>,
    abort: CancellationToken,
    closed: bool,
    done: bool,
}

impl<Tx: Send + 'static, Rx: Send + 'static> Drop for GrpcTransport<Tx, Rx> {
    fn drop(&mut self) {
        if !self.closed {
            self.abort.cancel();
            return;
        }
        // A cleanly closed call: keep the response open (draining it) until
        // the proxy ends it, so dropping the response does not cut off
        // request frames that are still queued.
        if !self.done
            && let Some(mut stream) = self.stream.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(CLOSE_LINGER, async {
                    while let Some(Ok(_)) = stream.next().await {}
                })
                .await;
            });
        }
    }
}

impl<Tx: Send + 'static, Rx: Send + 'static> Sink<Tx> for GrpcTransport<Tx, Rx> {
    type Error = TransportError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Poll::Ready(Err(TransportError::Closed));
        }
        self.sink
            .poll_reserve(cx)
            .map_err(|_| TransportError::Closed)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Tx) -> Result<(), Self::Error> {
        self.sink
            .send_item(item)
            .map_err(|_| TransportError::Closed)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.closed = true;
        self.sink.close();
        Poll::Ready(Ok(()))
    }
}

impl<Tx: Send + 'static, Rx: Send + 'static> Stream for GrpcTransport<Tx, Rx> {
    type Item = Result<Rx, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(stream).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(message))) => Poll::Ready(Some(Ok(message))),
            Poll::Ready(Some(Err(status))) => {
                self.done = true;
                Poll::Ready(Some(Err(status_to_transport(&status))))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
        }
    }
}

/// A request body that fails (resetting the HTTP/2 stream) once its token
/// is cancelled.
struct AbortableBody {
    inner: tonic::body::Body,
    abort: Pin<Box<WaitForCancellationFutureOwned>>,
    aborted: bool,
}

impl AbortableBody {
    fn new(inner: tonic::body::Body, token: CancellationToken) -> Self {
        Self {
            inner,
            abort: Box::pin(token.cancelled_owned()),
            aborted: false,
        }
    }
}

impl HttpBody for AbortableBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.aborted {
            return Poll::Ready(None);
        }
        if self.abort.as_mut().poll(cx).is_ready() {
            self.aborted = true;
            return Poll::Ready(Some(Err(Status::cancelled("relay stream aborted"))));
        }
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.aborted || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
