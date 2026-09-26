//! Host control streams: challenge, registration, `incoming`, heartbeats.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signature, VerifyingKey};
use futures::{SinkExt, StreamExt};
use tokio::time::{Instant, sleep, timeout};

use super::{Inner, PeerInfo, random_bytes, shutting_down};
use crate::link::RoomId;
use crate::proto::{
    Challenge, Heartbeat, HostFrame, Incoming, ProxyToHost, Register, Registered, RelayError,
    RelayErrorCode, host_frame, proxy_to_host, register_signing_message,
};
use crate::transport::ProxyControlTransport;

/// How long the proxy tries to deliver a final error frame.
const ERROR_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Check a registration against the challenge nonce; the room it owns.
pub(crate) fn verify_registration(nonce: &[u8], register: &Register) -> Result<RoomId, RelayError> {
    let unauthenticated = |message: &str| RelayError::new(RelayErrorCode::Unauthenticated, message);
    let key: [u8; 32] = register
        .ed25519_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| unauthenticated("ed25519 public key must be 32 bytes"))?;
    let signature: [u8; 64] = register
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| unauthenticated("ed25519 signature must be 64 bytes"))?;
    let verifying_key =
        VerifyingKey::from_bytes(&key).map_err(|_| unauthenticated("invalid ed25519 key"))?;
    verifying_key
        .verify_strict(
            &register_signing_message(nonce),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| unauthenticated("registration signature does not verify"))?;
    Ok(RoomId::from_ed25519(&key))
}

fn frame(frame: proxy_to_host::Frame) -> ProxyToHost {
    ProxyToHost { frame: Some(frame) }
}

/// Why a send to the host failed.
enum SendFailure {
    /// The host is gone.
    Closed,
    /// The host did not take the frame within the idle timeout.
    Stalled,
}

async fn send(
    inner: &Inner,
    transport: &mut ProxyControlTransport,
    message: proxy_to_host::Frame,
) -> Result<(), SendFailure> {
    match timeout(inner.limits.idle_timeout, transport.send(frame(message))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(SendFailure::Closed),
        Err(_) => Err(SendFailure::Stalled),
    }
}

/// Send a final error frame and close the sending direction (best effort).
async fn fail(transport: &mut ProxyControlTransport, error: RelayError) {
    let _ = timeout(ERROR_SEND_TIMEOUT, async {
        transport
            .send(frame(proxy_to_host::Frame::Error(error)))
            .await?;
        transport.close().await
    })
    .await;
}

pub(crate) async fn run_host(
    inner: Arc<Inner>,
    mut transport: ProxyControlTransport,
    _peer: PeerInfo,
) {
    if inner.shutdown.is_cancelled() {
        return fail(&mut transport, shutting_down()).await;
    }
    let nonce = random_bytes::<32>();
    let challenge = proxy_to_host::Frame::Challenge(Challenge {
        nonce: nonce.to_vec(),
    });
    if send(&inner, &mut transport, challenge).await.is_err() {
        return;
    }

    let first = tokio::select! {
        biased;
        () = inner.shutdown.cancelled() => return fail(&mut transport, shutting_down()).await,
        first = timeout(inner.limits.handshake_timeout, transport.next()) => first,
    };
    let register = match first {
        Err(_) => {
            let error = RelayError::new(RelayErrorCode::DeadlineExceeded, "registration timed out");
            return fail(&mut transport, error).await;
        }
        Ok(None | Some(Err(_))) => return,
        Ok(Some(Ok(HostFrame {
            frame: Some(host_frame::Frame::Register(register)),
        }))) => register,
        Ok(Some(Ok(_))) => {
            let error = RelayError::new(
                RelayErrorCode::InvalidArgument,
                "the first host frame must be `register`",
            );
            return fail(&mut transport, error).await;
        }
    };
    let room_id = match verify_registration(&nonce, &register) {
        Ok(room_id) => room_id,
        Err(error) => return fail(&mut transport, error).await,
    };
    let (registration, mut incoming) = match inner.register_room(&room_id) {
        Ok(registered) => registered,
        Err(error) => return fail(&mut transport, error).await,
    };
    let room = registration.room.clone();
    let registered = proxy_to_host::Frame::Registered(Registered {
        room_id: room_id.as_str().to_owned(),
    });
    if send(&inner, &mut transport, registered).await.is_err() {
        return;
    }

    let idle_timeout = inner.limits.idle_timeout;
    let idle = sleep(idle_timeout);
    tokio::pin!(idle);
    let outcome: Option<RelayError> = loop {
        tokio::select! {
            biased;
            () = room.token.cancelled() => break Some(inner.host_closed_error(&room)),
            () = &mut idle => {
                break Some(RelayError::new(
                    RelayErrorCode::DeadlineExceeded,
                    "host control stream idle",
                ));
            }
            Some(stream_id) = incoming.recv() => {
                let announce = proxy_to_host::Frame::Incoming(Incoming { stream_id });
                match send(&inner, &mut transport, announce).await {
                    Ok(()) => {}
                    Err(SendFailure::Closed) => break None,
                    Err(SendFailure::Stalled) => break Some(stalled()),
                }
            }
            next = transport.next() => {
                let Some(Ok(message)) = next else { break None };
                idle.as_mut().reset(Instant::now() + idle_timeout);
                match message.frame {
                    Some(host_frame::Frame::Heartbeat(Heartbeat { seq, pong: false })) => {
                        let pong = proxy_to_host::Frame::Heartbeat(Heartbeat { seq, pong: true });
                        match send(&inner, &mut transport, pong).await {
                            Ok(()) => {}
                            Err(SendFailure::Closed) => break None,
                            Err(SendFailure::Stalled) => break Some(stalled()),
                        }
                    }
                    Some(host_frame::Frame::Heartbeat(_)) => {}
                    Some(host_frame::Frame::Register(_)) => {
                        break Some(RelayError::new(
                            RelayErrorCode::FailedPrecondition,
                            "the room is already registered on this stream",
                        ));
                    }
                    None => {
                        break Some(RelayError::new(
                            RelayErrorCode::InvalidArgument,
                            "empty host frame",
                        ));
                    }
                }
            }
        }
    };
    // Evict the room first so its streams close promptly.
    drop(registration);
    if let Some(error) = outcome {
        fail(&mut transport, error).await;
    }
}

fn stalled() -> RelayError {
    RelayError::new(
        RelayErrorCode::DeadlineExceeded,
        "host stopped reading its control stream",
    )
}
