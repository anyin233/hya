#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The binding-independent proxy core: host registration, room replacement,
//! open/accept splicing, the `opened` ack, early-data buffering, limits, and
//! shutdown — all over the in-memory transport.

use std::time::Duration;

use super::support::memory::{self, MemoryTransport};
use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use hya_relay::keys::{OpenToken, Psk};
use hya_relay::link::RoomId;
use hya_relay::proto::{
    Accept, Chunk, Close, Heartbeat, HostFrame, Open, Opened, ProxyToHost, Register,
    RelayErrorCode, UpdateOpenToken, chunk, host_frame, proxy_to_host, register_signing_message,
    update_open_token_signing_message,
};
use hya_relay::proxy::{PeerInfo, ProxyCore, ProxyLimits};
use tokio::time::{Instant, timeout};

type HostEnd = MemoryTransport<HostFrame, ProxyToHost>;
type ChunkEnd = MemoryTransport<Chunk, Chunk>;

const WAIT: Duration = Duration::from_secs(60);

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn room_of(key: &SigningKey) -> String {
    RoomId::from_ed25519(key.verifying_key().as_bytes())
        .as_str()
        .to_owned()
}

fn psk_for(room: &str, generation: u8) -> Psk {
    let mut bytes = [generation; 32];
    let len = room.len().min(32);
    bytes[..len].copy_from_slice(&room.as_bytes()[..len]);
    Psk::from_bytes(bytes)
}

fn token_for(room: &str) -> Vec<u8> {
    token_gen(room, 0)
}

fn token_gen(room: &str, generation: u8) -> Vec<u8> {
    match RoomId::parse(room) {
        Ok(id) => OpenToken::derive(&psk_for(room, generation), &id)
            .as_bytes()
            .to_vec(),
        Err(_) => Vec::new(),
    }
}

fn hash_gen(key: &SigningKey, generation: u8) -> [u8; 32] {
    hya_relay::keys::open_token_hash(&token_gen(&room_of(key), generation))
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
    let hash = hash_gen(key, 0);
    HostFrame {
        frame: Some(host_frame::Frame::Register(Register {
            ed25519_pubkey: key.verifying_key().as_bytes().to_vec(),
            signature: key
                .sign(&register_signing_message(nonce, &hash))
                .to_bytes()
                .to_vec(),
            open_token_hash: hash.to_vec(),
        })),
    }
}

fn update_frame(key: &SigningKey, nonce: &[u8], hash: &[u8; 32]) -> HostFrame {
    HostFrame {
        frame: Some(host_frame::Frame::UpdateOpenToken(UpdateOpenToken {
            open_token_hash: hash.to_vec(),
            signature: key
                .sign(&update_open_token_signing_message(nonce, hash))
                .to_bytes()
                .to_vec(),
        })),
    }
}

async fn register(core: &ProxyCore, key: &SigningKey) -> HostEnd {
    register_as(core, key, "host").await
}

async fn register_as(core: &ProxyCore, key: &SigningKey, who: &str) -> HostEnd {
    let mut host = start_host(core, who);
    let nonce = challenge(&mut host).await;
    host.send(register_frame(key, &nonce)).await.unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::Registered(r) => assert_eq!(r.room_id, room_of(key)),
        other => panic!("expected registered, got {other:?}"),
    }
    host
}

async fn register_with_nonce(core: &ProxyCore, key: &SigningKey) -> (HostEnd, Vec<u8>) {
    let mut host = start_host(core, "host");
    let nonce = challenge(&mut host).await;
    host.send(register_frame(key, &nonce)).await.unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::Registered(_) => {}
        other => panic!("expected registered, got {other:?}"),
    }
    (host, nonce)
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
    start_open_with(core, room, token_for(room), who).await
}

