//! `hya-api` — the hya v1 dual-protocol contract crate.
//!
//! This crate is the single source of truth binding shared by the HTTP
//! (axum, JSON+SSE) and gRPC (tonic) transports. It contains:
//!
//! - [`v1`] — generated Rust types, clients, and servers for the `hya.v1`
//!   protobuf services, plus canonical protojson serde implementations.
//!   Regenerate with `cargo xtask gen-api`; the output is committed so
//!   normal builds never need `protoc`.
//! - [`error`] — the stable error-code table mapped to HTTP statuses and
//!   gRPC status codes.
//! - [`cursor`] — opaque pagination cursor helpers for list operations.
//!
//! The crate intentionally has no runtime dependencies beyond the protobuf
//! stack: business logic lives behind the engine/app control handles, and
//! both transport bindings call the same service implementations.

pub mod cursor;
pub mod error;

/// Generated protobuf code for the `hya.v1` contract.
///
/// The `#![allow]` inner attributes keep generated code outside the
/// hand-written lint contract (doc coverage and pedantic clippy gates
/// apply to authored code, not codegen output).
pub mod generated {
    #![allow(missing_docs, clippy::all)]

    /// The `hya` proto package.
    pub mod hya {
        /// The `hya.v1` proto package: all v1 services and messages, plus
        /// canonical protojson serde implementations. Well-known protobuf
        /// types resolve to the `pbjson_types` crate.
        pub mod v1 {
            include!("gen/hya.v1.rs");
            include!("gen/hya.v1.serde.rs");
        }
    }
}

pub use generated::hya::v1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_contract_types_are_reachable() {
        // Compile-time reachability check for a representative type from
        // each generated family; keeps the include tree honest.
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<v1::SessionInfo>();
        assert_debug::<v1::CreateTurnRequest>();
        assert_debug::<v1::StreamFrame>();
        assert_debug::<v1::Interaction>();
        assert_debug::<v1::WorkflowState>();
        assert_debug::<v1::TurnInfo>();
    }
}
