//! Rust compatibility export for the Bun bundle extension adapter.
//!
//! The TypeScript adapter lives under `adapter/` and speaks the hya plugin ABI v1
//! over NDJSON JSON-RPC stdio. See `docs/plugin-protocol.md`.

/// Historical adapter version constant.
#[deprecated(note = "Query the adapter entry point with --version")]
pub const BUN_ADAPTER_VERSION: &str = "1.0.0";
