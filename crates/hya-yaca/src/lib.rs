//! `hya-yaca` — native in-process transport from the `hya` frontend to the `yaca` backend.
//!
//! Builds `yaca_server::router(AppState)` in-process (via `yaca_app::YacaRuntime`) and drives it
//! with `tower::ServiceExt::oneshot` instead of HTTP/reqwest, plus an in-process `/global/event`
//! bridge. No TCP, no reqwest. Public surface (`YacaNativeTransport`, `spawn_event_bridge`) filled
//! in during Phase 2.
