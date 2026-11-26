#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The relay client: heartbeats and dead-peer detection (in memory), the
//! reconnect policy, and the client bindings against a real relay server.

use crate::support;

use std::time::Duration;

use super::support::memory::{self, MemoryTransport};
use futures::{SinkExt, StreamExt};
use hya_relay::client::{
    Backoff, Binding, ChoiceReason, ClientConfig, ClientError, HeartbeatConfig, ProbeFailureKind,
    ReconnectPolicy, RelayClient, RetryKind, with_heartbeat,
};
use hya_relay::link::{RelayAddress, RoomId, Transport};
use hya_relay::proto::{Chunk, Close, Heartbeat, RelayErrorCode, chunk};
use hya_relay::transport::{ChunkTransport, TransportError};
use tokio::time::{Instant, timeout};

use support::{Relay, TestTls};

type Peer = MemoryTransport<Chunk, Chunk>;

fn data(bytes: &[u8]) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Data(bytes.to_vec())),
    }
}

fn close() -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Close(Close {})),
    }
}

fn probe(seq: u64) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })),
    }
}

fn pong(seq: u64) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: true })),
    }
}

const SECOND: Duration = Duration::from_secs(1);

fn wrapped() -> (ChunkTransport, Peer) {
    wrapped_with(HeartbeatConfig::every(10 * SECOND))
}

fn wrapped_with(config: HeartbeatConfig) -> (ChunkTransport, Peer) {
    let (ours, peer) = memory::pair::<Chunk, Chunk>(16);
    (with_heartbeat(Box::pin(ours), config), peer)
}

async fn peer_recv(peer: &mut Peer) -> Chunk {
    timeout(600 * SECOND, peer.next())
        .await
        .expect("peer frame in time")
        .expect("peer stream open")
        .expect("peer frame ok")
}

fn is_probe(frame: &Chunk) -> bool {
    matches!(
        frame.frame,
        Some(chunk::Frame::Heartbeat(Heartbeat { pong: false, .. }))
    )
}

// ---- heartbeats ----

#[tokio::test(start_paused = true)]
async fn heartbeat_probing_suppression_one_way_upload_and_peer_probes() {
    // Idle leg sends probes every interval.
    let (_ours, mut peer) = wrapped();
    let start = Instant::now();
    assert_eq!(peer_recv(&mut peer).await, probe(1));
    assert!(start.elapsed() >= 10 * SECOND);
    peer.send(pong(1)).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, probe(2));
    assert!(start.elapsed() >= 20 * SECOND);

    // Bidirectional traffic suppresses probes.
    let (mut ours, mut peer) = wrapped();
    for i in 0..10u8 {
        tokio::time::sleep(5 * SECOND).await;
        ours.send(data(&[i])).await.unwrap();
        peer.send(data(&[i])).await.unwrap();
        assert_eq!(peer_recv(&mut peer).await, data(&[i]));
        assert_eq!(ours.next().await.unwrap().unwrap(), data(&[i]));
    }

    // One-way upload still probes silent peer; answered probes keep leg alive.
    let (mut ours, mut peer) = wrapped();
    let mut probes = 0;
    for i in 0..12u8 {
        tokio::time::sleep(5 * SECOND).await;
        ours.send(data(&[i])).await.unwrap();
        loop {
            let frame = peer_recv(&mut peer).await;
            if frame == data(&[i]) {
                break;
            }
            assert!(is_probe(&frame), "unexpected {frame:?}");
            probes += 1;
            if let Some(chunk::Frame::Heartbeat(Heartbeat { seq, .. })) = frame.frame {
                peer.send(pong(seq)).await.unwrap();
            }
        }
    }
    assert!(probes >= 4, "only {probes} probes in 60 s");
    assert!(
        timeout(SECOND, ours.next()).await.is_err(),
        "leg failed although peer answered"
    );

    // Probes from peer are answered and pongs swallowed.
    let (mut ours, mut peer) = wrapped();
    peer.send(probe(7)).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, pong(7));
    peer.send(pong(3)).await.unwrap();
    peer.send(data(b"x")).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), data(b"x"));
}

