//! `hya-relay` — building blocks for the hya secure relay (`hya proxy`).
//!
//! The relay is a blind rendezvous: a backend registers a room it owns, a
//! client opens a stream to that room, and the proxy splices the two byte
//! streams. The payload is end-to-end encrypted by the endpoints, so the
//! proxy sees room ids, stream ids, and ciphertext only. See `docs/relay.md`.
//!
//! - [`proto`] — generated `hya.relay.v1` messages and the tonic `Relay`
//!   service, plus small helpers (error-code mapping, the registration
//!   signing message). Regenerate with `cargo run -p xtask -- gen-relay`;
//!   the output is committed so normal builds never need `protoc`.
//! - [`transport`] — [`transport::RelayTransport`], the bidirectional message
//!   stream the relay state machines are written against, independent of the
//!   gRPC or WebSocket binding, plus an in-memory pair for tests.
//! - [`link`] — [`link::RelayLink`], the `hya://` relay link (the client
//!   credential), and [`link::RoomId`] derivation.
//! - [`keys`] — the Noise static X25519 keypair and the PSK.
//! - [`tunnel`] — [`tunnel::NoiseStream`], the Noise `NKpsk0` tunnel over a
//!   data stream, exposed as a tokio byte stream.
//!
//! This crate deliberately depends on no hya runtime crate.

pub mod keys;
pub mod link;
pub mod proto;
pub mod transport;
pub mod tunnel;
