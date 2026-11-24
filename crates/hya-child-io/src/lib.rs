//! Bounded framing and completion tracking for connection I/O.

/// NDJSON framing for stdio transports.
pub mod codec;
/// Composition tracking for connection readers and cleanup tasks.
pub mod io_tasks;
