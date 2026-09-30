//! Aggregate backend and frontend release versions.

/// The release version of the shipped `hya` backend.
pub const BACKEND_VERSION: &str = env!("HYA_BACKEND_VERSION");
/// The release version of the shipped Bun/OpenTUI frontend.
pub const FRONTEND_VERSION: &str = env!("HYA_FRONTEND_VERSION");
