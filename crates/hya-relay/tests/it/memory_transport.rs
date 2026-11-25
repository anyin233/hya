#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The in-memory transport pair behaves like a relay binding: ordered
//! delivery both ways, end-of-stream on close or drop, and send failure once
//! the peer is gone.

use futures::{SinkExt, StreamExt};
use hya_relay::proto::{Chunk, HostFrame, ProxyToHost, chunk, host_frame, proxy_to_host};
use hya_relay::transport::{BoxedTransport, RelayTransport, TransportError};

use super::support::memory;

fn data(bytes: &[u8]) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Data(bytes.to_vec())),
    }
}

#[tokio::test]
async fn pair_delivery_asymmetric_half_close_and_boxing() {
    // Symmetric ordered delivery.
    let (mut a, mut b) = memory::pair::<Chunk, Chunk>(4);
    a.send(data(b"one")).await.unwrap();
    a.send(data(b"two")).await.unwrap();
    b.send(data(b"back")).await.unwrap();
    assert_eq!(b.next().await.unwrap().unwrap(), data(b"one"));
    assert_eq!(b.next().await.unwrap().unwrap(), data(b"two"));
    assert_eq!(a.next().await.unwrap().unwrap(), data(b"back"));

    // Asymmetric host/proxy messages.
    let (mut host, mut proxy) = memory::pair::<HostFrame, ProxyToHost>(1);
    proxy
        .send(ProxyToHost {
            frame: Some(proxy_to_host::Frame::Challenge(
                hya_relay::proto::Challenge { nonce: vec![1; 32] },
            )),
        })
        .await
        .unwrap();
    assert!(matches!(
        host.next().await.unwrap().unwrap().frame,
        Some(proxy_to_host::Frame::Challenge(_))
    ));
    host.send(HostFrame {
        frame: Some(host_frame::Frame::Heartbeat(hya_relay::proto::Heartbeat {
            seq: 1,
            pong: false,
        })),
    })
    .await
    .unwrap();
    assert!(matches!(
        proxy.next().await.unwrap().unwrap().frame,
        Some(host_frame::Frame::Heartbeat(_))
    ));

    // Close ends peer stream after buffered frames while preserving half-close.
    let (mut a, mut b) = memory::pair::<Chunk, Chunk>(4);
    a.send(data(b"last")).await.unwrap();
    a.close().await.unwrap();
    assert_eq!(b.next().await.unwrap().unwrap(), data(b"last"));
    assert!(b.next().await.is_none());
    b.send(data(b"reply")).await.unwrap();
    assert_eq!(a.next().await.unwrap().unwrap(), data(b"reply"));

    // Boxing behind RelayTransport.
    fn boxed<T: RelayTransport<Chunk, Chunk> + 'static>(t: T) -> BoxedTransport<Chunk, Chunk> {
        Box::pin(t)
    }
    let (a, b) = memory::pair::<Chunk, Chunk>(1);
    let (mut a, mut b) = (boxed(a), boxed(b));
    a.send(data(b"boxed")).await.unwrap();
    assert_eq!(b.next().await.unwrap().unwrap(), data(b"boxed"));
}

#[tokio::test]
async fn pair_peer_drop_and_error_injection() {
    let (mut a, b) = memory::pair::<Chunk, Chunk>(4);
    drop(b);
    let error = a.send(data(b"x")).await.unwrap_err();
    assert!(matches!(error, TransportError::Closed));
    assert!(a.next().await.is_none());

    let (a, mut b) = memory::pair::<Chunk, Chunk>(4);
    a.inject_error(TransportError::Transport("hop reset".into()))
        .await
        .unwrap();
    let error = b.next().await.unwrap().unwrap_err();
    assert!(matches!(error, TransportError::Transport(message) if message == "hop reset"));
}
