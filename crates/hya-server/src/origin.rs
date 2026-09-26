//! Where a request came from (ADR-0025 D5).
//!
//! Requests the relay host connector serves carry the axum request
//! extension [`Origin::Relay`]; the TCP listener adds nothing, so a request
//! without the extension is [`Origin::Local`]. A request cannot forge the
//! extension: it is set by the server, never parsed from the wire.
//! Relay-control rpcs and the process stop/upgrade rpcs refuse relay-origin
//! requests with `permission_denied`; everything else is allowed, because
//! holding the relay link means owner trust.

use axum::http::Extensions;

/// The origin of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A direct connection to the server's own listener (or an in-process
    /// call, such as the gRPC binding's dispatch).
    Local,
    /// A connection that arrived through the secure relay.
    Relay,
}

impl Origin {
    /// The origin recorded in a request's extensions ([`Origin::Local`]
    /// when none is).
    #[must_use]
    pub fn of(extensions: &Extensions) -> Self {
        extensions.get::<Origin>().copied().unwrap_or(Origin::Local)
    }
}
