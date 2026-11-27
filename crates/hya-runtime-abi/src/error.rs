//! Error types shared across the native ABI v2 host/bundle boundary.

use std::fmt;

use hya_tool_error::ToolError;
use serde::{Deserialize, Serialize};

/// Typed errors returned by [`crate::HostRuntime`] services and native
/// extension traits across the ABI v2 boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
pub enum HostError {
    /// Requested session, state entry, agent, skill, or bundle was not found.
    NotFound(String),
    /// Optimistic concurrency or claim ownership conflict.
    Conflict(String),
    /// Claim epoch or lease has expired or been superseded.
    StaleClaim(String),
    /// Caller supplied malformed or out-of-range arguments.
    InvalidInput(String),
    /// Caller lacks permission for the requested operation.
    PermissionDenied(String),
    /// Backing service, transport, or model route is unavailable.
    Unavailable(String),
    /// Cooperative cancellation triggered.
    Cancelled,
    /// Persistence or state storage error.
    Storage(String),
    /// Uncategorized internal error.
    Internal(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::StaleClaim(msg) => write!(f, "stale claim: {msg}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            Self::Unavailable(msg) => write!(f, "unavailable: {msg}"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Storage(msg) => write!(f, "storage error: {msg}"),
            Self::Internal(msg) => write!(f, "internal host error: {msg}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<ToolError> for HostError {
    fn from(error: ToolError) -> Self {
        match error {
            ToolError::Input(msg) => Self::InvalidInput(msg),
            ToolError::Permission(err) => Self::PermissionDenied(err.to_string()),
            ToolError::Cancelled => Self::Cancelled,
            ToolError::Overloaded(msg) => Self::Unavailable(msg),
            ToolError::Io(err) => Self::Storage(err.to_string()),
            ToolError::Json(err) => Self::InvalidInput(err.to_string()),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<HostError> for ToolError {
    fn from(error: HostError) -> Self {
        match error {
            HostError::InvalidInput(msg) => Self::Input(msg),
            HostError::Cancelled => Self::Cancelled,
            HostError::Unavailable(msg) => Self::Overloaded(msg),
            other => Self::Other(other.to_string()),
        }
    }
}

/// Typed errors produced while resolving, verifying, or loading a native
/// bundle library through ABI v2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AbiError {
    /// Bundle identity is malformed or lacks the expected namespace prefix.
    InvalidBundleIdentity(String),
    /// Requested bundle is disabled by policy or missing on disk.
    DisabledOrMissing {
        /// Bundle identity or library stem.
        bundle: String,
        /// Human-readable reason.
        detail: String,
    },
    /// Dynamic library failed to open (`dlopen`) or extract.
    LoadFailed {
        /// Bundle identity or library stem.
        bundle: String,
        /// Dynamic loader diagnostic.
        detail: String,
    },
    /// Required ABI v2 C symbol was absent from the library.
    MissingSymbol {
        /// Bundle identity or library stem.
        bundle: String,
        /// Missing symbol name.
        symbol: String,
    },
    /// Library's `hya_native_abi_v2` digest did not match this host build.
    DigestMismatch {
        /// Bundle identity or library stem.
        bundle: String,
    },
    /// `hya_native_register_v2` returned a failure status or null check failed.
    RegistrationFailed {
        /// Bundle identity or library stem.
        bundle: String,
        /// Diagnostic reported by the bundle or bridge.
        detail: String,
    },
    /// Runtime bridge failed to enter or poll a bundle future.
    RuntimeBridge(String),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBundleIdentity(identity) => {
                write!(f, "invalid native bundle identity: {identity}")
            }
            Self::DisabledOrMissing { bundle, detail } => {
                write!(f, "native bundle `{bundle}` unavailable: {detail}")
            }
            Self::LoadFailed { bundle, detail } => {
                write!(f, "failed to load native bundle `{bundle}`: {detail}")
            }
            Self::MissingSymbol { bundle, symbol } => {
                write!(f, "native bundle `{bundle}` missing symbol `{symbol}`")
            }
            Self::DigestMismatch { bundle } => {
                write!(
                    f,
                    "native bundle `{bundle}` was built for a different hya-runtime-abi v2 ABI"
                )
            }
            Self::RegistrationFailed { bundle, detail } => {
                write!(
                    f,
                    "native bundle `{bundle}` registration failed: {detail}"
                )
            }
            Self::RuntimeBridge(detail) => {
                write!(f, "native runtime bridge error: {detail}")
            }
        }
    }
}

impl std::error::Error for AbiError {}