async fn start_open_with(core: &ProxyCore, room: &str, token: Vec<u8>, who: &str) -> ChunkEnd {
    let (mut client, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_open(Box::pin(proxy), peer(who)));
    client
        .send(Chunk {
            frame: Some(chunk::Frame::Open(Open {
                room_id: room.to_owned(),
                open_token: token,
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

async fn expect_no_incoming(host: &mut HostEnd) {
    match timeout(Duration::from_millis(40), host.next()).await {
        Err(_) => {}
        Ok(frame) => panic!("the host heard of a rejected open: {frame:?}"),
    }
}

async fn spliced(core: &ProxyCore, key: &SigningKey) -> (HostEnd, ChunkEnd, ChunkEnd) {
    let mut host = register(core, key).await;
    let mut opener = start_open(core, &room_of(key)).await;
    let id = incoming(&mut host).await;
    let host_leg = start_accept(core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    (host, opener, host_leg)
}

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
async fn registration_challenge_heartbeat_and_duplicate_register_rejection() {
    let core = ProxyCore::new(ProxyLimits::default());
    let mut a = start_host(&core, "a");
    let mut b = start_host(&core, "b");
    let nonce_a = challenge(&mut a).await;
    let nonce_b = challenge(&mut b).await;
    assert_eq!(nonce_a.len(), 32);
    assert_ne!(nonce_a, nonce_b);

    let k = key(1);
    a.send(register_frame(&k, &nonce_a)).await.unwrap();
    match recv_host(&mut a).await {
        proxy_to_host::Frame::Registered(r) => assert_eq!(r.room_id, room_of(&k)),
        other => panic!("expected registered, got {other:?}"),
    }
    assert_eq!(core.stats().rooms, 1);

    // Control heartbeat probe is answered.
    a.send(HostFrame {
        frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
            seq: 7,
            pong: false,
        })),
    })
    .await
    .unwrap();
    match recv_host(&mut a).await {
        proxy_to_host::Frame::Heartbeat(hb) => assert_eq!(hb, Heartbeat { seq: 7, pong: true }),
        other => panic!("expected pong, got {other:?}"),
    }

    // Second Register on one control stream is rejected.
    a.send(register_frame(&k, &[0; 32])).await.unwrap();
    assert_eq!(host_error(&mut a).await, RelayErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn registration_rejects_bad_auth_frames_and_token_hashes() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);

    type FrameBuilder = Box<dyn Fn(&[u8]) -> HostFrame>;
    let cases: [(&str, FrameBuilder, RelayErrorCode); 5] = [
        (
            "bad signature / replayed nonce",
            Box::new(move |nonce| {
                let wrong_nonce = if nonce == [0u8; 32] {
                    [1u8; 32]
                } else {
                    [0u8; 32]
                };
                register_frame(&key(1), &wrong_nonce)
            }),
            RelayErrorCode::Unauthenticated,
        ),
        (
            "malformed pubkey",
            Box::new(|_| HostFrame {
                frame: Some(host_frame::Frame::Register(Register {
                    ed25519_pubkey: vec![1; 31],
                    signature: vec![0; 64],
                    open_token_hash: vec![0; 32],
                })),
            }),
            RelayErrorCode::Unauthenticated,
        ),
        (
            "non-register first frame",
            Box::new(|_| HostFrame {
                frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
                    seq: 1,
                    pong: false,
                })),
            }),
            RelayErrorCode::InvalidArgument,
        ),
        (
            "missing token hash",
            Box::new(move |nonce| HostFrame {
                frame: Some(host_frame::Frame::Register(Register {
                    ed25519_pubkey: k.verifying_key().as_bytes().to_vec(),
                    signature: k
                        .sign(&register_signing_message(nonce, &[]))
                        .to_bytes()
                        .to_vec(),
                    open_token_hash: Vec::new(),
                })),
            }),
            RelayErrorCode::InvalidArgument,
        ),
        (
            "signature over swapped token hash",
            Box::new(|nonce| {
                let k = key(1);
                let signed = hash_gen(&k, 0);
                HostFrame {
                    frame: Some(host_frame::Frame::Register(Register {
                        ed25519_pubkey: k.verifying_key().as_bytes().to_vec(),
                        signature: k
                            .sign(&register_signing_message(nonce, &signed))
                            .to_bytes()
                            .to_vec(),
                        open_token_hash: hash_gen(&k, 1).to_vec(),
                    })),
                }
            }),
            RelayErrorCode::Unauthenticated,
        ),
    ];

    for (label, make_frame, expected) in cases {
        let mut host = start_host(&core, "host");
        let nonce = challenge(&mut host).await;
        host.send(make_frame(&nonce)).await.unwrap();
        assert_eq!(host_error(&mut host).await, expected, "{label}");
        assert_eq!(core.stats().rooms, 0, "{label}");
    }
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
async fn replacement_evicts_old_host_and_closes_existing_streams() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let (mut old, mut opener, mut host_leg) = spliced(&core, &k).await;
    let mut new = register(&core, &k).await;
    assert_eq!(host_error(&mut old).await, RelayErrorCode::AlreadyExists);
    assert!(timeout(WAIT, old.next()).await.unwrap().is_none());
    assert_eq!(chunk_error(&mut opener).await, RelayErrorCode::Unavailable);
    assert_eq!(
        chunk_error(&mut host_leg).await,
        RelayErrorCode::Unavailable
    );
    assert_eq!(core.stats().rooms, 1);
    // Opens now reach the new host.
    let _opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut new).await;
    assert!(!id.is_empty());
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
async fn open_and_accept_protocol_errors() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;

    // Open to missing room -> NotFound + stream end.
    let mut missing = start_open(&core, &room_of(&key(9))).await;
    assert_eq!(chunk_error(&mut missing).await, RelayErrorCode::NotFound);
    expect_end(&mut missing).await;

    // Open with malformed room -> InvalidArgument.
    let mut malformed = start_open(&core, "NOT-A-ROOM").await;
    assert_eq!(
        chunk_error(&mut malformed).await,
        RelayErrorCode::InvalidArgument
    );

    // First frame on open or accept must be Open / Accept.
    let (mut bad_open, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_open(Box::pin(proxy), peer("client")));
    bad_open.send(data(b"hi")).await.unwrap();
    assert_eq!(
        chunk_error(&mut bad_open).await,
        RelayErrorCode::InvalidArgument
    );

    let (mut bad_accept, proxy) = memory::pair::<Chunk, Chunk>(4);
    tokio::spawn(core.serve_accept(Box::pin(proxy), peer("host")));
    bad_accept.send(data(b"x")).await.unwrap();
    assert_eq!(
        chunk_error(&mut bad_accept).await,
        RelayErrorCode::InvalidArgument
    );

    // Unknown stream id on accept -> NotFound.
    let mut unknown = start_accept(&core, "0123456789abcdef0123456789abcdef").await;
    assert_eq!(chunk_error(&mut unknown).await, RelayErrorCode::NotFound);

    // Duplicate accept -> NotFound.
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    let _first = start_accept(&core, &id).await;
    assert_eq!(recv(&mut opener).await, opened());
    let mut dup = start_accept(&core, &id).await;
    assert_eq!(chunk_error(&mut dup).await, RelayErrorCode::NotFound);
}

