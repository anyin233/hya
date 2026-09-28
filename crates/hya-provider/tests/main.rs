//! Integration tests for `hya-provider`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod catalog;
mod catalog_discovery;
mod conformance;
mod http_headers;
mod multiprovider;
mod stream_error_frames;
mod structured_tool_error;
