//! Minimal execution kernel: the three-primitive turn loop over the session
//! event log (ADR-0033), the internal native bundle loader, and the shared
//! [`CoreError`].
//!
//! The full-mode session engine runs on this kernel from `hya-engine`.

/// Loading the internal native bundles.
pub mod bundles;
/// Shared error type for the kernel and the engine built on it.
pub mod error;
/// Minimal execution kernel.
pub mod kernel;

pub use error::CoreError;
