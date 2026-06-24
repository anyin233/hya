//! `yaca-app` — backend bootstrap library extracted from the `yaca-cli` binary.
//!
//! Holds the engine/`AppState` assembly (config, providers, MCP, plugins, agent, store) so both
//! `yaca-cli` (server + headless commands) and `hya-yaca` (in-process native transport) can build
//! the backend without the binary. Public surface filled in during Phase 1.
