#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Noise `NKpsk0` tunnel over an in-memory `ChunkTransport`.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use hya_relay::keys::{Psk, StaticKeypair};
use hya_relay::link::{RelayAddress, RelayLink, RoomId, Transport};
use hya_relay::proto::{Chunk, Close, Heartbeat, Open, RelayError, RelayErrorCode, chunk};
use hya_relay::transport::memory::{MemoryTransport, pair};
use hya_relay::tunnel::{MAX_RECORD_PLAINTEXT, NoiseStream, TunnelConfig, TunnelError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type End = MemoryTransport<Chunk, Chunk>;

fn room(byte: u8) -> RoomId {
    RoomId::from_ed25519(&[byte; 32])
}

struct Keys {
    server: StaticKeypair,
    psk: Psk,
    room: RoomId,
}

fn keys() -> Keys {
    Keys {
        server: StaticKeypair::generate().unwrap(),
        psk: Psk::generate().unwrap(),
        room: room(7),
    }
}

/// Handshake both sides over `client`/`host` ends with the given config.
async fn connect(
    keys: &Keys,
    client: End,
    host: End,
    config: TunnelConfig,
) -> (NoiseStream<End>, NoiseStream<End>) {
    let responder = NoiseStream::respond(host, &keys.room, &keys.server, &keys.psk, config);
    let initiator =
        NoiseStream::initiate(client, &keys.room, keys.server.public(), &keys.psk, config);
    let (i, r) = tokio::join!(initiator, responder);
    (i.unwrap(), r.unwrap())
}

fn tunnel_error(err: &io::Error) -> TunnelError {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<TunnelError>())
        .cloned()
        .unwrap_or_else(|| panic!("io error without a TunnelError: {err:?}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    ToHost,
    ToClient,
}

type Hook = Arc<Mutex<dyn FnMut(Dir, Chunk) -> Vec<Chunk> + Send>>;

/// A man-in-the-middle relay: `client end <-> mitm <-> host end`, passing
/// every frame through `hook`, which may drop, rewrite, or inject frames.
fn mitm(capacity: usize, hook: Hook) -> (End, End) {
    let (client, mitm_c) = pair::<Chunk, Chunk>(capacity);
    let (mitm_h, host) = pair::<Chunk, Chunk>(capacity);
    let (mut c_tx, mut c_rx) = mitm_c.split();
    let (mut h_tx, mut h_rx) = mitm_h.split();
    let hook_a = hook.clone();
    tokio::spawn(async move {
        while let Some(Ok(frame)) = c_rx.next().await {
            let out = (hook_a.lock().unwrap())(Dir::ToHost, frame);
            for frame in out {
                if h_tx.send(frame).await.is_err() {
                    return;
                }
            }
        }
        let _ = h_tx.close().await;
    });
    tokio::spawn(async move {
        while let Some(Ok(frame)) = h_rx.next().await {
            let out = (hook.lock().unwrap())(Dir::ToClient, frame);
            for frame in out {
                if c_tx.send(frame).await.is_err() {
                    return;
                }
            }
        }
        let _ = c_tx.close().await;
    });
    (client, host)
}

fn data_len(frame: &Chunk) -> Option<usize> {
    match &frame.frame {
        Some(chunk::Frame::Data(bytes)) => Some(bytes.len()),
        _ => None,
    }
}

#[tokio::test]
async fn round_trip_both_directions() {
    let keys = keys();
    let (client, host) = pair::<Chunk, Chunk>(8);
    let (mut i, mut r) = connect(&keys, client, host, TunnelConfig::default()).await;

    i.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    i.flush().await.unwrap();
    let mut buf = [0u8; 18];
    r.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"GET / HTTP/1.1\r\n\r\n");

    r.write_all(b"HTTP/1.1 200 OK\r\n").await.unwrap();
    r.flush().await.unwrap();
    let mut buf = [0u8; 17];
    i.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"HTTP/1.1 200 OK\r\n");
}

#[tokio::test]
async fn initiate_link_uses_link_keys_and_room() {
    let keys = keys();
    let address = RelayAddress::parse_proxy_url("https://relay.example.com").unwrap();
    let link = RelayLink::new(
        address,
        keys.room.clone(),
        Transport::Auto,
        *keys.server.public(),
        *keys.psk.as_bytes(),
    );
    let (client, host) = pair::<Chunk, Chunk>(8);
    let config = TunnelConfig::default();
    let (i, r) = tokio::join!(
        NoiseStream::initiate_link(client, &link, config),
        NoiseStream::respond(host, &keys.room, &keys.server, &keys.psk, config),
    );
    let (mut i, mut r) = (i.unwrap(), r.unwrap());
    i.write_all(b"ping").await.unwrap();
    i.flush().await.unwrap();
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");
}

