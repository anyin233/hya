#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The binding-independent proxy core: host registration, room replacement,
//! open/accept splicing, the `opened` ack, early-data buffering, limits, and
//! shutdown — all over the in-memory transport.

use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use hya_relay::link::RoomId;
use hya_relay::proto::{
    Accept, Chunk, Close, Heartbeat, HostFrame, Open, Opened, ProxyToHost, Register,
    RelayErrorCode, chunk, host_frame, proxy_to_host, register_signing_message,
};
use hya_relay::proxy::{PeerInfo, ProxyCore, ProxyLimits};
use hya_relay::transport::memory::{self, MemoryTransport};
use tokio::time::{Instant, timeout};

type HostEnd = MemoryTransport<HostFrame, ProxyToHost>;
type ChunkEnd = MemoryTransport<Chunk, Chunk>;

/// Longer than every proxy timeout the tests rely on (paused-time tests
/// auto-advance through it).
const WAIT: Duration = Duration::from_secs(60);

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn room_of(key: &SigningKey) -> String {
    RoomId::from_ed25519(key.verifying_key().as_bytes())
        .as_str()
        .to_owned()
}

fn peer(name: &str) -> PeerInfo {
    PeerInfo::new(name)
}

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

fn opened() -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Opened(Opened {})),
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

/// Start a host control stream on the core; returns the host's end.
fn start_host(core: &ProxyCore, who: &str) -> HostEnd {
    let (host, proxy) = memory::pair::<HostFrame, ProxyToHost>(16);
    tokio::spawn(core.serve_host(Box::pin(proxy), peer(who)));
    host
}

async fn recv_host(host: &mut HostEnd) -> proxy_to_host::Frame {
    timeout(WAIT, host.next())
        .await
        .expect("host frame in time")
        .expect("host stream open")
        .expect("host frame ok")
        .frame
        .expect("frame set")
}

async fn challenge(host: &mut HostEnd) -> Vec<u8> {
    match recv_host(host).await {
        proxy_to_host::Frame::Challenge(c) => c.nonce,
        other => panic!("expected challenge, got {other:?}"),
    }
}

fn register_frame(key: &SigningKey, nonce: &[u8]) -> HostFrame {
    HostFrame {
        frame: Some(host_frame::Frame::Register(Register {
            ed25519_pubkey: key.verifying_key().as_bytes().to_vec(),
            signature: key
                .sign(&register_signing_message(nonce))
                .to_bytes()
                .to_vec(),
        })),
    }
}

/// Register `key` and return the host end after `registered`.
async fn register(core: &ProxyCore, key: &SigningKey) -> HostEnd {
    let mut host = start_host(core, "host");
    let nonce = challenge(&mut host).await;
    host.send(register_frame(key, &nonce)).await.unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::Registered(r) => assert_eq!(r.room_id, room_of(key)),
        other => panic!("expected registered, got {other:?}"),
    }
    host
}

async fn host_error(host: &mut HostEnd) -> RelayErrorCode {
    match recv_host(host).await {
        proxy_to_host::Frame::Error(e) => e.error_code(),
        other => panic!("expected error, got {other:?}"),
    }
}

async fn incoming(host: &mut HostEnd) -> String {
    match recv_host(host).await {
        proxy_to_host::Frame::Incoming(i) => i.stream_id,
        other => panic!("expected incoming, got {other:?}"),
    }
}

async fn start_open_as(core: &ProxyCore, room: &str, who: &str) -> ChunkEnd {
    let (mut client, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_open(Box::pin(proxy), peer(who)));
    client
        .send(Chunk {
            frame: Some(chunk::Frame::Open(Open {
                room_id: room.to_owned(),
            })),
        })
        .await
        .unwrap();
    client
}

async fn start_open(core: &ProxyCore, room: &str) -> ChunkEnd {
    start_open_as(core, room, "client").await
}

async fn start_accept(core: &ProxyCore, stream_id: &str) -> ChunkEnd {
    let (mut host_leg, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_accept(Box::pin(proxy), peer("host")));
    host_leg
        .send(Chunk {
            frame: Some(chunk::Frame::Accept(Accept {
                stream_id: stream_id.to_owned(),
            })),
        })
        .await
        .unwrap();
    host_leg
}

async fn recv(end: &mut ChunkEnd) -> Chunk {
    timeout(WAIT, end.next())
        .await
        .expect("chunk in time")
        .expect("stream open")
        .expect("chunk ok")
}

