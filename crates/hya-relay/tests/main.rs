//! Integration tests for `hya-relay`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod support;

mod client;
mod conformance;
mod independence;
mod link;
mod memory_transport;
mod proto_roundtrip;
mod proxy_core;
mod server;
mod tunnel;