#[tokio::test]
async fn stream_ids_echo_splice_data_integrity_and_opened_ordering() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;

    // Stream ids are >= 128-bit hex and unique.
    let mut opener_a = start_open(&core, &room_of(&k)).await;
    let mut opener_b = start_open(&core, &room_of(&k)).await;
    let id_a = incoming(&mut host).await;
    let id_b = incoming(&mut host).await;
    assert_ne!(id_a, id_b);
    assert!(id_a.len() >= 32 && id_a.bytes().all(|c| c.is_ascii_hexdigit()));

    // `opened` ack precedes frames sent by the host immediately after accept.
    let mut leg_a = start_accept(&core, &id_a).await;
    leg_a.send(data(b"server hello")).await.unwrap();
    assert_eq!(recv(&mut opener_a).await, opened());
    assert_eq!(recv(&mut opener_a).await, data(b"server hello"));

    // Arbitrary byte patterns (0..=255, 60 KB, empty) forward unchanged both ways.
    let mut leg_b = start_accept(&core, &id_b).await;
    assert_eq!(recv(&mut opener_b).await, opened());
    let all: Vec<u8> = (0..=255u8).collect();
    let big: Vec<u8> = (0..60_000u32).map(|i| (i * 31 % 251) as u8).collect();
    for payload in [all, big, Vec::new()] {
        opener_b.send(data(&payload)).await.unwrap();
        assert_eq!(recv(&mut leg_b).await, data(&payload));
        leg_b.send(data(&payload)).await.unwrap();
        assert_eq!(recv(&mut opener_b).await, data(&payload));
    }

    // Echo host round-trip with clean close.
    let k2 = key(2);
    let echo_host = register(&core, &k2).await;
    spawn_echo_host(core.clone(), echo_host);
    let mut opener = start_open(&core, &room_of(&k2)).await;
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
async fn early_opener_data_and_half_close_delivered_after_accept() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;

    // Early multi-chunk data before accept.
    let mut opener1 = start_open(&core, &room_of(&k)).await;
    opener1.send(data(b"early-1")).await.unwrap();
    opener1.send(data(b"early-2")).await.unwrap();
    let id1 = incoming(&mut host).await;
    let mut leg1 = start_accept(&core, &id1).await;
    assert_eq!(recv(&mut opener1).await, opened());
    assert_eq!(recv(&mut leg1).await, data(b"early-1"));
    assert_eq!(recv(&mut leg1).await, data(b"early-2"));

    // Early data + close before accept, with responder still replying.
    let mut opener2 = start_open(&core, &room_of(&k)).await;
    opener2.send(data(b"req")).await.unwrap();
    opener2.send(close()).await.unwrap();
    let id2 = incoming(&mut host).await;
    let mut leg2 = start_accept(&core, &id2).await;
    assert_eq!(recv(&mut opener2).await, opened());
    assert_eq!(recv(&mut leg2).await, data(b"req"));
    assert_eq!(recv(&mut leg2).await, close());
    expect_end(&mut leg2).await;
    leg2.send(data(b"resp")).await.unwrap();
    leg2.send(close()).await.unwrap();
    assert_eq!(recv(&mut opener2).await, data(b"resp"));
    assert_eq!(recv(&mut opener2).await, close());
    expect_end(&mut opener2).await;
}