/// Run a handshake where the client uses `client_*` parameters, returning
/// both results and the number of data frames that reached the host.
async fn mismatched_handshake(
    keys: &Keys,
    client_room: &RoomId,
    client_server_key: &[u8; 32],
    client_psk: &Psk,
) -> (
    Result<NoiseStream<End>, TunnelError>,
    Result<NoiseStream<End>, TunnelError>,
    usize,
) {
    let to_host = Arc::new(Mutex::new(0usize));
    let counter = to_host.clone();
    let hook: Hook = Arc::new(Mutex::new(move |dir: Dir, frame: Chunk| {
        if dir == Dir::ToHost && data_len(&frame).is_some() {
            *counter.lock().unwrap() += 1;
        }
        vec![frame]
    }));
    let (client, host) = mitm(8, hook);
    let config = TunnelConfig::default();
    let (i, r) = tokio::join!(
        NoiseStream::initiate(client, client_room, client_server_key, client_psk, config),
        NoiseStream::respond(host, &keys.room, &keys.server, &keys.psk, config),
    );
    let frames = *to_host.lock().unwrap();
    (i, r, frames)
}

#[tokio::test]
async fn wrong_psk_fails_on_responder_before_app_data() {
    let keys = keys();
    let wrong = Psk::generate().unwrap();
    let (i, r, frames) =
        mismatched_handshake(&keys, &keys.room, keys.server.public(), &wrong).await;
    assert!(matches!(r, Err(TunnelError::Handshake(_))), "{r:?}");
    assert!(i.is_err(), "initiator must not complete: {:?}", i.is_ok());
    // Only the first handshake message ever reached the host.
    assert_eq!(frames, 1);
}

#[tokio::test]
async fn wrong_server_key_fails() {
    let keys = keys();
    let other = StaticKeypair::generate().unwrap();
    let (i, r, _) = mismatched_handshake(&keys, &keys.room, other.public(), &keys.psk).await;
    assert!(matches!(r, Err(TunnelError::Handshake(_))), "{r:?}");
    assert!(i.is_err());
}

#[tokio::test]
async fn room_prologue_mismatch_fails() {
    let keys = keys();
    let other_room = room(9);
    let (i, r, _) = mismatched_handshake(&keys, &other_room, keys.server.public(), &keys.psk).await;
    assert!(matches!(r, Err(TunnelError::Handshake(_))), "{r:?}");
    assert!(i.is_err());
}

#[tokio::test]
async fn tampered_ciphertext_is_fatal_without_partial_plaintext() {
    let keys = keys();
    let seen = Arc::new(Mutex::new(0usize));
    let seen_hook = seen.clone();
    let hook: Hook = Arc::new(Mutex::new(move |dir: Dir, mut frame: Chunk| {
        if let (Dir::ToHost, Some(chunk::Frame::Data(bytes))) = (dir, &mut frame.frame) {
            let mut n = seen_hook.lock().unwrap();
            *n += 1;
            // Frame 1 is the handshake; frame 2 the first record; flip a
            // byte in the second record.
            if *n == 3 {
                bytes[0] ^= 0x01;
            }
        }
        vec![frame]
    }));
    let (client, host) = mitm(8, hook);
    let config = TunnelConfig::default()
        .with_max_record_plaintext(4)
        .unwrap();
    let (mut i, mut r) = connect(&keys, client, host, config).await;

    i.write_all(b"goodEVIL").await.unwrap();
    i.flush().await.unwrap();

    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"good");

    let mut rest = Vec::new();
    let err = r.read_to_end(&mut rest).await.unwrap_err();
    assert!(
        rest.is_empty(),
        "tampered record leaked plaintext: {rest:?}"
    );
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(matches!(tunnel_error(&err), TunnelError::Decrypt));

    // The stream stays dead in both directions.
    let err = r.read(&mut buf).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(r.write_all(b"x").await.is_err());
}

