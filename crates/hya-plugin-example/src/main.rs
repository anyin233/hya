//! Minimal native plugin binary used as a build/fixture stub.
//!
//! This is intentionally a no-op process (`main` returns immediately). It is
//! **not** a full JSON-RPC stdio plugin: it does not speak `initialize`, hooks,
//! or `tool/call` on stdin/stdout. Use it as a concrete crate target for host
//! packaging tests and as a scaffold when authoring a real plugin binary.

// Fully documented; keep it that way. Removed when the workspace lint
// table is promoted from `warn` to `deny`.
#![deny(missing_docs)]

fn main() {}
