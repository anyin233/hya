//! An in-memory relay transport pair for tests.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::channel::mpsc;
use futures::{Sink, SinkExt, Stream};

use hya_relay::transport::TransportError;

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

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tx.poll_ready(cx).map_err(|_| TransportError::Closed)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Tx) -> Result<(), Self::Error> {
        self.tx
            .start_send(Ok(item))
            .map_err(|_| TransportError::Closed)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.tx)
            .poll_flush(cx)
            .map_err(|_| TransportError::Closed)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
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