#[tokio::test]
async fn five_mib_round_trip_with_small_records_both_directions() {
    let keys = keys();
    let (client, host) = pair::<Chunk, Chunk>(4);
    let config = TunnelConfig::default()
        .with_max_record_plaintext(1000)
        .unwrap();
    let (i, r) = connect(&keys, client, host, config).await;

    let payload: Vec<u8> = (0..5 * 1024 * 1024u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let reversed: Vec<u8> = payload.iter().rev().copied().collect();

    let (mut i_read, mut i_write) = tokio::io::split(i);
    let (mut r_read, mut r_write) = tokio::io::split(r);

    let send_i = {
        let payload = payload.clone();
        async move {
            i_write.write_all(&payload).await.unwrap();
            i_write.shutdown().await.unwrap();
        }
    };
    let send_r = {
        let reversed = reversed.clone();
        async move {
            r_write.write_all(&reversed).await.unwrap();
            r_write.shutdown().await.unwrap();
        }
    };
    let recv_r = async move {
        let mut got = Vec::new();
        r_read.read_to_end(&mut got).await.unwrap();
        got
    };
    let recv_i = async move {
        let mut got = Vec::new();
        i_read.read_to_end(&mut got).await.unwrap();
        got
    };
    let ((), (), at_host, at_client) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(send_i, send_r, recv_r, recv_i)
    })
    .await
    .expect("5 MiB transfer stalled");
    assert!(at_host == payload, "host payload mismatch");
    assert!(at_client == reversed, "client payload mismatch");
}

#[tokio::test]
async fn half_close_initiator_shutdown_gives_responder_eof_and_responder_can_still_write() {
    let keys = keys();
    let (client, host) = pair::<Chunk, Chunk>(8);
    let (mut i, mut r) = connect(&keys, client, host, TunnelConfig::default()).await;

    i.write_all(b"request").await.unwrap();
    i.shutdown().await.unwrap();
    assert!(i.write_all(b"late").await.is_err());

    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"request");
    // EOF is sticky.
    let mut buf = [0u8; 1];
    assert_eq!(r.read(&mut buf).await.unwrap(), 0);

    r.write_all(b"response").await.unwrap();
    r.shutdown().await.unwrap();
    let mut got = Vec::new();
    i.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"response");
}

#[tokio::test]
async fn bare_close_without_close_record_is_truncation() {
    let keys = keys();
    // Drop the peer's encrypted close record so only `Chunk.close` arrives.
    let hook: Hook = Arc::new(Mutex::new(move |dir: Dir, frame: Chunk| {
        // Records carrying application data are > 16 bytes of ciphertext;
        // the close record is exactly the 16-byte tag.
        if dir == Dir::ToHost && data_len(&frame) == Some(16) {
            return Vec::new();
        }
        vec![frame]
    }));
    let (client, host) = mitm(8, hook);
    let (mut i, mut r) = connect(&keys, client, host, TunnelConfig::default()).await;
    i.write_all(b"partial").await.unwrap();
    i.shutdown().await.unwrap();

    let mut got = Vec::new();
    let err = r.read_to_end(&mut got).await.unwrap_err();
    assert_eq!(got, b"partial");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(matches!(tunnel_error(&err), TunnelError::Truncated));
}

#[tokio::test]
async fn writer_blocks_when_transport_is_full() {
    let keys = keys();
    let capacity = 2;
    let (client, host) = pair::<Chunk, Chunk>(capacity);
    let record = 1024;
    let config = TunnelConfig::default()
        .with_max_record_plaintext(record)
        .unwrap();
    let (mut i, mut r) = connect(&keys, client, host, config).await;

    // The responder is not reading, so the channel fills up and the writer
    // must stop accepting bytes instead of buffering them.
    let block = vec![0x5a; record];
    let mut accepted = 0usize;
    while let Ok(n) = tokio::time::timeout(Duration::from_millis(100), i.write(&block)).await {
        accepted += n.unwrap();
        assert!(
            accepted <= record * (capacity + 2),
            "writer buffered {accepted} bytes past a {capacity}-frame transport"
        );
    }
    assert!(accepted >= record, "writer never made progress");

    // Draining the responder unblocks the writer.
    let reader = tokio::spawn(async move {
        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        got.len()
    });
    i.write_all(&block).await.unwrap();
    i.shutdown().await.unwrap();
    let total = reader.await.unwrap();
    assert_eq!(total, accepted + record);
}