async fn expect_end(end: &mut ChunkEnd) {
    let next = timeout(WAIT, end.next()).await.expect("end in time");
    assert!(next.is_none(), "expected end of stream, got {next:?}");
}

async fn chunk_error(end: &mut ChunkEnd) -> RelayErrorCode {
    match recv(end).await.frame {
        Some(chunk::Frame::Error(e)) => e.error_code(),
        other => panic!("expected error frame, got {other:?}"),
    }
}

/// A registered host plus one fully spliced stream (opener got `opened`).
async fn spliced(core: &ProxyCore, key: &SigningKey) -> (HostEnd, ChunkEnd, ChunkEnd) {
    let mut host = register(core, key).await;
    let mut opener = start_open(core, &room_of(key)).await;
    let id = incoming(&mut host).await;
    let host_leg = start_accept(core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    (host, opener, host_leg)
}

/// Echo host: accept every incoming stream and echo data until close.
fn spawn_echo_host(core: ProxyCore, mut host: HostEnd) {
    tokio::spawn(async move {
        while let Some(Ok(frame)) = host.next().await {
            if let Some(proxy_to_host::Frame::Incoming(i)) = frame.frame {
                let mut leg = start_accept(&core, &i.stream_id).await;
                tokio::spawn(async move {
                    while let Some(Ok(c)) = leg.next().await {
                        match c.frame {
                            Some(chunk::Frame::Data(d)) => {
                                leg.send(data(&d)).await.unwrap();
                            }
                            Some(chunk::Frame::Close(_)) => {
                                leg.send(close()).await.unwrap();
                                leg.close().await.unwrap();
                                break;
                            }
                            _ => {}
                        }
                    }
                });
            }
        }
    });
}

// ---- registration ----

#[tokio::test]
async fn registration_succeeds_with_a_valid_signature() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = start_host(&core, "host");
    let nonce = challenge(&mut host).await;
    assert_eq!(nonce.len(), 32);
    let k = key(1);
    host.send(register_frame(&k, &nonce)).await.unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::Registered(r) => assert_eq!(r.room_id, room_of(&k)),
        other => panic!("expected registered, got {other:?}"),
    }
    assert_eq!(core.stats().rooms, 1);
}

#[tokio::test]
async fn challenges_are_fresh() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut a = start_host(&core, "a");
    let mut b = start_host(&core, "b");
    assert_ne!(challenge(&mut a).await, challenge(&mut b).await);
}

#[tokio::test]
async fn bad_signature_is_unauthenticated() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = start_host(&core, "host");
    let nonce = challenge(&mut host).await;
    // Signed over a different nonce: a replayed registration.
    let mut frame = register_frame(&key(1), &[0u8; 32]);
    if nonce == [0u8; 32] {
        frame = register_frame(&key(1), &[1u8; 32]);
    }
    host.send(frame).await.unwrap();
    assert_eq!(host_error(&mut host).await, RelayErrorCode::Unauthenticated);
    assert_eq!(core.stats().rooms, 0);
}

#[tokio::test]
async fn malformed_key_is_unauthenticated() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = start_host(&core, "host");
    let _ = challenge(&mut host).await;
    host.send(HostFrame {
        frame: Some(host_frame::Frame::Register(Register {
            ed25519_pubkey: vec![1; 31],
            signature: vec![0; 64],
        })),
    })
    .await
    .unwrap();
    assert_eq!(host_error(&mut host).await, RelayErrorCode::Unauthenticated);
}

#[tokio::test]
async fn first_host_frame_must_be_register() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = start_host(&core, "host");
    let _ = challenge(&mut host).await;
    host.send(HostFrame {
        frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
            seq: 1,
            pong: false,
        })),
    })
    .await
    .unwrap();
    assert_eq!(host_error(&mut host).await, RelayErrorCode::InvalidArgument);
}

#[tokio::test(start_paused = true)]
async fn registration_times_out() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = start_host(&core, "host");
    let _ = challenge(&mut host).await;
    let started = Instant::now();
    assert_eq!(
        host_error(&mut host).await,
        RelayErrorCode::DeadlineExceeded
    );
    assert!(started.elapsed() >= ProxyLimits::default().handshake_timeout);
}

#[tokio::test]
async fn control_heartbeat_probe_is_answered() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut host = register(&core, &key(1)).await;
    host.send(HostFrame {
        frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
            seq: 7,
            pong: false,
        })),
    })
    .await
    .unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::Heartbeat(hb) => assert_eq!(hb, Heartbeat { seq: 7, pong: true }),
        other => panic!("expected pong, got {other:?}"),
    }
}

