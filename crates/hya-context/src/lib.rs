//! Context-window accounting and the native rewrite extension contract.

pub mod rewrite;
pub mod tokens;

/// Source inputs for the lockstep native bundle ABI digest.
#[doc(hidden)]
pub const NATIVE_ABI_SOURCES: [&[u8]; 2] =
    [include_bytes!("rewrite.rs"), include_bytes!("tokens.rs")];
