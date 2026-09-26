//! The WebSocket binding, client side: one connection per relay stream.

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::connect::{ALPN_HTTP1, Io, Tls, connect};
use super::{ClientConfig, ProbeFailure, ProbeFailureKind};
use crate::link::{RelayAddress, WsRoute};
use crate::proto::RelayErrorCode;
use crate::server::{NOT_FOUND_BODY, WS_CLOSE_ERROR_BASE, duplex::FinalError};
use crate::transport::TransportError;

/// Connect and upgrade `route` (TCP, TLS with ALPN `http/1.1`, HTTP/1.1
/// upgrade).
pub(crate) async fn connect_route(
    address: &RelayAddress,
    tls: Option<&Tls>,
    route: WsRoute,
    config: &ClientConfig,
) -> Result<WebSocketStream<Box<dyn Io>>, ProbeFailure> {
    let io = connect(address, tls, ALPN_HTTP1, config.connect_timeout).await?;
    let url = address.ws_url(route);
    let request = url
        .as_str()
        .into_client_request()
        .map_err(|error| ProbeFailure {
            kind: ProbeFailureKind::Connect,
            detail: format!("invalid WebSocket URL {url}: {error}"),
        })?;
    let handshake = tokio_tungstenite::client_async(request, io);
    match tokio::time::timeout(config.connect_timeout, handshake).await {
        Err(_) => Err(ProbeFailure {
            kind: ProbeFailureKind::Timeout,
            detail: format!(
                "no WebSocket upgrade answer from {url} within {:?}",
                config.connect_timeout
            ),
        }),
        Ok(Err(error)) => Err(upgrade_failure(&url, error)),
        Ok(Ok((socket, _response))) => Ok(socket),
    }
}

fn upgrade_failure(url: &str, error: WsError) -> ProbeFailure {
    match error {
        WsError::Http(response) => {
            let status = response.status();
            let body = response
                .body()
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            if status.as_u16() == 404 && body.trim() == NOT_FOUND_BODY {
                ProbeFailure {
                    kind: ProbeFailureKind::WrongPath,
                    detail: format!(
                        "the relay answered 404 for {url}: the path prefix does not match the relay's --path-prefix"
                    ),
                }
            } else {
                ProbeFailure {
                    kind: ProbeFailureKind::HopRejected,
                    detail: format!(
                        "the WebSocket upgrade to {url} was answered with HTTP {status}"
                    ),
                }
            }
        }
        other => ProbeFailure {
            kind: ProbeFailureKind::Unexpected,
            detail: format!("WebSocket upgrade to {url} failed: {other}"),
        },
    }
}

/// One WebSocket as a relay transport: binary frames carry the protobuf
/// messages; the proxy's final `RelayError` frame (or a `4000 + code`
/// close) becomes [`TransportError::Status`].
pub(crate) struct WsTransport<Tx, Rx> {
    socket: WebSocketStream<Box<dyn Io>>,
    done: bool,
    _types: PhantomData<fn(Tx) -> Rx>,
}

impl<Tx, Rx> WsTransport<Tx, Rx> {
    pub(crate) fn new(socket: WebSocketStream<Box<dyn Io>>) -> Self {
        Self {
            socket,
            done: false,
            _types: PhantomData,
        }
    }
}

fn ws_error(error: &WsError) -> TransportError {
    match error {
        WsError::ConnectionClosed | WsError::AlreadyClosed => TransportError::Closed,
        other => TransportError::Transport(format!("WebSocket: {other}")),
    }
}

impl<Tx: prost::Message, Rx> Sink<Tx> for WsTransport<Tx, Rx> {
    type Error = TransportError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.socket)
            .poll_ready(cx)
            .map_err(|e| ws_error(&e))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Tx) -> Result<(), Self::Error> {
        Pin::new(&mut self.socket)
            .start_send(Message::Binary(item.encode_to_vec()))
            .map_err(|e| ws_error(&e))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.socket)
            .poll_flush(cx)
            .map_err(|e| ws_error(&e))
    }

    /// Sends a close frame: the proxy treats it as the end of this side's
    /// direction and keeps sending until it is done too.
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match Pin::new(&mut self.socket).poll_close(cx) {
            Poll::Ready(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => {
                Poll::Ready(Ok(()))
            }
            other => other.map_err(|e| ws_error(&e)),
        }
    }
}

impl<Tx, Rx> Stream for WsTransport<Tx, Rx>
where
    Rx: prost::Message + Default + FinalError,
{
    type Item = Result<Rx, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            let message = match Pin::new(&mut self.socket).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(message) => message,
            };
            let item = match message {
                Some(Ok(Message::Binary(bytes))) => match Rx::decode(bytes.as_slice()) {
                    Ok(frame) => match frame.into_result() {
                        Ok(frame) => Ok(frame),
                        Err(relay_error) => {
                            self.done = true;
                            Err(TransportError::from(relay_error))
                        }
                    },
                    Err(error) => {
                        self.done = true;
                        Err(TransportError::Decode(error))
                    }
                },
                Some(Ok(Message::Close(frame))) => {
                    self.done = true;
                    match close_error(frame.as_ref()) {
                        Some(error) => Err(error),
                        None => return Poll::Ready(None),
                    }
                }
                Some(Ok(Message::Text(_))) => {
                    self.done = true;
                    Err(TransportError::Transport(
                        "the relay sent a text WebSocket frame".to_owned(),
                    ))
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => {
                    self.done = true;
                    match error {
                        WsError::ConnectionClosed | WsError::AlreadyClosed => {
                            return Poll::Ready(None);
                        }
                        other => Err(TransportError::Transport(format!(
                            "WebSocket connection lost: {other}"
                        ))),
                    }
                }
                None => {
                    self.done = true;
                    Err(TransportError::Transport(
                        "WebSocket connection lost".to_owned(),
                    ))
                }
            };
            return Poll::Ready(Some(item));
        }
    }
}

/// The error a close frame reports, or `None` for a normal close.
fn close_error(frame: Option<&CloseFrame<'_>>) -> Option<TransportError> {
    let frame = frame?;
    let code = u16::from(frame.code);
    if matches!(frame.code, CloseCode::Normal | CloseCode::Away) {
        return None;
    }
    if let Some(relay) = code.checked_sub(WS_CLOSE_ERROR_BASE).filter(|c| *c < 1000) {
        let code = RelayErrorCode::try_from(i32::from(relay)).unwrap_or(RelayErrorCode::Unknown);
        return Some(TransportError::Status {
            code,
            message: frame.reason.to_string(),
        });
    }
    Some(TransportError::Transport(format!(
        "WebSocket closed with code {code}: {}",
        frame.reason
    )))
}