#[tokio::test]
async fn heartbeats_and_unknown_frames_are_transparent_and_probes_get_pongs() {
    let keys = keys();
    let pongs = Arc::new(Mutex::new(Vec::<(Dir, u64)>::new()));
    let pongs_hook = pongs.clone();
    let mut seq = 100u64;
    let hook: Hook = Arc::new(Mutex::new(move |dir: Dir, frame: Chunk| {
        if let Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: true })) = &frame.frame {
            pongs_hook.lock().unwrap().push((dir, *seq));
            return Vec::new();
        }
        seq += 1;
        vec![
            Chunk {
                frame: Some(chunk::Frame::Heartbeat(Heartbeat { seq, pong: false })),
            },
            Chunk {
                frame: Some(chunk::Frame::Heartbeat(Heartbeat {
                    seq: 9_999,
                    pong: true,
                })),
            },
            Chunk { frame: None },
            Chunk {
                frame: Some(chunk::Frame::Open(Open {
                    room_id: "ignored".into(),
                    open_token: Vec::new(),
                })),
            },
            frame,
        ]
    }));
    let (client, host) = mitm(8, hook);
    let (mut i, mut r) = connect(&keys, client, host, TunnelConfig::default()).await;

    i.write_all(b"hello").await.unwrap();
    i.shutdown().await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"hello");

    r.write_all(b"world").await.unwrap();
    r.shutdown().await.unwrap();
    let mut got = Vec::new();
    i.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"world");

    // Both endpoints answered probes (the pong travels back toward the
    // side that the probe impersonated).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let seen = pongs.lock().unwrap().clone();
            if seen.iter().any(|(d, _)| *d == Dir::ToHost)
                && seen.iter().any(|(d, _)| *d == Dir::ToClient)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("probes were not answered with pongs");
    assert!(pongs.lock().unwrap().iter().all(|(_, seq)| *seq > 100));
}

#[tokio::test]
async fn relay_error_frame_surfaces_code() {
    let keys = keys();
    let armed = Arc::new(Mutex::new(false));
    let armed_hook = armed.clone();
    let hook: Hook = Arc::new(Mutex::new(move |dir: Dir, frame: Chunk| {
        if dir == Dir::ToHost && *armed_hook.lock().unwrap() {
            return vec![Chunk {
                frame: Some(chunk::Frame::Error(RelayError {
                    code: RelayErrorCode::Unavailable as i32,
                    message: "host went away".into(),
                })),
            }];
        }
        vec![frame]
    }));
    let (client, host) = mitm(8, hook);
    let (mut i, mut r) = connect(&keys, client, host, TunnelConfig::default()).await;
    *armed.lock().unwrap() = true;
    i.write_all(b"x").await.unwrap();
    i.flush().await.unwrap();

    let mut buf = [0u8; 1];
    let err = r.read(&mut buf).await.unwrap_err();
    match tunnel_error(&err) {
        TunnelError::Relay { code, message } => {
            assert_eq!(code, RelayErrorCode::Unavailable);
            assert_eq!(message, "host went away");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn handshake_fails_when_peer_closes_first() {
    let keys = keys();
    let (client, host) = pair::<Chunk, Chunk>(8);
    let mut host = host;
    let driver = tokio::spawn(async move {
        let first = host.next().await;
        host.send(Chunk {
            frame: Some(chunk::Frame::Close(Close {})),
        })
        .await
        .unwrap();
        first
    });
    let result = NoiseStream::initiate(
        client,
        &keys.room,
        keys.server.public(),
        &keys.psk,
        TunnelConfig::default(),
    )
    .await;
    assert!(matches!(result, Err(TunnelError::Handshake(_))));
    let first = driver.await.unwrap().unwrap().unwrap();
    assert!(
        data_len(&first).is_some(),
        "initiator sends msg1 immediately"
    );
}

#[test]
fn record_size_is_validated() {
    assert!(
        TunnelConfig::default()
            .with_max_record_plaintext(0)
            .is_err()
    );
    assert!(
        TunnelConfig::default()
            .with_max_record_plaintext(MAX_RECORD_PLAINTEXT + 1)
            .is_err()
    );
    assert_eq!(MAX_RECORD_PLAINTEXT, 65535 - 16);
    let config = TunnelConfig::default()
        .with_max_record_plaintext(MAX_RECORD_PLAINTEXT)
        .unwrap();
    assert_eq!(config.max_record_plaintext(), MAX_RECORD_PLAINTEXT);
    assert_eq!(TunnelConfig::default().max_record_plaintext(), 16 * 1024);
}

#[test]
fn static_keypair_derives_public_from_secret_and_redacts() {
    let pair = StaticKeypair::generate().unwrap();
    let again = StaticKeypair::from_secret(*pair.secret()).unwrap();
    assert_eq!(pair.public(), again.public());
    assert_ne!(pair.public(), &[0u8; 32]);
    let debug = format!("{pair:?}");
    assert!(!debug.contains(&format!("{:?}", pair.secret())));

    let psk = Psk::generate().unwrap();
    assert_ne!(psk.as_bytes(), Psk::generate().unwrap().as_bytes());
    assert_eq!(Psk::from_bytes(*psk.as_bytes()).as_bytes(), psk.as_bytes());
    assert!(format!("{psk:?}").contains("redacted"));
}
