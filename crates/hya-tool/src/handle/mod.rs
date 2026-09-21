//! Internal resource URLs for agent-owned payloads.
//!
//! A handle is a short, stable name for something the agent produced or owns:
//! spilled tool output (`artifact://`), a skill body (`skill://`), a scratch
//! payload (`local://`). Tools put a handle in their result instead of a large
//! body, and the model retrieves the body — or a slice of it — only when it
//! actually needs it.
//!
//! These URLs index *agent* resources, not the workspace. `read`, `write`,
//! `grep` and `bash` keep treating every ordinary path exactly as they did;
//! nothing here changes how a filesystem path behaves. A tool opts into handles
//! by asking the router, and [`HandleRef::looks_like_handle`] is what keeps the
//! two namespaces from overlapping.

mod artifact;
mod plane;
mod reference;
mod registry;
mod router;

pub use artifact::{ArtifactHook, ArtifactId, ArtifactMeta, ArtifactStore};
pub use plane::ArtifactPlane;
pub use reference::{HandleRef, HandleScheme, Projection};
pub use registry::{
    SchemeBinding, SchemeDispatch, SchemeHandler, SchemeReadTool, SchemeRegistry,
    SchemeRegistryError, SchemeWriteTool,
};
pub use router::{HandleContent, HandleRouter};

/// Why a handle could not be parsed or resolved.
#[derive(Debug, thiserror::Error)]
pub enum HandleError {
    /// The text carries no `scheme://` prefix at all.
    ///
    /// Distinct from [`HandleError::UnknownScheme`] on purpose: this is the
    /// answer for an ordinary filesystem path, and callers use it to fall back
    /// to path handling rather than to report a failure.
    #[error("not an internal handle: {0}")]
    NotAHandle(String),
    /// A `scheme://` prefix that is not one of the shipped families.
    #[error("unknown handle scheme: {0}://")]
    UnknownScheme(String),
    /// Empty, absolute, or traversing resource path.
    #[error("malformed handle path: {0}")]
    MalformedPath(String),
    /// Query string that is not a supported projection.
    #[error("malformed handle query: {0}")]
    MalformedQuery(String),
    /// Query key outside the supported set.
    #[error("unknown handle query key: {0}")]
    UnknownQueryKey(String),
    /// The scheme is known but nothing is registered to resolve it.
    #[error("no resolver registered for {0}://")]
    NoResolver(HandleScheme),
    /// The resource does not exist.
    #[error("handle not found: {0}")]
    NotFound(String),
    /// A hook in the chain failed.
    #[error("artifact hook {hook} failed: {message}")]
    Hook {
        /// Hook name, for locating the offending registration.
        hook: String,
        /// Bounded diagnostic.
        message: String,
    },
    /// The scheme is known, but its resources are not writable.
    ///
    /// Distinct from [`HandleError::UnknownScheme`]: `artifact://` is a real
    /// family whose stored bytes are deliberately immutable, and reporting it
    /// as unknown would send a caller looking for a typo.
    #[error("{0}:// is read-only")]
    NotWritable(HandleScheme),
    /// A registered external scheme has not declared write access.
    ///
    /// The string form mirrors [`HandleError::NotWritable`]; the payload is the
    /// scheme token itself because external schemes are not part of the closed
    /// [`HandleScheme`] set.
    #[error("{0}:// is read-only")]
    SchemeNotWritable(String),
    /// Storage or retrieval I/O failure.
    #[error("handle io: {0}")]
    Io(#[from] std::io::Error),
}
