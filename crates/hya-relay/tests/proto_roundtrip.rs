#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `hya.relay.v1` messages encode and decode losslessly; the same bytes serve
//! the gRPC and WebSocket bindings.

use hya_relay::proto::{
    Accept, Challenge, Chunk, Close, Heartbeat, HostFrame, Incoming, Open, Opened, ProxyToHost,
    Register, Registered, RelayError, RelayErrorCode, chunk, host_frame, proxy_to_host,
};
use prost::Message;

fn round_trip<M: Message + Default + PartialEq + std::fmt::Debug>(message: M) {
    let bytes = message.encode_to_vec();
    let decoded = M::decode(bytes.as_slice()).unwrap();
    assert_eq!(decoded, message);
}

#[test]
fn host_frames_round_trip() {
    round_trip(HostFrame {
        frame: Some(host_frame::Frame::Register(Register {
            ed25519_pubkey: vec![7; 32],
            signature: vec![9; 64],
            open_token_hash: vec![3; 32],
        })),
    });
    round_trip(HostFrame {
        frame: Some(host_frame::Frame::UpdateOpenToken(
            hya_relay::proto::UpdateOpenToken {
                open_token_hash: vec![4; 32],
                signature: vec![5; 64],
            },
        )),
    });
    round_trip(HostFrame {
        frame: Some(host_frame::Frame::Heartbeat(Heartbeat {
            seq: 3,
            pong: true,
        })),
    });
}

#[test]
fn proxy_to_host_frames_round_trip() {
    for frame in [
        proxy_to_host::Frame::Challenge(Challenge { nonce: vec![1; 32] }),
        proxy_to_host::Frame::Registered(Registered {
            room_id: "abcdefghijklmnopqrstuvwxyz".into(),
        }),
        proxy_to_host::Frame::Heartbeat(Heartbeat {
            seq: 1,
            pong: false,
        }),
        proxy_to_host::Frame::Incoming(Incoming {
            stream_id: "s-1".into(),
        }),
        proxy_to_host::Frame::Error(RelayError {
            code: RelayErrorCode::AlreadyExists as i32,
            message: "room taken".into(),
        }),
        proxy_to_host::Frame::OpenTokenUpdated(hya_relay::proto::OpenTokenUpdated {}),
    ] {
        round_trip(ProxyToHost { frame: Some(frame) });
    }
}

#[test]
fn chunk_frames_round_trip() {
    for frame in [
        chunk::Frame::Open(Open {
            room_id: "abcdefghijklmnopqrstuvwxyz".into(),
            open_token: vec![6; 32],
        }),
        chunk::Frame::Accept(Accept {
            stream_id: "s-1".into(),
        }),
        chunk::Frame::Data(vec![0, 1, 2, 255]),
        chunk::Frame::Close(Close {}),
        chunk::Frame::Heartbeat(Heartbeat { seq: 9, pong: true }),
        chunk::Frame::Error(RelayError {
            code: RelayErrorCode::NotFound as i32,
            message: "room offline".into(),
        }),
        chunk::Frame::Opened(Opened {}),
    ] {
        round_trip(Chunk { frame: Some(frame) });
    }
}

#[test]
fn opened_ack_uses_a_new_field_number() {
    // `opened` is field 7 of `Chunk`: tag byte (7 << 3) | 2 = 0x3a, length 0.
    let bytes = Chunk {
        frame: Some(chunk::Frame::Opened(Opened {})),
    }
    .encode_to_vec();
    assert_eq!(bytes, vec![0x3a, 0x00]);
}

#[test]
fn error_codes_mirror_grpc_status_codes() {
    use hya_relay::proto::relay_error_code_to_grpc;
    assert_eq!(
        relay_error_code_to_grpc(RelayErrorCode::NotFound),
        tonic::Code::NotFound
    );
    assert_eq!(
        relay_error_code_to_grpc(RelayErrorCode::Unauthenticated),
        tonic::Code::Unauthenticated
    );
    assert_eq!(
        relay_error_code_to_grpc(RelayErrorCode::ResourceExhausted),
        tonic::Code::ResourceExhausted
    );
    assert_eq!(
        relay_error_code_to_grpc(RelayErrorCode::Unspecified),
        tonic::Code::Unknown
    );
    for code in [
        tonic::Code::NotFound,
        tonic::Code::AlreadyExists,
        tonic::Code::Unavailable,
        tonic::Code::InvalidArgument,
    ] {
        assert_eq!(
            relay_error_code_to_grpc(hya_relay::proto::relay_error_code_from_grpc(code)),
            code
        );
    }
}

#[test]
fn register_signing_message_is_domain_separated_and_versioned() {
    let nonce = [5u8; 32];
    let hash = [9u8; 32];
    let message = hya_relay::proto::register_signing_message(&nonce, &hash);
    assert!(message.starts_with(b"hya.relay.v1/register/v2\0"));
    assert_eq!(message.len(), b"hya.relay.v1/register/v2\0".len() + 32 + 32);
    assert_eq!(&message[message.len() - 64..message.len() - 32], &nonce);
    assert!(message.ends_with(&hash));
    // Never a v1 message (context || nonce) or an open-token update.
    assert!(!message.starts_with(b"hya.relay.v1/register\0"));
    let update = hya_relay::proto::update_open_token_signing_message(&nonce, &hash);
    assert!(update.starts_with(b"hya.relay.v1/update-open-token/v1\0"));
    assert!(update.ends_with(&hash));
    assert_ne!(update, message);
    // A different hash signs different bytes.
    assert_ne!(
        hya_relay::proto::register_signing_message(&nonce, &[8u8; 32]),
        message
    );
}
