#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The relay client: heartbeats and dead-peer detection (in memory), the
//! reconnect policy, and the client bindings against a real relay server
//! (typed open errors, pinned and negotiated bindings, prefix and TLS
//! failures).

mod support;

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use hya_relay::client::{
    Backoff, Binding, ChoiceReason, ClientConfig, ClientError, HeartbeatConfig, ProbeFailureKind,
    ReconnectPolicy, RelayClient, RetryKind, with_heartbeat,
};
use hya_relay::link::{RelayAddress, RoomId, Transport};
use hya_relay::proto::{Chunk, Close, Heartbeat, RelayErrorCode, chunk};
use hya_relay::transport::memory::{self, MemoryTransport};
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

/// A heartbeat-wrapped end (probe every 10 s, dead after 30 s) and its
/// in-memory peer.
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
async fn an_idle_leg_sends_probes_every_interval() {
    let (_ours, mut peer) = wrapped();
    let start = Instant::now();
    assert_eq!(peer_recv(&mut peer).await, probe(1));
    assert!(start.elapsed() >= 10 * SECOND);
    peer.send(pong(1)).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, probe(2));
    assert!(start.elapsed() >= 20 * SECOND);
}

#[tokio::test(start_paused = true)]
async fn traffic_both_ways_suppresses_probes() {
    let (mut ours, mut peer) = wrapped();
    for i in 0..10u8 {
        tokio::time::sleep(5 * SECOND).await;
        ours.send(data(&[i])).await.unwrap();
        peer.send(data(&[i])).await.unwrap();
        assert_eq!(peer_recv(&mut peer).await, data(&[i]));
        assert_eq!(ours.next().await.unwrap().unwrap(), data(&[i]));
    }
}

#[tokio::test(start_paused = true)]
async fn a_one_way_upload_still_probes_the_silent_peer() {
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
    // Answered probes keep the leg alive: no dead-peer failure.
    assert!(
        timeout(SECOND, ours.next()).await.is_err(),
        "the leg failed although the peer answered"
    );
}

#[tokio::test(start_paused = true)]
async fn probes_from_the_peer_are_answered_and_pongs_swallowed() {
    let (mut ours, mut peer) = wrapped();
    peer.send(probe(7)).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, pong(7));
    peer.send(pong(3)).await.unwrap();
    peer.send(data(b"x")).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), data(b"x"));
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_is_declared_dead() {
    let (mut ours, mut peer) = wrapped();
    let start = Instant::now();
    let item = timeout(600 * SECOND, ours.next())
        .await
        .expect("fails in time");
    assert!(
        matches!(item, Some(Err(TransportError::Transport(_)))),
        "got {item:?}"
    );
    assert!(start.elapsed() >= 30 * SECOND);
    assert!(start.elapsed() < 40 * SECOND);
    // The inner transport is dropped: after the unanswered probes the peer
    // sees the end.
    loop {
        match timeout(600 * SECOND, peer.next()).await.expect("ends") {
            Some(Ok(frame)) => assert!(is_probe(&frame)),
            None => break,
            Some(Err(error)) => panic!("unexpected {error:?}"),
        }
    }
    // Sending afterwards fails.
    assert!(ours.send(data(b"late")).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn dead_peer_detection_is_configurable() {
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
async fn no_probes_after_the_peer_closed_its_direction() {
    let (mut ours, mut peer) = wrapped();
    peer.send(close()).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), close());
    // Nothing is sent and nothing fails, however long the leg stays quiet.
    assert!(timeout(300 * SECOND, peer.next()).await.is_err());
    assert!(timeout(SECOND, ours.next()).await.is_err());
    // The local direction still works.
    ours.send(data(b"after")).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, data(b"after"));
}

#[tokio::test(start_paused = true)]
async fn no_probes_after_this_side_closed_its_direction() {
    let (mut ours, mut peer) = wrapped();
    ours.send(close()).await.unwrap();
    assert_eq!(peer_recv(&mut peer).await, close());
    assert!(timeout(300 * SECOND, peer.next()).await.is_err());
    // The peer's direction still works, and its probes are not answered
    // (they cannot be) without failing the leg.
    peer.send(probe(1)).await.unwrap();
    peer.send(data(b"reply")).await.unwrap();
    assert_eq!(ours.next().await.unwrap().unwrap(), data(b"reply"));
}

#[tokio::test(start_paused = true)]
async fn a_failed_pong_does_not_end_the_other_direction() {
    let (mut ours, mut peer) = wrapped();
    // Closing the sink closes the inner sink: pongs can no longer be sent.
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
    // The direction was ended, so the inner transport is closed cleanly.
    assert!(timeout(SECOND, peer.next()).await.unwrap().is_none());
}

// ---- reconnect policy ----