#[tokio::test(start_paused = true)]
async fn heartbeat_dead_peer_detection_and_configuration() {
    let (mut ours, mut peer) = wrapped();
    let start = Instant::now();
    let item = timeout(600 * SECOND, ours.next())
        .await
        .expect("fails in time");
    assert!(
        matches!(item, Some(Err(TransportError::Transport(_)))),
        "got {item:?}"
    );
    assert!(start.elapsed() >= 30 * SECOND && start.elapsed() < 40 * SECOND);
    loop {
        match timeout(600 * SECOND, peer.next()).await.expect("ends") {
            Some(Ok(frame)) => assert!(is_probe(&frame)),
            None => break,
            Some(Err(error)) => panic!("unexpected {error:?}"),
        }
    }
    assert!(ours.send(data(b"late")).await.is_err());

    // Dead-peer detection disabled vs custom threshold.
    let (mut ours, _peer) =
        wrapped_with(HeartbeatConfig::every(10 * SECOND).dead_peer_after(Duration::ZERO));
    assert!(timeout(300 * SECOND, ours.next()).await.is_err());
    let (mut ours, _peer) =
        wrapped_with(HeartbeatConfig::every(10 * SECOND).dead_peer_after(SECOND * 12));
    let start = Instant::now();
    assert!(matches!(ours.next().await, Some(Err(_))));
    assert!(start.elapsed() < 15 * SECOND);
}

#[tokio::test(start_paused = true)]
async fn heartbeat_half_close_and_failed_pong_resilience() {
    // Peer closed its direction: no probes, local direction stays open.
    let (mut ours, mut peer) = wrapped();
    peer.send(close()).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), close());
    assert!(timeout(300 * SECOND, peer.next()).await.is_err());
    assert!(timeout(SECOND, ours.next()).await.is_err());
    ours.send(data(b"after")).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, data(b"after"));

    // Local side closed its direction: no probes, peer direction still works.
    let (mut ours, mut peer) = wrapped();
    ours.send(close()).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, close());
    assert!(timeout(300 * SECOND, peer.next()).await.is_err());
    peer.send(probe(1)).await.unwrap();
    peer.send(data(b"reply")).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), data(b"reply"));

    // Closing the sink prevents pongs without ending the peer->local direction.
    let (mut ours, mut peer) = wrapped();
    ours.close().await.unwrap();
    assert!(timeout(SECOND, peer.next()).await.unwrap().is_none());
    peer.send(probe(1)).await.unwrap();
    peer.send(data(b"still")).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), data(b"still"));
}

#[tokio::test(start_paused = true)]
async fn queued_frames_are_delivered_after_drop() {
    let (mut ours, mut peer) = wrapped();
    ours.send(data(b"a")).await.unwrap();
    ours.send(close()).await.unwrap();
    drop(ours);
    assert_eq!(peer_recv(&mut peer).await, data(b"a"));
    assert_eq!(peer_recv(&mut peer).await, close());
    assert!(timeout(SECOND, peer.next()).await.unwrap().is_none());
}

// ---- reconnect policy ----

#[test]
fn backoff_growth_jitter_and_conflict_classification() {
    let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), 7);
    for attempt in 0..12u32 {
        let ceiling = (SECOND * 2u32.pow(attempt.min(10))).min(60 * SECOND);
        let delay = backoff.next_delay(RetryKind::Normal);
        assert!(
            delay >= ceiling / 2 && delay <= ceiling,
            "{attempt}: {delay:?}"
        );
    }

    let delays = |seed| {
        let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), seed);
        (0..6)
            .map(|_| backoff.next_delay(RetryKind::Normal))
            .collect::<Vec<_>>()
    };
    assert_ne!(delays(1), delays(2));
    assert_eq!(delays(3), delays(3));

    let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), 1);
    let first = backoff.next_delay(RetryKind::Conflict);
    assert!(first >= 15 * SECOND && first <= 30 * SECOND, "{first:?}");
    for _ in 0..10 {
        assert!(backoff.next_delay(RetryKind::Conflict) <= 600 * SECOND);
    }
    let error = ClientError::Relay {
        code: RelayErrorCode::AlreadyExists,
        message: "room was registered by a newer control stream".into(),
    };
    assert_eq!(RetryKind::of_client_error(&error), RetryKind::Conflict);
    let status = TransportError::Status {
        code: RelayErrorCode::AlreadyExists,
        message: String::new(),
    };
    assert_eq!(RetryKind::of_transport_error(&status), RetryKind::Conflict);
    assert_eq!(
        RetryKind::of_transport_error(&TransportError::Closed),
        RetryKind::Normal
    );
}