#[tokio::test]
async fn second_register_on_one_control_stream_is_rejected() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    host.send(register_frame(&k, &[0; 32])).await.unwrap();
    assert_eq!(
        host_error(&mut host).await,
        RelayErrorCode::FailedPrecondition
    );
}

#[tokio::test]
async fn new_registration_replaces_the_old_host() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut old = register(&core, &k).await;
    let mut new = register(&core, &k).await;
    assert_eq!(host_error(&mut old).await, RelayErrorCode::AlreadyExists);
    assert!(timeout(WAIT, old.next()).await.unwrap().is_none());
    assert_eq!(core.stats().rooms, 1);
    // Opens now reach the new host.
    let _opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut new).await;
    assert!(!id.is_empty());
}

#[tokio::test]
async fn replacement_closes_the_old_hosts_streams() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let (_old, mut opener, mut host_leg) = spliced(&core, &k).await;
    let _new = register(&core, &k).await;
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::Unavailable);
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::Unavailable
    );
}

#[tokio::test]
async fn host_disconnect_evicts_the_room_and_its_streams() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let (mut host, mut opener, mut host_leg) = spliced(&core, &k).await;
    let mut pending = start_open(&core, &room_of(&k)).await;
    let _ = incoming(&mut host).await;
    host.close().await.unwrap();
    drop(host);
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::Unavailable);
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::Unavailable
    );
    assert_eq!(chunk_error(&mut pending).await, RelayErrorCode::Unavailable);
    let mut late = start_open(&core, &room_of(&k)).await;
    assert_eq!(chunk_error(&mut late).await, RelayErrorCode::NotFound);
    assert_eq!(core.stats().rooms, 0);
}

// ---- open / accept ----

#[tokio::test]
async fn open_to_a_missing_room_is_not_found() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut opener = start_open(&core, &room_of(&key(9))).await;
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::NotFound);
    expect_end(&mut opener).await;
}