#[test]
fn backoff_grows_with_jitter_up_to_the_cap() {
    let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), 7);
    for attempt in 0..12u32 {
        let ceiling = (SECOND * 2u32.pow(attempt.min(10))).min(60 * SECOND);
        let delay = backoff.next_delay(RetryKind::Normal);
        assert!(
            delay >= ceiling / 2 && delay <= ceiling,
            "{attempt}: {delay:?}"
        );
    }
}

#[test]
fn jitter_differs_between_seeds() {
    let delays = |seed| {
        let mut backoff = Backoff::with_seed(ReconnectPolicy::default(), seed);
        (0..6)
            .map(|_| backoff.next_delay(RetryKind::Normal))
            .collect::<Vec<_>>()
    };
    assert_ne!(delays(1), delays(2));
    assert_eq!(delays(3), delays(3));
}

#[test]
fn already_exists_backs_off_slower() {
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
    // A short-lived connection does not reset.
    backoff.connected();
    tokio::time::advance(5 * SECOND).await;
    assert!(backoff.next_delay(RetryKind::Normal) >= 30 * SECOND);
    // A stable one does.
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

fn offline_room() -> RoomId {
    RoomId::from_ed25519(&[42; 32])
}

#[tokio::test]
async fn open_to_an_offline_room_is_a_typed_error_on_both_bindings() {
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
    }
    relay.stop().await;
}

#[tokio::test]
async fn pinned_bindings_skip_probing() {
    let relay = Relay::start(support::relay_config()).await;
    let client = RelayClient::new(relay.address(), config(Transport::Ws)).unwrap();
    let choice = client.binding().await.unwrap();
    assert_eq!(choice.binding, Binding::Ws);
    assert_eq!(choice.reason, ChoiceReason::Pinned);
    assert_eq!(client.remembered_binding(), None);
    relay.stop().await;
}

#[tokio::test]
async fn auto_picks_grpc_on_a_direct_path_and_remembers_it() {
    let relay = Relay::start(support::relay_config()).await;
    let client = RelayClient::new(relay.address(), config(Transport::Auto)).unwrap();
    let choice = client.binding().await.unwrap();
    assert_eq!(choice.binding, Binding::Grpc);
    assert_eq!(choice.reason, ChoiceReason::GrpcWorks);
    // Another client of the same address reuses the decision.
    let other = RelayClient::new(relay.address(), config(Transport::Auto)).unwrap();
    assert_eq!(other.remembered_binding(), Some(choice));
    other.forget_binding();
    assert_eq!(client.remembered_binding(), None);
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
async fn an_untrusted_certificate_is_a_tls_failure() {
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
async fn an_unreadable_ca_file_is_a_config_error() {
    let address = RelayAddress::new(true, "localhost", Some(1), "").unwrap();
    let error = RelayClient::new(
        address,
        ClientConfig {
            extra_ca_pem: Some("/nonexistent/hya-relay-ca.pem".into()),
            ..ClientConfig::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ClientError::Config(_)), "{error:?}");
}

#[tokio::test]
async fn accept_of_an_unknown_stream_fails_on_the_stream() {
    let relay = Relay::start(support::relay_config()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let client = RelayClient::new(relay.address(), config(transport)).unwrap();
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
async fn frames_sent_right_before_drop_are_delivered_on_both_bindings() {
    use hya_relay::client::register_host;
    use hya_relay::proto::proxy_to_host;
    let relay = Relay::start(support::relay_config()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let host = RelayClient::new(relay.address(), config(transport)).unwrap();
        let mut control = host.host().await.unwrap();
        let room = register_host(&mut control, &key, 5 * SECOND).await.unwrap();
        let opener = host.clone();
        let opening = tokio::spawn(async move { opener.open(&room).await.unwrap() });
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
        // The host answers, ends its direction, and drops the leg at once.
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
    }
    relay.stop().await;
}

#[tokio::test]
async fn dropping_an_unclosed_stream_aborts_it_on_both_bindings() {
    use hya_relay::client::register_host;
    use hya_relay::proto::proxy_to_host;
    let relay = Relay::start(support::relay_config()).await;
    for transport in [Transport::Grpc, Transport::Ws] {
        let key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let host = RelayClient::new(relay.address(), config(transport)).unwrap();
        let mut control = host.host().await.unwrap();
        let room = register_host(&mut control, &key, 5 * SECOND).await.unwrap();
        // The opener shares the host's gRPC connection: the abort must reset
        // only its own stream.
        let opener = host.clone();
        let opening = tokio::spawn(async move { opener.open(&room).await.unwrap() });
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
        // The control stream on the same connection is unaffected.
        let offline = RelayClient::new(relay.address(), config(transport)).unwrap();
        assert!(matches!(
            offline.open(&offline_room()).await,
            Err(ClientError::RoomOffline(_))
        ));
        let again = host.clone();
        let reopened = tokio::spawn(async move {
            again
                .open(&RoomId::from_ed25519(key.verifying_key().as_bytes()))
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

async fn next(open: &mut ChunkTransport) -> Option<Result<Chunk, TransportError>> {
    timeout(5 * SECOND, open.next()).await.unwrap()
}