#[tokio::test(start_paused = true)]
async fn a_stable_connection_resets_the_backoff() {
    let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), 1);
    for _ in 0..6 {
        backoff.next_delay(RetryKind::Normal);
    }
    backoff.connected();
    tokio::time::advance(5 * SECOND).await;
    assert!(backoff.next_delay(RetryKind::Normal) >= 30 * SECOND);
    backoff.connected();
    tokio::time::advance(61 * SECOND).await;
    assert!(backoff.next_delay(RetryKind::Normal) <= SECOND);
}

// ---- bindings against a real relay ----

fn config(transport: Transport) -> ClientConfig {
    ClientConfig {
        transport,
        probe_timeout: 5 * SECOND,
        connect_timeout: 5 * SECOND,
        open_timeout: 5 * SECOND,
        ..ClientConfig::default()
    }
}

fn token_of(key: &ed25519_dalek::SigningKey) -> ([u8; 32], hya_relay::keys::OpenToken) {
    let room = RoomId::from_ed25519(key.verifying_key().as_bytes());
    let token = hya_relay::keys::OpenToken::derive(
        &hya_relay::keys::Psk::from_bytes(key.to_bytes()),
        &room,
    );
    (token.hash(), token)
}

fn offline_room() -> RoomId {
    RoomId::from_ed25519(&[42; 32])
}

async fn next(open: &mut ChunkTransport) -> Option<Result<Chunk, TransportError>> {
    timeout(5 * SECOND, open.next()).await.unwrap()
}

#[tokio::test]
async fn binding_selection_pinned_and_auto_cache() {
    let relay = Relay::start(support::relay_config()).await;

    let pinned = RelayClient::new(relay.address(), config(Transport::Ws)).unwrap();
    let choice = pinned.binding().await.unwrap();
    assert_eq!(choice.binding, Binding::Ws);
    assert_eq!(choice.reason, ChoiceReason::Pinned);
    assert_eq!(pinned.remembered_binding(), None);

    let auto = RelayClient::new(relay.address(), config(Transport::Auto)).unwrap();
    let choice = auto.binding().await.unwrap();
    assert_eq!(choice.binding, Binding::Grpc);
    assert_eq!(choice.reason, ChoiceReason::GrpcWorks);
    let other = RelayClient::new(relay.address(), config(Transport::Auto)).unwrap();
    assert_eq!(other.remembered_binding(), Some(choice));
    other.forget_binding();
    assert_eq!(auto.remembered_binding(), None);

    relay.stop().await;
}

#[tokio::test]
async fn offline_open_and_unknown_accept_errors_on_both_bindings() {
    let relay = Relay::start(support::relay_config()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let client = RelayClient::new(relay.address(), config(transport)).unwrap();
        let Err(error) = client.open(&offline_room()).await else {
            panic!("{transport}: open to an offline room succeeded");
        };
        assert!(
            matches!(error, ClientError::RoomOffline(_)),
            "{transport}: {error:?}"
        );
        assert_eq!(error.code(), Some(RelayErrorCode::NotFound));

        let mut leg = client
            .accept("00000000000000000000000000000000")
            .await
            .unwrap();
        let item = timeout(5 * SECOND, leg.next()).await.unwrap();
        assert!(
            matches!(
                item,
                Some(Err(TransportError::Status {
                    code: RelayErrorCode::NotFound,
                    ..
                }))
            ),
            "{transport}: {item:?}"
        );
    }
    relay.stop().await;
}

#[tokio::test]
async fn a_wrong_prefix_is_reported_as_such_on_both_bindings() {
    let relay = Relay::start(support::relay_config().path_prefix("/relay").unwrap()).await;
    let address =
        RelayAddress::new(false, "127.0.0.1", Some(relay.addr.port()), "/elsewhere").unwrap();
    let client = RelayClient::new(address, config(Transport::Auto)).unwrap();
    for binding in [Binding::Grpc, Binding::Ws] {
        let failure = client.probe(binding).await.unwrap_err();
        assert_eq!(
            failure.kind,
            ProbeFailureKind::WrongPath,
            "{binding}: {failure}"
        );
    }
    match client.binding().await.unwrap_err() {
        ClientError::NoBinding { grpc, ws } => {
            assert_eq!(grpc.kind, ProbeFailureKind::WrongPath);
            assert_eq!(ws.kind, ProbeFailureKind::WrongPath);
        }
        other => panic!("expected NoBinding, got {other:?}"),
    }
    relay.stop().await;
}

