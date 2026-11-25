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

use crate::proto::{
    Chunk, HostFrame, ProxyToHost, RelayError, RelayErrorCode, chunk, proxy_to_host,
};

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
