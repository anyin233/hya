//! `hya-yaca` — native in-process transport from the `hya` frontend to the `yaca` backend.
//!
//! Builds `yaca_server::router(AppState)` in-process (via [`yaca_app::YacaRuntime`]) and drives it
//! with `tower::ServiceExt::oneshot` instead of HTTP/reqwest ([`YacaNativeTransport`]), plus an
//! in-process `/global/event` bridge ([`spawn_event_bridge`]). No TCP, no reqwest — the Rust
//! analogue of opencode's in-process `app.fetch`.

mod events;
mod transport;

pub use events::spawn_event_bridge;
pub use transport::{YacaNativeClient, YacaNativeTransport};
