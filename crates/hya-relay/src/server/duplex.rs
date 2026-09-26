//! The channel-backed [`RelayTransport`](crate::transport::RelayTransport)
//! both bindings hand to the proxy core.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;

use crate::proto::{Chunk, ProxyToHost, RelayError, chunk, proxy_to_host};
use crate::transport::TransportError;

/// Incoming half: whatever the binding decodes from the wire.
pub(crate) type Incoming<Rx> = Pin<Box<dyn Stream<Item = Result<Rx, TransportError>> + Send>>;

/// A transport whose outgoing messages go into a channel the binding
/// drains, and whose incoming messages come from the binding's stream.
///
/// Closing the sink only refuses further sends: the binding ends its wire
/// stream once the core drops the transport, so a half-closed relay stream
/// (a `close{}` frame already sent) keeps receiving until the core is done
/// with it. gRPC cannot half-close a response without ending the call, and
/// a WebSocket close ends both directions.
pub(crate) struct Duplex<Tx: Send + 'static, Rx> {
    sink: PollSender<Tx>,
    closed: bool,
    incoming: Incoming<Rx>,
}

impl<Tx: Send + 'static, Rx> Duplex<Tx, Rx> {
    pub(crate) fn new(outgoing: mpsc::Sender<Tx>, incoming: Incoming<Rx>) -> Self {
        Self {
            sink: PollSender::new(outgoing),
            closed: false,
            incoming,
        }
    }
}

impl<Tx: Send + 'static, Rx> Sink<Tx> for Duplex<Tx, Rx> {
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
        if self.closed {
            return Err(TransportError::Closed);
        }
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
        Poll::Ready(Ok(()))
    }
}

impl<Tx: Send + 'static, Rx> Stream for Duplex<Tx, Rx> {
    type Item = Result<Rx, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.as_mut().poll_next(cx)
    }
}

/// Messages the proxy sends that may carry a final `RelayError`.
pub(crate) trait FinalError: Sized {
    /// The relay error this frame carries, if it is an `error` frame.
    fn final_error(&self) -> Option<&RelayError>;

    /// Split off the relay error of an `error` frame.
    fn into_result(self) -> Result<Self, RelayError>;
}

impl FinalError for ProxyToHost {
    fn final_error(&self) -> Option<&RelayError> {
        match &self.frame {
            Some(proxy_to_host::Frame::Error(error)) => Some(error),
            _ => None,
        }
    }

    fn into_result(self) -> Result<Self, RelayError> {
        match self.frame {
            Some(proxy_to_host::Frame::Error(error)) => Err(error),
            frame => Ok(Self { frame }),
        }
    }
}

impl FinalError for Chunk {
    fn final_error(&self) -> Option<&RelayError> {
        match &self.frame {
            Some(chunk::Frame::Error(error)) => Some(error),
            _ => None,
        }
    }

    fn into_result(self) -> Result<Self, RelayError> {
        match self.frame {
            Some(chunk::Frame::Error(error)) => Err(error),
            frame => Ok(Self { frame }),
        }
    }
}
