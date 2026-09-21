//! Version pin for the Bun/TypeScript bundle extension adapter.
//!
//! This crate does not implement protocol logic. The adapter itself lives in
//! [`adapter/`](../../adapter/) as a Bun/TypeScript package speaking the hya
//! plugin ABI v1 (NDJSON JSON-RPC 2.0 over stdio; see
//! `docs/plugin-protocol.md`). This constant publishes the adapter version the
//! host and the adapter must agree on:
//!
//! - [`BUN_ADAPTER_VERSION`] — `@hya/bun-adapter` package version
//!
//! Bumping the constant without also shipping the matching adapter (and
//! re-verified extension load) breaks bundle extension activation. Treat a
//! change here as a coordinated release.

/// Pinned `@hya/bun-adapter` version the Bun adapter ships as.
pub const BUN_ADAPTER_VERSION: &str = "1.0.0";
