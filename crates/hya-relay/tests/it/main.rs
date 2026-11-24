//! Integration tests for `hya-relay`, built as one binary; each module covers one area.

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
