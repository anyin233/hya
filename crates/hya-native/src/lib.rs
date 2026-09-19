//! `hya-native` — native in-process transport from the `hya` frontend to the `hya` backend.
//!
//! Builds `hya_server::router(AppState)` in-process (via `hya_app::HyaRuntime`) and drives it
//! with `tower::ServiceExt::oneshot` instead of HTTP/reqwest ([`HyaNativeTransport`]).
//! No TCP, no reqwest — the Rust analogue of compat's in-process `app.fetch`.

mod transport;

pub use transport::{HyaNativeClient, HyaNativeTransport};
