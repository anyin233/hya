//! The WebSocket binding: `GET <prefix>/hya.relay.v1/ws/{host,accept,open}`.
//!
//! One protobuf-encoded message per binary frame in both directions. The
//! proxy ends a stream with a WebSocket close: `1000` after a clean end,
//! [`WS_CLOSE_ERROR_BASE`]` + code` after a final `RelayError` frame, and
//! [`WS_CLOSE_UNSUPPORTED_DATA`] when the client sent a text frame.

use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::Response;
use axum::routing::get;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::duplex::{Duplex, FinalError};
use super::{WS_CLOSE_ERROR_BASE, WS_CLOSE_UNSUPPORTED_DATA};
use crate::link::WsRoute;
use crate::proto::{Chunk, HostFrame, ProxyToHost, RelayErrorCode};
use crate::proxy::{PeerInfo, ProxyCore, ProxyLimits};
use crate::transport::{BoxedTransport, TransportError};

/// Frames buffered per direction between the socket and the core.
const CAPACITY: usize = 16;
/// How long to wait for the client's close reply after our close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
/// Largest WebSocket close reason (bytes).
const MAX_CLOSE_REASON: usize = 123;
/// Frame size allowance over `max_chunk_data` for the protobuf envelope.
const FRAME_OVERHEAD: usize = 1024;

#[derive(Clone)]
pub(crate) struct WsState {
    pub(crate) core: ProxyCore,
    pub(crate) tasks: TaskTracker,
    pub(crate) hard_stop: CancellationToken,
    pub(crate) max_message_size: usize,
}

/// Largest accepted WebSocket message: one `Chunk` with a maximal `data`.
pub(crate) fn max_message_size(limits: &ProxyLimits) -> usize {
    limits.max_chunk_data.saturating_add(FRAME_OVERHEAD)
}

/// The three upgrade routes under `prefix`.
pub(crate) fn router(prefix: &str, state: WsState) -> Router {
    let path = |route: WsRoute| format!("{prefix}/hya.relay.v1/ws/{}", route.as_str());
    Router::new()
        .route(&path(WsRoute::Host), get(host))
        .route(&path(WsRoute::Accept), get(accept))
        .route(&path(WsRoute::Open), get(open))
        .with_state(state)
}

async fn host(
    State(state): State<WsState>,
    Extension(peer): Extension<PeerInfo>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let core = state.core.clone();
    upgrade_with(
        upgrade,
        state,
        move |transport: BoxedTransport<ProxyToHost, HostFrame>| core.serve_host(transport, peer),
    )
}

async fn accept(
    State(state): State<WsState>,
    Extension(peer): Extension<PeerInfo>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let core = state.core.clone();
    upgrade_with(
        upgrade,
        state,
        move |transport: BoxedTransport<Chunk, Chunk>| core.serve_accept(transport, peer),
    )
}

async fn open(
    State(state): State<WsState>,
    Extension(peer): Extension<PeerInfo>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let core = state.core.clone();
    upgrade_with(
        upgrade,
        state,
        move |transport: BoxedTransport<Chunk, Chunk>| core.serve_open(transport, peer),
    )
}

fn upgrade_with<Tx, Rx, F, Fut>(upgrade: WebSocketUpgrade, state: WsState, serve: F) -> Response
where
    Tx: prost::Message + FinalError + Send + 'static,
    Rx: prost::Message + Default + Send + 'static,
    F: FnOnce(BoxedTransport<Tx, Rx>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    upgrade
        .max_message_size(state.max_message_size)
        .max_frame_size(state.max_message_size)
        .on_upgrade(move |socket| {
            let tasks = state.tasks.clone();
            tasks.track_future(run(socket, state.hard_stop, serve))
        })
}

