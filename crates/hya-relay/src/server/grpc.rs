//! The gRPC binding: the tonic `hya.relay.v1.Relay` service.

use std::pin::Pin;

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use super::duplex::{Duplex, FinalError};
use super::peer_of;
use crate::proto::relay_server::Relay;
use crate::proto::{Chunk, HostFrame, ProxyToHost};
use crate::proxy::ProxyCore;
use crate::transport::{BoxedTransport, TransportError};

/// Outgoing frames buffered per stream before the core waits.
const OUTGOING_CAPACITY: usize = 16;

/// A response stream of relay messages; a final error frame is the status.
pub(crate) type OutStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// The tonic service handing every call to the proxy core.
pub(crate) struct GrpcRelay {
    core: ProxyCore,
}

impl GrpcRelay {
    pub(crate) fn new(core: ProxyCore) -> Self {
        Self { core }
    }
}

/// Adapt a call's request stream plus a response channel into a transport.
fn adapt<Tx, Rx>(requests: Streaming<Rx>) -> (BoxedTransport<Tx, Rx>, OutStream<Tx>)
where
    Tx: FinalError + Send + 'static,
    Rx: Send + 'static,
{
    let incoming = requests.map(|item| {
        // A request stream fails only on transport problems (reset, cancel,
        // decode): clients cannot send a status.
        item.map_err(|status| TransportError::Transport(format!("gRPC request stream: {status}")))
    });
    let (tx, rx) = mpsc::channel(OUTGOING_CAPACITY);
    let transport = Duplex::new(tx, Box::pin(incoming));
    let responses = ReceiverStream::new(rx).map(to_response);
    (Box::pin(transport), Box::pin(responses))
}

/// A final error frame ends the call with that status.
#[allow(clippy::result_large_err)] // tonic fixes the stream item type
fn to_response<Tx: FinalError>(message: Tx) -> Result<Tx, Status> {
    message.into_result().map_err(Status::from)
}

#[tonic::async_trait]
impl Relay for GrpcRelay {
    type HostStream = OutStream<ProxyToHost>;
    type AcceptStream = OutStream<Chunk>;
    type OpenStream = OutStream<Chunk>;

    async fn host(
        &self,
        request: Request<Streaming<HostFrame>>,
    ) -> Result<Response<Self::HostStream>, Status> {
        let peer = peer_of(request.extensions());
        let (transport, responses) = adapt(request.into_inner());
        tokio::spawn(self.core.serve_host(transport, peer));
        Ok(Response::new(responses))
    }

    async fn accept(
        &self,
        request: Request<Streaming<Chunk>>,
    ) -> Result<Response<Self::AcceptStream>, Status> {
        let peer = peer_of(request.extensions());
        let (transport, responses) = adapt(request.into_inner());
        tokio::spawn(self.core.serve_accept(transport, peer));
        Ok(Response::new(responses))
    }

    async fn open(
        &self,
        request: Request<Streaming<Chunk>>,
    ) -> Result<Response<Self::OpenStream>, Status> {
        let peer = peer_of(request.extensions());
        let (transport, responses) = adapt(request.into_inner());
        tokio::spawn(self.core.serve_open(transport, peer));
        Ok(Response::new(responses))
    }
}