#[tokio::test]
async fn open_with_a_malformed_room_is_invalid() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut opener = start_open(&core, "NOT-A-ROOM").await;
    assert_eq!(
        chunk_error(&mut opener).await,
        RelayErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn first_open_frame_must_be_open() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (mut client, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_open(Box::pin(proxy), peer("client")));
    client.send(data(b"hi")).await.unwrap();
    assert_eq!(
        chunk_error(&mut client).await,
        RelayErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn stream_ids_are_long_and_unique() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let _a = start_open(&core, &room_of(&k)).await;
    let _b = start_open(&core, &room_of(&k)).await;
    let a = incoming(&mut host).await;
    let b = incoming(&mut host).await;
    assert_ne!(a, b);
    // At least 128 bits of hex.
    assert!(a.len() >= 32 && a.bytes().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn splice_round_trips_both_ways_through_an_echo_host() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let host = register(&core, &k).await;
    spawn_echo_host(core.clone(), host);
    let mut opener = start_open(&core, &room_of(&k)).await;
    assert_eq!(recv(&mut opener).await, opened());
    for msg in [&b"hello"[..], b"world", &[0u8, 255, 1, 254]] {
        opener.send(data(msg)).await.unwrap();
        assert_eq!(recv(&mut opener).await, data(msg));
    }
    opener.send(close()).await.unwrap();
    assert_eq!(recv(&mut opener).await, close());
    expect_end(&mut opener).await;
}

#[tokio::test]
async fn proxy_forwards_data_bytes_unchanged() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (_host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    let all: Vec<u8> = (0..=255u8).collect();
    let big: Vec<u8> = (0..60_000u32).map(|i| (i * 31 % 251) as u8).collect();
    for payload in [all.clone(), big.clone(), Vec::new()] {
        opener.send(data(&payload)).await.unwrap();
        assert_eq!(recv(&mut host_leg).await, data(&payload));
        host_leg.send(data(&payload)).await.unwrap();
        assert_eq!(recv(&mut opener).await, data(&payload));
    }
}

#[tokio::test]
async fn opened_ack_precedes_host_frames() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    let mut host_leg = start_accept(&core, &id).await;
    // The host speaks first, before the opener sent anything.
    host_leg.send(data(b"server hello")).await.unwrap();
    assert_eq!(recv(&mut opener).await, opened());
    assert_eq!(recv(&mut opener).await, data(b"server hello"));
}

#[tokio::test]
async fn early_opener_data_is_delivered_after_accept() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    opener.send(data(b"early-1")).await.unwrap();
    opener.send(data(b"early-2")).await.unwrap();
    let id = incoming(&mut host).await;
    let mut host_leg = start_accept(&core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    assert_eq!(recv(&mut host_leg).await, data(b"early-1"));
    assert_eq!(recv(&mut host_leg).await, data(b"early-2"));
}

#[tokio::test(start_paused = true)]
async fn early_data_is_bounded_then_backpressured() {
    let limits = ProxyLimits {
        early_data_limit: 16,
        ..ProxyLimits::default()
    };
    let core = ProxyCore::new(limits);
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    // 8-byte chunks: the proxy reads two (16 bytes), then stops reading, so
    // the opener's sends block once the in-memory channel fills.
    let mut sent = 0usize;
    for i in 0..40u8 {
        match timeout(Duration::from_millis(200), opener.send(data(&[i; 8]))).await {
            Ok(Ok(())) => sent += 1,
            Ok(Err(e)) => panic!("send failed: {e}"),
            Err(_) => break,
        }
    }
    assert!(sent < 40, "proxy kept reading past the early-data limit");
    let mut host_leg = start_accept(&core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    for i in 0..sent {
        assert_eq!(recv(&mut host_leg).await, data(&[i as u8; 8]));
    }
}

#[tokio::test]
async fn opener_close_before_accept_is_forwarded() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    opener.send(data(b"req")).await.unwrap();
    opener.send(close()).await.unwrap();
    let id = incoming(&mut host).await;
    let mut host_leg = start_accept(&core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    assert_eq!(recv(&mut host_leg).await, data(b"req"));
    assert_eq!(recv(&mut host_leg).await, close());
    expect_end(&mut host_leg).await;
    // The other direction still works.
    host_leg.send(data(b"resp")).await.unwrap();
    host_leg.send(close()).await.unwrap();
    assert_eq!(recv(&mut opener).await, data(b"resp"));
    assert_eq!(recv(&mut opener).await, close());
    expect_end(&mut opener).await;
}

#[tokio::test(start_paused = true)]
async fn accept_timeout_fails_the_opener_with_unavailable() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    let started = Instant::now();
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::Unavailable);
    assert!(started.elapsed() >= ProxyLimits::default().accept_timeout);
    // A late accept finds nothing.
    let mut late = start_accept(&core, &id).await;
    assert_eq!(chunk_error(&mut late).await, RelayErrorCode::NotFound);
    assert_eq!(core.stats().streams, 0);
}

#[tokio::test]
async fn accept_with_an_unknown_stream_id_is_not_found() {
    let core = ProxyCore::new(ProxyLimits::default());
    let _host = register(&core, &key(1)).await;
    let mut leg = start_accept(&core, "0123456789abcdef0123456789abcdef").await;
    assert_eq!(chunk_error(&mut leg).await, RelayErrorCode::NotFound);
}

#[tokio::test]
async fn duplicate_accept_is_not_found() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    let _first = start_accept(&core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    let mut second = start_accept(&core, &id).await;
    assert_eq!(chunk_error(&mut second).await, RelayErrorCode::NotFound);
}

#[tokio::test]
async fn first_accept_frame_must_be_accept() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (mut leg, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_accept(Box::pin(proxy), peer("host")));
    leg.send(data(b"x")).await.unwrap();
    assert_eq!(chunk_error(&mut leg).await, RelayErrorCode::InvalidArgument);
}

#[tokio::test]
async fn data_heartbeats_are_answered_locally_not_forwarded() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (_host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    opener.send(probe(3)).await.unwrap();
    assert_eq!(recv(&mut opener).await, pong(3));
    host_leg.send(probe(4)).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, pong(4));
    // Pongs are swallowed; the next frames each side sees are real data.
    opener.send(pong(5)).await.unwrap();
    opener.send(data(b"a")).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, data(b"a"));
    host_leg.send(data(b"b")).await.unwrap();
    assert_eq!(recv(&mut opener).await, data(b"b"));
}

#[tokio::test]
async fn opener_heartbeat_before_accept_is_answered() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let _host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    opener.send(probe(1)).await.unwrap();
    assert_eq!(recv(&mut opener).await, pong(1));
}

