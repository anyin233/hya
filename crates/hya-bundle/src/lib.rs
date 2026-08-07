//! Native AgentBundle source preparation and prepared-catalog decoding.
//!
//! Source parsing and filesystem access stop at this crate. Runtime callers
//! consume only the immutable prepared data returned by [`prepare_package`].

mod catalog;
mod error;
mod model;
mod package;
mod prepare;
mod source;

pub use catalog::{BundleCatalog, ExportKind};
pub use error::BundleError;
pub use model::{
    AgentRole, BundleIdentity, ModelPolicy, PreparedAgent, PreparedBundle, PreparedBundleIndex,
    PreparedCatalog, PreparedResource, ResourceView, SpawnLifecycle,
};
pub use package::{
    PackageFormat, PackageInspection, PrivatePackageAuthentication, PrivatePackageInspection,
    PrivatePackagePayload, PublicPackageInspection, StagedPackage, cleanup_orphaned_staging,
    detect_package_format, inspect_private_package, inspect_public_package, stage_package,
};
pub use prepare::prepare_package;
pub use source::{BundleSource, SourceFile};
