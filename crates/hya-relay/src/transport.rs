//! The binding-independent relay transport abstraction.
//!
//! Every relay stream — the host control stream and each data stream — is a
//! bidirectional sequence of `hya.relay.v1` messages. The gRPC and WebSocket
//! bindings each adapt their stream type to [`RelayTransport`], so the relay
//! state machines (proxy core, host connector, bridge) are written once.
//!
//! Conventions every implementation follows:
//!
//! - The stream yields `Ok(message)` per received frame, `Err(_)` for a
//!   binding or peer failure, and `None` when the peer closed its sending
//!   direction cleanly.
//! - `poll_close` on the sink ends the local sending direction; the peer's
//!   stream then ends after any frames already sent (half-close).
//! - Sending after the peer went away fails with [`TransportError::Closed`].

use std::pin::Pin;

use futures::{Sink, Stream};

use crate::proto::{Chunk, HostFrame, ProxyToHost, RelayError, RelayErrorCode};

/// Failure on a relay transport.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The peer or the binding closed the stream.
    #[error("relay transport closed")]
    Closed,
    /// The peer ended the stream with a relay error (gRPC status or a final
    /// WebSocket `RelayError` frame).
    #[error("relay peer error {code:?}: {message}")]
    Status {
        /// Failure class.
        code: RelayErrorCode,
        /// Human-readable detail.
        message: String,
    },
    /// A received frame was not a valid protobuf message.
    #[error("relay frame decode failed: {0}")]
    Decode(#[from] prost::DecodeError),
    /// The underlying binding failed (I/O, HTTP/2, WebSocket protocol, …).
    #[error("relay transport failed: {0}")]
    Transport(String),
}

impl From<RelayError> for TransportError {
    fn from(error: RelayError) -> Self {
        TransportError::Status {
            code: error.error_code(),
            message: error.message,
        }
    }
}

/// A bidirectional relay message stream: a [`Sink`] of outgoing `Tx`
/// messages and a [`Stream`] of incoming `Rx` messages.
///
/// Implemented automatically for every type with the right `Sink` and
/// `Stream` implementations.
pub trait RelayTransport<Tx, Rx>:
    Sink<Tx, Error = TransportError> + Stream<Item = Result<Rx, TransportError>> + Send
{
}

impl<T, Tx, Rx> RelayTransport<Tx, Rx> for T where
    T: Sink<Tx, Error = TransportError> + Stream<Item = Result<Rx, TransportError>> + Send
{
}

/// A type-erased, pinned relay transport.
pub type BoxedTransport<Tx, Rx> = Pin<Box<dyn RelayTransport<Tx, Rx>>>;

/// The host connector's end of the control stream.
pub type HostControlTransport = BoxedTransport<HostFrame, ProxyToHost>;

/// The proxy's end of a host control stream.
pub type ProxyControlTransport = BoxedTransport<ProxyToHost, HostFrame>;

/// Either end of a data stream (`Accept` or `Open`).
pub type ChunkTransport = BoxedTransport<Chunk, Chunk>;

pub mod memory {
    //! An in-memory [`RelayTransport`](super::RelayTransport) pair for tests.

    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::channel::mpsc;
    use futures::{Sink, SinkExt, Stream};

    use super::TransportError;

    /// One end of an in-memory transport pair: sends `Tx`, receives `Rx`.
    #[derive(Debug)]
    pub struct MemoryTransport<Tx, Rx> {
        tx: mpsc::Sender<Result<Tx, TransportError>>,
        rx: mpsc::Receiver<Result<Rx, TransportError>>,
    }

    /// Create a connected pair with `capacity` buffered frames per direction.
    ///
    /// The first end sends `A` and receives `B`; the second is the mirror.
    #[must_use]
    pub fn pair<A, B>(capacity: usize) -> (MemoryTransport<A, B>, MemoryTransport<B, A>) {
        let (a_tx, a_rx) = mpsc::channel(capacity);
        let (b_tx, b_rx) = mpsc::channel(capacity);
        (
            MemoryTransport { tx: a_tx, rx: b_rx },
            MemoryTransport { tx: b_tx, rx: a_rx },
        )
    }

    impl<Tx, Rx> MemoryTransport<Tx, Rx> {
        /// Deliver `error` to the peer's stream, simulating a binding failure.
        ///
        /// # Errors
        /// [`TransportError::Closed`] when the peer is gone.
        pub async fn inject_error(mut self, error: TransportError) -> Result<(), TransportError> {
            self.tx
                .send(Err(error))
                .await
                .map_err(|_| TransportError::Closed)
        }
    }

    impl<Tx, Rx> Sink<Tx> for MemoryTransport<Tx, Rx> {
        type Error = TransportError;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.tx.poll_ready(cx).map_err(|_| TransportError::Closed)
        }

        fn start_send(mut self: Pin<&mut Self>, item: Tx) -> Result<(), Self::Error> {
            self.tx
                .start_send(Ok(item))
                .map_err(|_| TransportError::Closed)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.tx)
                .poll_flush(cx)
                .map_err(|_| TransportError::Closed)
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.tx)
                .poll_close(cx)
                .map_err(|_| TransportError::Closed)
        }
    }

    impl<Tx, Rx> Stream for MemoryTransport<Tx, Rx> {
        type Item = Result<Rx, TransportError>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.rx).poll_next(cx)
        }
    }

    impl<Tx, Rx> Unpin for MemoryTransport<Tx, Rx> {}
}