#[tokio::test]
async fn tls_untrusted_cert_and_unreadable_ca_errors() {
    let bad_ca_addr = RelayAddress::new(true, "localhost", Some(1), "").unwrap();
    let error = RelayClient::new(
        bad_ca_addr,
        ClientConfig {
            extra_ca_pem: Some("/nonexistent/hya-relay-ca.pem".into()),
            ..ClientConfig::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ClientError::Config(_)), "{error:?}");

    let tls = TestTls::new();
    let relay = Relay::start(support::relay_config().tls(tls.files.clone())).await;
    let address = RelayAddress::new(true, "localhost", Some(relay.addr.port()), "").unwrap();
    let client = RelayClient::new(address, config(Transport::Auto)).unwrap();
    for binding in [Binding::Grpc, Binding::Ws] {
        let failure = client.probe(binding).await.unwrap_err();
        assert_eq!(failure.kind, ProbeFailureKind::Tls, "{binding}: {failure}");
    }
    relay.stop().await;
}

#[tokio::test]
async fn stream_close_delivery_and_unclosed_drop_abort_on_both_bindings() {
    use hya_relay::client::register_host;
    use hya_relay::proto::proxy_to_host;
    let relay = Relay::start(support::relay_config()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        // 1. Frames sent right before drop are delivered.
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let host = RelayClient::new(relay.address(), config(transport)).unwrap();
        let mut control = host.host().await.unwrap();
        let (hash, token) = token_of(&key);
        let room = register_host(&mut control, &key, &hash, 5 * SECOND)
            .await
            .unwrap()
            .room()
            .clone();
        let opener = host.clone();
        let opening =
            tokio::spawn(async move { opener.open_with_token(&room, &token).await.unwrap() });
        let stream_id = loop {
            let frame = timeout(5 * SECOND, control.next()).await.unwrap();
            if let Some(Ok(hya_relay::proto::ProxyToHost {
                frame: Some(proxy_to_host::Frame::Incoming(incoming)),
            })) = frame
            {
                break incoming.stream_id;
            }
        };
        let mut leg = host.accept(&stream_id).await.unwrap();
        let mut open = opening.await.unwrap();
        open.send(data(b"question")).await.unwrap();
        open.send(close()).await.unwrap();
        assert_eq!(leg.next().await.unwrap().unwrap(), data(b"question"));
        assert_eq!(leg.next().await.unwrap().unwrap(), close());
        leg.send(data(b"answer")).await.unwrap();
        leg.send(close()).await.unwrap();
        drop(leg);
        assert_eq!(
            next(&mut open).await.unwrap().unwrap(),
            data(b"answer"),
            "{transport}"
        );
        assert_eq!(
            next(&mut open).await.unwrap().unwrap(),
            close(),
            "{transport}"
        );
        assert!(next(&mut open).await.is_none(), "{transport}");

        // 2. Dropping an unclosed stream aborts only that stream without affecting control.
        let opener = host.clone();
        let room2 = RoomId::from_ed25519(key.verifying_key().as_bytes());
        let token2 = token_of(&key).1;
        let opening =
            tokio::spawn(async move { opener.open_with_token(&room2, &token2).await.unwrap() });
        let stream_id = loop {
            let frame = timeout(5 * SECOND, control.next()).await.unwrap();
            if let Some(Ok(hya_relay::proto::ProxyToHost {
                frame: Some(proxy_to_host::Frame::Incoming(incoming)),
            })) = frame
            {
                break incoming.stream_id;
            }
        };
        let mut leg = host.accept(&stream_id).await.unwrap();
        let mut open = opening.await.unwrap();
        open.send(data(b"partial")).await.unwrap();
        assert_eq!(next(&mut leg).await.unwrap().unwrap(), data(b"partial"));
        drop(open);
        let item = next(&mut leg).await;
        assert!(
            matches!(
                item,
                Some(Err(TransportError::Status {
                    code: RelayErrorCode::Unavailable,
                    ..
                }))
            ),
            "{transport}: {item:?}"
        );
        let offline = RelayClient::new(relay.address(), config(transport)).unwrap();
        assert!(matches!(
            offline.open(&offline_room()).await,
            Err(ClientError::RoomOffline(_))
        ));
        let again = host.clone();
        let token3 = token_of(&key).1;
        let reopened = tokio::spawn(async move {
            again
                .open_with_token(
                    &RoomId::from_ed25519(key.verifying_key().as_bytes()),
                    &token3,
                )
                .await
        });
        assert!(
            matches!(
                timeout(5 * SECOND, control.next()).await.unwrap(),
                Some(Ok(hya_relay::proto::ProxyToHost {
                    frame: Some(proxy_to_host::Frame::Incoming(_)),
                }))
            ),
            "{transport}: control stream still serves opens"
        );
        reopened.abort();
    }
    relay.stop().await;
}