/// Bridge one WebSocket to a core transport until both are done.
async fn run<Tx, Rx, F, Fut>(socket: WebSocket, hard_stop: CancellationToken, serve: F)
where
    Tx: prost::Message + FinalError + Send + 'static,
    Rx: prost::Message + Default + Send + 'static,
    F: FnOnce(BoxedTransport<Tx, Rx>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (ws_tx, mut ws_rx) = socket.split();
    let (out_tx, out_rx) = mpsc::channel::<Tx>(CAPACITY);
    let (in_tx, in_rx) = mpsc::channel::<Result<Rx, TransportError>>(CAPACITY);
    let text_received = Arc::new(AtomicBool::new(false));
    let transport = Duplex::new(out_tx, Box::pin(ReceiverStream::new(in_rx)));
    tokio::spawn(serve(Box::pin(transport)));
    let mut writer = tokio::spawn(write_loop(ws_tx, out_rx, text_received.clone()));

    // Reader: frames go to the core until the client closes or breaks the
    // protocol; the socket stays open until the writer has sent its close.
    let mut to_core = Some(in_tx);
    loop {
        tokio::select! {
            biased;
            () = hard_stop.cancelled() => {
                writer.abort();
                return;
            }
            _ = &mut writer => break,
            message = ws_rx.next(), if to_core.is_some() => {
                let item = match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        Rx::decode(bytes.as_slice()).map_err(TransportError::from)
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Text(_))) => {
                        text_received.store(true, Ordering::SeqCst);
                        Err(TransportError::Transport(
                            "text WebSocket frames are not part of the relay protocol".to_owned(),
                        ))
                    }
                    // The client ended the stream.
                    Some(Ok(Message::Close(_))) => {
                        to_core = None;
                        continue;
                    }
                    // The connection ended without a closing handshake.
                    None => Err(TransportError::Transport(
                        "WebSocket connection lost".to_owned(),
                    )),
                    Some(Err(error)) => Err(TransportError::Transport(error.to_string())),
                };
                let failed = item.is_err();
                let delivered = match &to_core {
                    Some(tx) => tx.send(item).await.is_ok(),
                    None => false,
                };
                if failed || !delivered {
                    to_core = None;
                }
            }
        }
    }

    // Our close frame is out; give the client a moment to answer it.
    let _ = timeout(CLOSE_GRACE, async {
        tokio::select! {
            () = hard_stop.cancelled() => {}
            () = async { while let Some(Ok(_)) = ws_rx.next().await {} } => {}
        }
    })
    .await;
}

/// Encode core messages as binary frames; when the core drops the
/// transport, close the socket with the code matching how it ended.
async fn write_loop<Tx>(
    mut ws_tx: SplitSink<WebSocket, Message>,
    mut out_rx: mpsc::Receiver<Tx>,
    text_received: Arc<AtomicBool>,
) where
    Tx: prost::Message + FinalError,
{
    let mut close = CloseFrame {
        code: 1000,
        reason: Cow::Borrowed(""),
    };
    while let Some(message) = out_rx.recv().await {
        if let Some(error) = message.final_error() {
            close = error_close(error.error_code(), &error.message);
        }
        if ws_tx
            .send(Message::Binary(message.encode_to_vec()))
            .await
            .is_err()
        {
            return;
        }
    }
    if text_received.load(Ordering::SeqCst) {
        close = CloseFrame {
            code: WS_CLOSE_UNSUPPORTED_DATA,
            reason: Cow::Borrowed("binary frames only"),
        };
    }
    let _ = ws_tx.send(Message::Close(Some(close))).await;
}

/// The close frame after a final relay error: `4000 + code`.
fn error_close(code: RelayErrorCode, message: &str) -> CloseFrame<'static> {
    let code = match code {
        RelayErrorCode::Unspecified => RelayErrorCode::Unknown,
        code => code,
    };
    let mut end = message.len().min(MAX_CLOSE_REASON);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    CloseFrame {
        code: WS_CLOSE_ERROR_BASE + code as u16,
        reason: Cow::Owned(message[..end].to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_close_codes_and_reasons() {
        let close = error_close(RelayErrorCode::NotFound, "room is offline");
        assert_eq!(close.code, 4005);
        assert_eq!(close.reason, "room is offline");
        assert_eq!(error_close(RelayErrorCode::Unspecified, "").code, 4002);
        let long = "é".repeat(100);
        let close = error_close(RelayErrorCode::Unavailable, &long);
        assert_eq!(close.code, 4014);
        assert!(close.reason.len() <= MAX_CLOSE_REASON);
    }
}
