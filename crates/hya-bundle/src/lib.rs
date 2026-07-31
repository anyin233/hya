//! Native AgentBundle source preparation and prepared-catalog decoding.
//!
//! Source parsing and filesystem access stop at this crate. Runtime callers
//! consume only the immutable prepared data returned by [`prepare_builtins`].

mod catalog;
mod error;
mod model;
mod prepare;
mod source;

pub use catalog::{BundleCatalog, ExportKind};
pub use error::BundleError;
pub use model::{
    AgentRole, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy, PreparedAgent,
    PreparedBundle, PreparedBundleIndex, PreparedCatalog, PreparedResource, ResourceView,
    SpawnLifecycle,
};
pub use prepare::prepare_builtins;
pub use source::{BundleSource, SourceFile};
