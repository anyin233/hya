//! `hya-native` — native in-process transport from the `hya` frontend to the `hya` backend.
//!
//! Builds `hya_server::router(AppState)` in-process (via [`hya_app::HyaRuntime`]) and drives it
//! with `tower::ServiceExt::oneshot` instead of HTTP/reqwest ([`HyaNativeTransport`]), plus an
//! in-process `/global/event` bridge ([`spawn_event_bridge`]). No TCP, no reqwest — the Rust
//! analogue of opencode's in-process `app.fetch`.

mod events;
mod transport;

pub use events::spawn_event_bridge;
pub use transport::{HyaNativeClient, HyaNativeTransport};
