//! Native installable-bundle source preparation and prepared-catalog decoding.
//!
//! Source parsing and filesystem access stop at this crate. Runtime callers
//! consume only immutable prepared data returned by [`prepare_package`].

mod api;
mod catalog;
mod cli;
mod error;
mod first_party;
mod model;
mod package;
mod prepare;
mod source;

pub use api::{
    ApiMethod, ApiPathTemplate, ApiScope, ApiTemplateSegment, MAX_API_PARAM_NAME_BYTES,
    MAX_API_PATH_BYTES, MAX_API_PATH_SEGMENTS, MAX_API_REQUEST_PATH_BYTES, split_request_path,
};
pub use catalog::{BundleCatalog, ExportKind};
pub use cli::{CliArg, CliCommand, CliCommandKind, CliValue};
pub use error::BundleError;
pub use first_party::{
    BuiltinBundleEntry, BuiltinBundleProfile, BuiltinBundleSelection, BuiltinBundleStatus,
    BuiltinResolutionReport, FIRST_PARTY_BUNDLES, FIRST_PARTY_SOURCE_ROOT_ENV, FirstPartySource,
    INTERNAL_BUNDLES, first_party_bundle, first_party_bundle_optional, first_party_package_name,
    first_party_source, first_party_source_directory, first_party_source_root, load_first_party,
    resolve_first_party_bundles, resolve_first_party_bundles_with,
};
pub use model::{
    AgentRole, Bundle, BundleIdentity, ChannelCapability, ChannelParticipantRole, ChannelRetention,
    ChannelScope, ChannelTemplateKind, HandoffPolicy, ModelPolicy, PreparedAgent, PreparedApi,
    PreparedBundleApis, PreparedBundleIndex, PreparedBundleKind, PreparedBundlePermissionModes,
    PreparedBundleProcess, PreparedBundleSchemas, PreparedCatalog, PreparedChannelParticipant,
    PreparedChannelTemplate, PreparedCheck, PreparedJsRuntimeMode, PreparedNativeCapability,
    PreparedPermissionMode, PreparedProcessExtension, PreparedProcessKind, PreparedResource,
    PreparedRuntime, PreparedRuntimeKind, PreparedSchema, PreparedTuiExtension, PreparedWorkflow,
    ResourceView,
};
pub use package::{
    PackageFormat, PublicPackageInspection, StagedPackage, cleanup_orphaned_staging,
    detect_package_format, inspect_public_package, stage_package, write_public_package,
};
pub use prepare::{prepare_package, read_bundle_identity};
pub use source::{BundleSource, SourceFile, SourceTuiExtension};
