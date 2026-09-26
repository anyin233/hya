//! Host control streams: challenge, registration, `incoming`, heartbeats.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signature, VerifyingKey};
use futures::{SinkExt, StreamExt};
use tokio::time::{Instant, sleep, timeout};

use tokio::sync::mpsc;

use super::{Inner, PeerInfo, RoomRegistration, random_bytes, shutting_down};
use crate::link::RoomId;
use crate::proto::{
    Challenge, Heartbeat, HostFrame, Incoming, OpenTokenUpdated, ProxyToHost, Register, Registered,
    RelayError, RelayErrorCode, UpdateOpenToken, host_frame, proxy_to_host,
    register_signing_message, update_open_token_signing_message,
};
use crate::transport::ProxyControlTransport;

/// How long the proxy tries to deliver a final error frame.
const ERROR_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// A verified registration.
pub(crate) struct Verified {
    room_id: RoomId,
    key: VerifyingKey,
    open_token_hash: [u8; 32],
}

fn unauthenticated(message: &str) -> RelayError {
    RelayError::new(RelayErrorCode::Unauthenticated, message)
}

fn token_hash(bytes: &[u8]) -> Result<[u8; 32], RelayError> {
    bytes.try_into().map_err(|_| {
        RelayError::new(
            RelayErrorCode::InvalidArgument,
            "open_token_hash must be 32 bytes (upgrade the host)",
        )
    })
}

fn signature(bytes: &[u8]) -> Result<Signature, RelayError> {
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| unauthenticated("ed25519 signature must be 64 bytes"))?;
    Ok(Signature::from_bytes(&bytes))
}

/// Check a registration against the challenge nonce: the room it owns, its
/// key, and its open token hash.
pub(crate) fn verify_registration(
    nonce: &[u8],
    register: &Register,
) -> Result<Verified, RelayError> {
    let open_token_hash = token_hash(&register.open_token_hash)?;
    let key: [u8; 32] = register
        .ed25519_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| unauthenticated("ed25519 public key must be 32 bytes"))?;
    let signature = signature(&register.signature)?;
    let verifying_key =
        VerifyingKey::from_bytes(&key).map_err(|_| unauthenticated("invalid ed25519 key"))?;
    verifying_key
        .verify_strict(
            &register_signing_message(nonce, &open_token_hash),
            &signature,
        )
        .map_err(|_| unauthenticated("registration signature does not verify"))?;
    Ok(Verified {
        room_id: RoomId::from_ed25519(&key),
        key: verifying_key,
        open_token_hash,
    })
}

/// Check an open token update against the room key and the challenge
/// nonce of this control stream; the new hash.
fn verify_update(
    nonce: &[u8],
    key: &VerifyingKey,
    update: &UpdateOpenToken,
) -> Result<[u8; 32], RelayError> {
    let open_token_hash = token_hash(&update.open_token_hash)?;
    let signature = signature(&update.signature)?;
    key.verify_strict(
        &update_open_token_signing_message(nonce, &open_token_hash),
        &signature,
    )
    .map_err(|_| unauthenticated("open token update signature does not verify"))?;
    Ok(open_token_hash)
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

/// A registered room as seen by its control stream.
struct Registration {
    registration: RoomRegistration,
    incoming: mpsc::Receiver<String>,
    room_id: RoomId,
    key: VerifyingKey,
    nonce: [u8; 32],
}

/// Challenge the host and register its room.
///
/// `Err(None)` means the host went away (nothing to report).
async fn register(
    inner: &Arc<Inner>,
    transport: &mut ProxyControlTransport,
    peer: &PeerInfo,
) -> Result<Registration, Option<RelayError>> {
    let nonce = random_bytes::<32>();
    let challenge = proxy_to_host::Frame::Challenge(Challenge {
        nonce: nonce.to_vec(),
    });
    if send(inner, transport, challenge).await.is_err() {
        return Err(None);
    }

    let first = tokio::select! {
        biased;
        () = inner.shutdown.cancelled() => return Err(Some(shutting_down())),
        first = timeout(inner.limits.handshake_timeout, transport.next()) => first,
    };
    let register = match first {
        Err(_) => {
            return Err(Some(RelayError::new(
                RelayErrorCode::DeadlineExceeded,
                "registration timed out",
            )));
        }
        Ok(None | Some(Err(_))) => return Err(None),
        Ok(Some(Ok(HostFrame {
            frame: Some(host_frame::Frame::Register(register)),
        }))) => register,
        Ok(Some(Ok(_))) => {
            return Err(Some(RelayError::new(
                RelayErrorCode::InvalidArgument,
                "the first host frame must be `register`",
            )));
        }
    };
    let verified = verify_registration(&nonce, &register)?;
    let (registration, incoming) =
        inner.register_room(&verified.room_id, verified.open_token_hash, peer)?;
    Ok(Registration {
        registration,
        incoming,
        room_id: verified.room_id,
        key: verified.key,
        nonce,
    })
}

pub(crate) async fn run_host(
    inner: Arc<Inner>,
    mut transport: ProxyControlTransport,
    peer: PeerInfo,
) {
    if inner.shutdown.is_cancelled() {
        return fail(&mut transport, shutting_down()).await;
    }
    let pending = match inner.begin_registration(&peer) {
        Ok(pending) => pending,
        Err(error) => return fail(&mut transport, error).await,
    };
    let registered = register(&inner, &mut transport, &peer).await;
    // The registration is decided: free the pending slot before reporting.
    drop(pending);
    let Registration {
        registration,
        mut incoming,
        room_id,
        key,
        nonce,
    } = match registered {
        Ok(registered) => registered,
        Err(Some(error)) => return fail(&mut transport, error).await,
        Err(None) => return,
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
                    Some(host_frame::Frame::UpdateOpenToken(update)) => {
                        match verify_update(&nonce, &key, &update) {
                            Ok(hash) => inner.update_open_token(&room, hash),
                            Err(error) => break Some(error),
                        }
                        let updated = proxy_to_host::Frame::OpenTokenUpdated(OpenTokenUpdated {});
                        match send(&inner, &mut transport, updated).await {
                            Ok(()) => {}
                            Err(SendFailure::Closed) => break None,
                            Err(SendFailure::Stalled) => break Some(stalled()),
                        }
                    }
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
