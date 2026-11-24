//! Unified hya release version and compatibility minimum.

/// The unified release version of shipped `hya` (backend, bundles, and TUI/WebUI).
pub const VERSION: &str = env!("HYA_VERSION");
/// Alias for [`VERSION`] retained for backend crates.
pub const BACKEND_VERSION: &str = VERSION;
/// Alias for [`VERSION`] retained for frontend consumers.
pub const FRONTEND_VERSION: &str = VERSION;

/// The minimum backend version required by the current frontend.
pub const MINIMUM_BACKEND_VERSION: &str = env!("HYA_MINIMUM_BACKEND_VERSION");

/// `rustc -V` of the compiler that built this crate. Rust has no stable ABI
/// across compilers, so natively loaded libraries must report the same one.
pub const RUSTC: &str = env!("HYA_RUSTC");
