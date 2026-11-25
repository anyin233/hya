//! Typed contract for offline Claude Code plugin translation.
//!
//! The Bun/TypeScript adapter under `adapter/` translates Claude plugin directories
//! and marketplace entries. It also runs `kind: claude` hooks using the hya plugin
//! ABI v1 over NDJSON JSON-RPC stdio. See `docs/claude-plugin-import.md`.
//!
//! [`emit`] decodes the `--emit-bundle-manifest` result for
//! `hya bundle install --claude`.

pub mod emit;