#[tokio::test(start_paused = true)]
async fn early_data_is_bounded_per_stream_and_globally() {
    // Per-stream limit backpressures then drains on accept.
    let core = ProxyCore::new(ProxyLimits {
        early_data_limit: 16,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let mut host = register(&core, &k).await;
    let mut opener = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
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

    // Global cap across multiple openers.
    let core = ProxyCore::new(ProxyLimits {
        early_data_limit: 1024,
        max_early_data_bytes: 16,
        ..ProxyLimits::default()
    });
    let mut host = register(&core, &k).await;
    let mut first = start_open_as(&core, &room_of(&k), "10.0.0.1").await;
    let first_id = incoming(&mut host).await;
    let mut second = start_open_as(&core, &room_of(&k), "10.0.0.2").await;
    let _second_id = incoming(&mut host).await;
    let mut sent = [0usize; 2];
    for i in 0..40u8 {
        for (n, opener) in [&mut first, &mut second].into_iter().enumerate() {
            if let Ok(Ok(())) = timeout(Duration::from_millis(50), opener.send(data(&[i; 8]))).await
            {
                sent[n] += 1;
            }
        }
    }
    let buffered = core.stats().early_data_bytes;
    assert!((16..=32).contains(&buffered), "{buffered}");
    assert!(sent[0] < 40 && sent[1] < 40, "{sent:?}");
    let mut leg = start_accept(&core, &first_id).await;
    assert_eq!(recv(&mut first).await, opened());
    assert_eq!(recv(&mut leg).await, data(&[0; 8]));
    timeout(WAIT, async {
        while core.stats().early_data_bytes > 16 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
    let mut late = start_accept(&core, &id).await;
    assert_eq!(chunk_error(&mut late).await, RelayErrorCode::NotFound);
    assert_eq!(core.stats().streams, 0);
}

#[tokio::test]
async fn data_heartbeats_before_during_and_after_half_close() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let mut host = register(&core, &k).await;

    // Heartbeat before accept is answered locally.
    let mut pre = start_open(&core, &room_of(&k)).await;
    let id = incoming(&mut host).await;
    pre.send(probe(1)).await.unwrap();
    assert_eq!(recv(&mut pre).await, pong(1));

    // Once spliced, probes on both legs are answered locally and pongs swallowed.
    let mut host_leg = start_accept(&core, &id).await;
    assert_eq!(recv(&mut pre).await, opened());
    pre.send(probe(3)).await.unwrap();
    assert_eq!(recv(&mut pre).await, pong(3));
    host_leg.send(probe(4)).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, pong(4));
    pre.send(pong(5)).await.unwrap();
    pre.send(data(b"a")).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, data(b"a"));

    // After host closes its direction, probe on opener is silently dropped without aborting.
    host_leg.send(data(b"reply")).await.unwrap();
    host_leg.send(close()).await.unwrap();
    assert_eq!(recv(&mut pre).await, data(b"reply"));
    assert_eq!(recv(&mut pre).await, close());
    expect_end(&mut pre).await;
    pre.send(probe(9)).await.unwrap();
    pre.send(data(b"late")).await.unwrap();
    pre.send(close()).await.unwrap();
    assert_eq!(recv(&mut host_leg).await, data(b"late"));
    assert_eq!(recv(&mut host_leg).await, close());
    expect_end(&mut host_leg).await;
}

#[tokio::test]
async fn peer_error_frame_and_transport_drop_propagate() {
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

    let (_host2, opener2, mut host_leg2) = spliced(&core, &key(2)).await;
    opener2
        .inject_error(hya_relay::transport::TransportError::Transport(
            "reset".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        chunk_error(&mut host_leg2).await,
        RelayErrorCode::Unavailable
    );
}

// ---- limits ----

#[tokio::test]
async fn room_limits_global_per_peer_replacement_and_eviction() {
    // Global max_rooms.
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
    let _again = register(&core, &k1).await;

    // Per-peer max_rooms, replacement, cross-peer takeover, and eviction release.
    let core = ProxyCore::new(ProxyLimits {
        max_rooms_per_peer: 1,
        ..ProxyLimits::default()
    });
    let mut a = register_as(&core, &k1, "10.0.0.1").await;
    let mut extra = start_host(&core, "10.0.0.1");
    let nonce = challenge(&mut extra).await;
    extra.send(register_frame(&key(2), &nonce)).await.unwrap();
    assert_eq!(
        host_error(&mut extra).await,
        RelayErrorCode::ResourceExhausted
    );
    // Same peer may replace its own room; another peer is unaffected.
    let mut a_replaced = register_as(&core, &k1, "10.0.0.1").await;
    assert_eq!(host_error(&mut a).await, RelayErrorCode::AlreadyExists);
    let _other = register_as(&core, &key(3), "10.0.0.2").await;

    // Peer B takes over peer A's room (key(1)): A's slot frees up.
    let _b = register_as(&core, &k1, "10.0.0.3").await;
    assert_eq!(
        host_error(&mut a_replaced).await,
        RelayErrorCode::AlreadyExists
    );
    let a2 = register_as(&core, &key(2), "10.0.0.1").await;
    drop(a2);
    let deadline = Instant::now() + WAIT;
    while core.stats().rooms > 2 {
        assert!(Instant::now() < deadline, "room not evicted");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let _a3 = register_as(&core, &key(4), "10.0.0.1").await;
}

#[tokio::test]
async fn pending_registration_limits_per_peer_global_and_release() {
    // Per-peer cap + release on completion & failure.
    let core = ProxyCore::new(ProxyLimits {
        max_pending_registrations_per_peer: 2,
        ..ProxyLimits::default()
    });
    let mut p1 = start_host(&core, "10.0.0.1");
    let mut p2 = start_host(&core, "10.0.0.1");
    let _ = challenge(&mut p1).await;
    let nonce2 = challenge(&mut p2).await;
    let mut p3 = start_host(&core, "10.0.0.1");
    assert_eq!(host_error(&mut p3).await, RelayErrorCode::ResourceExhausted);
    let mut other = start_host(&core, "10.0.0.2");
    let _ = challenge(&mut other).await;

    p2.send(register_frame(&key(1), &nonce2)).await.unwrap();
    assert!(matches!(
        recv_host(&mut p2).await,
        proxy_to_host::Frame::Registered(_)
    ));
    let mut p4 = start_host(&core, "10.0.0.1");
    let _ = challenge(&mut p4).await;
    // Failing frees its slot too.
    p4.send(HostFrame { frame: None }).await.unwrap();
    assert_eq!(host_error(&mut p4).await, RelayErrorCode::InvalidArgument);
    let _registered = register_as(&core, &key(2), "10.0.0.1").await;

    // Global cap across distinct peers.
    let core = ProxyCore::new(ProxyLimits {
        max_pending_registrations: 2,
        ..ProxyLimits::default()
    });
    let mut g1 = start_host(&core, "10.0.0.1");
    let mut g2 = start_host(&core, "10.0.0.2");
    let _ = challenge(&mut g1).await;
    let nonce = challenge(&mut g2).await;
    assert_eq!(core.stats().pending_registrations, 2);
    let mut g3 = start_host(&core, "10.0.0.3");
    assert_eq!(host_error(&mut g3).await, RelayErrorCode::ResourceExhausted);
    g2.send(register_frame(&key(1), &nonce)).await.unwrap();
    assert!(matches!(
        recv_host(&mut g2).await,
        proxy_to_host::Frame::Registered(_)
    ));
    let mut g4 = start_host(&core, "10.0.0.4");
    let _ = challenge(&mut g4).await;
}

#[tokio::test]
async fn stream_limits_per_room_per_peer_and_global() {
    // Per-room cap.
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

    // Per-peer cap (other peer unaffected).
    let core = ProxyCore::new(ProxyLimits {
        max_streams_per_peer: 1,
        ..ProxyLimits::default()
    });
    let mut host = register(&core, &k).await;
    let _first = start_open_as(&core, &room_of(&k), "10.0.0.1").await;
    let _ = incoming(&mut host).await;
    let mut second = start_open_as(&core, &room_of(&k), "10.0.0.1").await;
    assert_eq!(
        chunk_error(&mut second).await,
        RelayErrorCode::ResourceExhausted
    );
    let _other = start_open_as(&core, &room_of(&k), "10.0.0.2").await;
    let _ = incoming(&mut host).await;

    // Global cap across rooms and peers.
    let core = ProxyCore::new(ProxyLimits {
        max_streams: 2,
        ..ProxyLimits::default()
    });
    let (a, b) = (key(1), key(2));
    let mut host_a = register(&core, &a).await;
    let mut host_b = register(&core, &b).await;
    let _one = start_open_as(&core, &room_of(&a), "10.0.0.1").await;
    let _ = incoming(&mut host_a).await;
    let _two = start_open_as(&core, &room_of(&b), "10.0.0.2").await;
    let _ = incoming(&mut host_b).await;
    let mut third = start_open_as(&core, &room_of(&a), "10.0.0.3").await;
    assert_eq!(
        chunk_error(&mut third).await,
        RelayErrorCode::ResourceExhausted
    );
    assert_eq!(core.stats().streams, 2);
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
async fn idle_timeouts_evict_streams_and_control_unless_kept_alive() {
    let limits = ProxyLimits {
        idle_timeout: Duration::from_secs(30),
        ..ProxyLimits::default()
    };

    // 1. Silent data stream closes while control stream stays alive.
    let core = ProxyCore::new(limits.clone());
    let (mut host, mut opener, mut host_leg) = spliced(&core, &key(1)).await;
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

    // 2. Heartbeats on control + both data legs keep everything alive past 4x idle_timeout.
    let core = ProxyCore::new(limits.clone());
    let (mut host, mut opener, mut host_leg) = spliced(&core, &key(2)).await;
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

    // 3. Idle control stream evicts its room.
    let core = ProxyCore::new(limits);
    let mut idle_host = register(&core, &key(3)).await;
    assert_eq!(
        host_error(&mut idle_host).await,
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
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "elapsed {:?}",
        started.elapsed()
    );
    let _opener = writer.await.unwrap();
}

#[tokio::test]
async fn oversized_chunks_are_resource_exhausted_before_and_after_accept() {
    // Spliced stream.
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

    // Early chunk before accept.
    let core = ProxyCore::new(ProxyLimits {
        max_chunk_data: 8,
        ..ProxyLimits::default()
    });
    let k = key(2);
    let _host = register(&core, &k).await;
    let mut early = start_open(&core, &room_of(&k)).await;
    early.send(data(&[0; 9])).await.unwrap();
    assert_eq!(
        chunk_error(&mut early).await,
        RelayErrorCode::ResourceExhausted
    );
}

#[test]
fn limits_have_documented_defaults() {
    let l = ProxyLimits::default();
    assert_eq!(l.max_rooms, 1024);
    assert_eq!(l.max_streams_per_room, 64);
    assert_eq!(l.max_streams_per_peer, 256);
    assert_eq!(l.max_rooms_per_peer, 16);
    assert_eq!(l.max_pending_registrations_per_peer, 8);
    assert_eq!(l.idle_timeout, Duration::from_secs(120));
    assert_eq!(l.stream_rate_bytes_per_sec, 8 * 1024 * 1024);
    assert_eq!(l.stream_rate_burst_bytes, 1024 * 1024);
    assert_eq!(l.max_chunk_data, 256 * 1024);
    assert_eq!(l.early_data_limit, 64 * 1024);
    assert_eq!(l.max_streams, 8192);
    assert_eq!(l.max_pending_registrations, 256);
    assert_eq!(l.max_early_data_bytes, 64 * 1024 * 1024);
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

// ---- open tokens (link-holder gating) ----

#[tokio::test]
async fn wrong_open_tokens_look_offline_before_room_limits_apply() {
    let core = ProxyCore::new(ProxyLimits {
        max_streams_per_room: 1,
        ..ProxyLimits::default()
    });
    let k = key(1);
    let room = room_of(&k);
    let mut host = register(&core, &k).await;
    let wrong_room_token = token_for(&room_of(&key(2)));
    for token in [
        Vec::new(),
        vec![0; 32],
        token_gen(&room, 1),
        wrong_room_token,
    ] {
        let mut opener = start_open_with(&core, &room, token, "client").await;
        match recv(&mut opener).await.frame {
            Some(chunk::Frame::Error(e)) => {
                assert_eq!(e.error_code(), RelayErrorCode::NotFound);
                assert_eq!(e.message, "room is offline");
            }
            other => panic!("expected NOT_FOUND, got {other:?}"),
        }
        assert_eq!(core.stats().streams, 0, "no stream slot was taken");
    }
    expect_no_incoming(&mut host).await;

    // Right token opens and fills the single room slot.
    let _first = start_open(&core, &room).await;
    let _ = incoming(&mut host).await;
    // A wrong token against a full room still returns NotFound (not ResourceExhausted).
    let mut guess = start_open_with(&core, &room, vec![7; 32], "client").await;
    assert_eq!(chunk_error(&mut guess).await, RelayErrorCode::NotFound);
}

#[tokio::test]
async fn open_token_update_requires_valid_signature_and_invalidates_old_tokens() {
    let core = ProxyCore::new(ProxyLimits::default());
    let k = key(1);
    let room = room_of(&k);

    // Wrong nonce and wrong key are both rejected.
    let (mut bad_nonce_host, nonce) = register_with_nonce(&core, &k).await;
    let mut other_nonce = nonce.clone();
    other_nonce[0] ^= 1;
    bad_nonce_host
        .send(update_frame(&k, &other_nonce, &hash_gen(&k, 1)))
        .await
        .unwrap();
    assert_eq!(
        host_error(&mut bad_nonce_host).await,
        RelayErrorCode::Unauthenticated
    );

    let (mut bad_key_host, nonce) = register_with_nonce(&core, &k).await;
    bad_key_host
        .send(update_frame(&key(2), &nonce, &hash_gen(&k, 1)))
        .await
        .unwrap();
    assert_eq!(
        host_error(&mut bad_key_host).await,
        RelayErrorCode::Unauthenticated
    );

    // Valid update swaps active hash and invalidates generation-0 tokens.
    let (mut host, nonce) = register_with_nonce(&core, &k).await;
    host.send(update_frame(&k, &nonce, &hash_gen(&k, 1)))
        .await
        .unwrap();
    match recv_host(&mut host).await {
        proxy_to_host::Frame::OpenTokenUpdated(_) => {}
        other => panic!("expected open_token_updated, got {other:?}"),
    }
    let mut old = start_open_with(&core, &room, token_gen(&room, 0), "client").await;
    assert_eq!(chunk_error(&mut old).await, RelayErrorCode::NotFound);
    expect_no_incoming(&mut host).await;
    let _new = start_open_with(&core, &room, token_gen(&room, 1), "client").await;
    let _ = incoming(&mut host).await;
}