#[tokio::test]
async fn only_link_holders_get_past_the_proxy_on_both_bindings() {
    use hya_relay::keys::Psk;
    use hya_relay::link::RelayLink;
    let relay = Relay::start(support::relay_config()).await;
    let host = RelayClient::new(relay.address(), config(Transport::Grpc)).unwrap();
    let backend =
        support::Backend::start(host, support::Identity::new(31), support::quick_policy()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let link = backend.identity.link(relay.address(), transport);
        let bare = RelayClient::new(relay.address(), config(transport)).unwrap();
        assert!(
            matches!(
                bare.open(link.room_id()).await,
                Err(ClientError::RoomOffline(_))
            ),
            "{transport}"
        );
        let wrong = RelayLink::from_keys(
            link.address().clone(),
            link.room_id().clone(),
            transport,
            *link.server_key(),
            &Psk::from_bytes([1; 32]),
        );
        let wrong_client = RelayClient::from_link(&wrong, config(transport)).unwrap();
        assert!(
            matches!(
                wrong_client.open(wrong.room_id()).await,
                Err(ClientError::RoomOffline(_))
            ),
            "{transport}"
        );
        let holder = RelayClient::from_link(&link, config(transport)).unwrap();
        support::round_trip(&holder, &link, b"link holder").await;
    }
    relay.stop().await;
}

#[tokio::test]
async fn the_client_refuses_oversized_relay_messages_on_both_bindings() {
    use hya_relay::client::{MAX_CLIENT_MESSAGE_SIZE, register_host};
    use hya_relay::proto::proxy_to_host;
    let relay = Relay::start(
        support::relay_config().limits(hya_relay::proxy::ProxyLimits {
            max_chunk_data: 4 * MAX_CLIENT_MESSAGE_SIZE,
            stream_rate_bytes_per_sec: 0,
            ..hya_relay::proxy::ProxyLimits::default()
        }),
    )
    .await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let (hash, token) = token_of(&key);
        let host = RelayClient::new(relay.address(), config(transport)).unwrap();
        let mut control = host.host().await.unwrap();
        let room = register_host(&mut control, &key, &hash, 5 * SECOND)
            .await
            .unwrap()
            .room()
            .clone();
        let opener = RelayClient::new(relay.address(), config(transport)).unwrap();
        let opening = tokio::spawn(async move { opener.open_with_token(&room, &token).await });
        let stream_id = loop {
            let frame = timeout(5 * SECOND, control.next()).await.unwrap();
            if let Some(Ok(hya_relay::proto::ProxyToHost {
                frame: Some(proxy_to_host::Frame::Incoming(incoming)),
            })) = frame
            {
                break incoming.stream_id;
            }
        };
        let mut leg = host.accept(&stream_id).await.unwrap();
        let mut open = opening.await.unwrap().unwrap();
        let record = vec![1u8; hya_relay::tunnel::MAX_NOISE_MESSAGE];
        leg.send(data(&record)).await.unwrap();
        assert_eq!(
            next(&mut open).await.unwrap().unwrap(),
            data(&record),
            "{transport}"
        );
        leg.send(data(&vec![2u8; 2 * MAX_CLIENT_MESSAGE_SIZE]))
            .await
            .unwrap();
        let item = next(&mut open).await;
        assert!(matches!(item, Some(Err(_)) | None), "{transport}: {item:?}");
    }
    relay.stop().await;
}