#[tokio::test]
async fn peer_error_frame_is_forwarded() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (_host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    host_leg
        .send(Chunk {
            frame: Some(chunk::Frame::Error(hya_relay::proto::RelayError::new(
                RelayErrorCode::PermissionDenied,
                "nope",
            ))),
        })
        .await
        .unwrap();
    assert_eq!(
        chunk_error(&mut opener).await,
        RelayErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn opener_drop_fails_the_host_leg() {
    let core = ProxyCore::new(ProxyLimits::default());
    let (_host, opener, mut host_leg) = spliced(&core, &key(1)).await;
    opener
        .inject_error(hya_relay::transport::TransportError::Transport(
            "reset".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::Unavailable
    );
}

// ---- limits ----

#[tokio::test]
async fn max_rooms_is_enforced() {
    let core = ProxyCore::new(ProxyLimits {
        max_rooms: 1,
        ..ProxyLimits::default()
    });
    let k1 = key(1);
    let _first = register(&core, &k1).await;
    let mut second = start_host(&core, "host");
    let nonce = challenge(&mut second).await;
    second.send(register_frame(&key(2), &nonce)).await.unwrap();
    assert_eq!(
        host_error(&mut second).await,
        RelayErrorCode::ResourceExhausted
    );
    // Replacing an existing room needs no new slot.
    let _again = register(&core, &k1).await;
}

#[tokio::test]
async fn max_streams_per_room_is_enforced() {
    let core = ProxyCore::new(ProxyLimits {
        max_streams_per_room: 1,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let mut host = register(&core, &k).await;
    let _first = start_open(&core, &room_of(&k)).await;
    let _ = incoming(&mut host).await;
    let mut second = start_open(&core, &room_of(&k)).await;
    assert_eq!(
        chunk_error(&mut second).await,
        RelayErrorCode::ResourceExhausted
    );
}

#[tokio::test]
async fn max_streams_per_peer_is_enforced() {
    let core = ProxyCore::new(ProxyLimits {
        max_streams_per_peer: 1,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let mut host = register(&core, &k).await;
    let _first = start_open_as(&core, &room_of(&k), "10.0.0.1").await;
    let _ = incoming(&mut host).await;
    let mut second = start_open_as(&core, &room_of(&k), "10.0.0.1").await;
    assert_eq!(
        chunk_error(&mut second).await,
        RelayErrorCode::ResourceExhausted
    );
    // Another client is not affected.
    let _other = start_open_as(&core, &room_of(&k), "10.0.0.2").await;
    let _ = incoming(&mut host).await;
}

#[tokio::test]
async fn stream_slots_are_released_when_streams_end() {
    let core = ProxyCore::new(ProxyLimits {
        max_streams_per_room: 1,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let (mut host, mut opener, mut host_leg) = spliced(&core, &k).await;
    opener.send(close()).await.unwrap();
    host_leg.send(close()).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, close());
    assert_eq!(recv(&mut opener).await, close());
    expect_end(&mut opener).await;
    let _next = start_open(&core, &room_of(&k)).await;
    let _ = incoming(&mut host).await;
}

#[tokio::test(start_paused = true)]
async fn idle_streams_are_closed() {
    let limits = ProxyLimits {
        idle_timeout: Duration::from_secs(30),
        ..ProxyLimits::default()
    };
    let core = ProxyCore::new(limits);
    let (mut host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    // Keep the control stream alive; the data stream stays silent.
    let keepalive = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if host
                .send(HostFrame {
                    frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
                        seq: 1,
                        pong: false,
                    })),
                })
                .await
                .is_err()
            {
                break;
            }
            let _ = host.next().await;
        }
    });
    let started = Instant::now();
    assert_eq!(
        chunk_error(&mut opener).await,
        RelayErrorCode::DeadlineExceeded
    );
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::DeadlineExceeded
    );
    assert!(started.elapsed() >= Duration::from_secs(30));
    keepalive.abort();
}

#[tokio::test(start_paused = true)]
async fn heartbeats_keep_a_stream_alive() {
    let limits = ProxyLimits {
        idle_timeout: Duration::from_secs(30),
        ..ProxyLimits::default()
    };
    let core = ProxyCore::new(limits);
    let (mut host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    for seq in 0..6 {
        tokio::time::sleep(Duration::from_secs(20)).await;
        host.send(HostFrame {
            frame: Some(host_frame::Frame::Heartbeat(Heartbeat { seq, pong: false })),
        })
        .await
        .unwrap();
        let _ = recv_host(&mut host).await;
        opener.send(probe(seq)).await.unwrap();
        assert_eq!(recv(&mut opener).await, pong(seq));
        host_leg.send(probe(seq)).await.unwrap();
        assert_eq!(recv(&mut host_leg).await, pong(seq));
    }
    opener.send(data(b"still here")).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, data(b"still here"));
}

#[tokio::test(start_paused = true)]
async fn idle_control_stream_evicts_the_room() {
    let limits = ProxyLimits {
        idle_timeout: Duration::from_secs(30),
        ..ProxyLimits::default()
    };
    let core = ProxyCore::new(limits);
    let mut host = register(&core, &key(1)).await;
    assert_eq!(
        host_error(&mut host).await,
        RelayErrorCode::DeadlineExceeded
    );
    assert_eq!(core.stats().rooms, 0);
}

#[tokio::test(start_paused = true)]
async fn stream_byte_rate_is_capped() {
    let limits = ProxyLimits {
        stream_rate_bytes_per_sec: 1_000,
        stream_rate_burst_bytes: 1_000,
        ..ProxyLimits::default()
    };
    let core = ProxyCore::new(limits);
    let (_host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    let started = Instant::now();
    let writer = tokio::spawn(async move {
        for _ in 0..4 {
            opener.send(data(&[7; 1_000])).await.unwrap();
        }
        opener
    });
    for _ in 0..4 {
        assert_eq!(recv(&mut host_leg).await, data(&[7; 1_000]));
    }
    // 4000 bytes at 1000 B/s with a 1000-byte burst: at least 3 s.
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "elapsed {:?}",
        started.elapsed()
    );
    let _opener = writer.await.unwrap();
}

#[tokio::test]
async fn oversized_chunk_is_resource_exhausted() {
    let core = ProxyCore::new(ProxyLimits {
        max_chunk_data: 1024,
        ..ProxyLimits::default()
    });
    let (_host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
    opener.send(data(&[0; 1024])).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, data(&[0; 1024]));
    opener.send(data(&[0; 1025])).await.unwrap();
    assert_eq!(
        chunk_error(&mut opener).await,
        RelayErrorCode::ResourceExhausted
    );
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::ResourceExhausted
    );
}

#[tokio::test]
async fn oversized_early_chunk_is_resource_exhausted() {
    let core = ProxyCore::new(ProxyLimits {
        max_chunk_data: 8,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let _host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    opener.send(data(&[0; 9])).await.unwrap();
    assert_eq!(
        chunk_error(&mut opener).await,
        RelayErrorCode::ResourceExhausted
    );
}

#[test]
fn limits_have_documented_defaults() {
    let l = ProxyLimits::default();
    assert_eq!(l.max_rooms, 1024);
    assert_eq!(l.max_streams_per_room, 64);
    assert_eq!(l.max_streams_per_peer, 256);
    assert_eq!(l.idle_timeout, Duration::from_secs(120));
    assert_eq!(l.stream_rate_bytes_per_sec, 8 * 1024 * 1024);
    assert_eq!(l.stream_rate_burst_bytes, 1024 * 1024);
    assert_eq!(l.max_chunk_data, 256 * 1024);
    assert_eq!(l.early_data_limit, 64 * 1024);
    assert_eq!(l.accept_timeout, Duration::from_secs(10));
    assert_eq!(l.handshake_timeout, Duration::from_secs(10));
}

// ---- shutdown ----

#[tokio::test]
async fn shutdown_closes_everything_and_refuses_new_work() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let (mut host, mut opener, mut host_leg) = spliced(&core, &k).await;
    let mut pending_host = start_host(&core, "late-host");
    let _ = challenge(&mut pending_host).await;
    timeout(WAIT, core.shutdown())
        .await
        .expect("shutdown completes");
    assert_eq!(host_error(&mut host).await, RelayErrorCode::Unavailable);
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::Unavailable);
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::Unavailable
    );
    assert_eq!(
        host_error(&mut pending_host).await,
        RelayErrorCode::Unavailable
    );
    assert_eq!(core.stats().rooms, 0);
    let mut refused = start_host(&core, "after");
    assert_eq!(host_error(&mut refused).await, RelayErrorCode::Unavailable);
    let mut refused_open = start_open(&core, &room_of(&k)).await;
    assert_eq!(
        chunk_error(&mut refused_open).await,
        RelayErrorCode::Unavailable
    );
}
