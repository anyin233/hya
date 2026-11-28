//! Lockstep Native Runtime ABI v2 for `hya` native bundles.
//!
//! This crate defines the zero-pollution host/bundle boundary:
//! - [`HostRuntime`] and its seven sub-traits (`HostSessionStore`,
//!   `HostBundleStorage`, `HostTurnRunner`, `HostModelCaller`, `HostEventBus`,
//!   `HostCatalogReader`, `HostExtensionRegistry`),
//! - Native extension traits (`NativeService`, `NativeHook`,
//!   `NativeBundleApiHandler`, `ProjectionReducer`, `UriSchemaHandler`,
//!   `NativeDriver`),
//! - Lockstep ABI v2 digest verification, [`NativePluginContext`],
//!   [`NativeRegistrations`], and [`export_native_bundle_v2!`].
//!
//! Native bundles link only this crate (and `hya-proto`, `hya-tool`,
//! `hya-context`, `hya-bundle`, `hya-tool-error`, `hya-version`) and never
//! depend on `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or
//! `tonic`.

mod abi;
mod error;
mod extensions;
mod host;

pub use abi::{
    ABI_VERSION_V2, AbiDigestFn, IntoRegistrationResult, NativeAbiV2, NativePluginContext,
    NativeRegistrations, RegisterV2Fn, SYMBOL_ABI_V2, SYMBOL_REGISTER_V2, SYMBOL_SHARE_DEADLINE_V2,
    SYMBOL_WITH_RUNTIME_V2, ShareDeadlineFn, WithRuntimeFn, abi_digest_v2, deadline_cell_v2,
    share_deadline_v2, with_runtime_v2,
};
pub use error::{AbiError, HostError};
pub use extensions::{
    DriverReport, DriverRequest, DriverRunOutcome, ExtensionEventContext, HeadlessRunPhase,
    HookPoint, NativeAgentSpawnInput, NativeApiEndpoint, NativeBundleApiHandler,
    NativeBundleApiReply, NativeBundleApiRequest, NativeChatParamsInput, NativeCommandBeforeInput,
    NativeCompactionAfterInput, NativeCompactionBeforeInput, NativeCompactionDecision,
    NativeCompactionTrigger, NativeDriver, NativeHeadlessRunInput, NativeHook,
    NativeMessageUserBeforeInput, NativeModelFallbackInput, NativePermissionApproveInput,
    NativeService, NativeSessionCloseInput, NativeSessionLifecycleInput, NativeSessionRestoreInput,
    NativeStreamPermit, NativeStreamPermitInput, NativeTextCompleteInput, NativeToolAfterInput,
    NativeToolAfterOutcome, NativeToolBeforeInput, NativeToolBeforeOutcome, NativeToolResult,
    NativeTurnAdmitInput, NativeTurnAdmitOutcome, NativeTurnStopInput, ProjectionReducer,
    ServiceHealth, SessionCloseKind, ToolOutputOverride, ToolVisibilityInput,
    ToolVisibilityOutcome, UriContent, UriResolveCtx, UriSchemaHandleAdapter, UriSchemaHandler,
    adapt_uri_schema_handler, is_declared_hook_point,
};
pub use host::{
    AgentModelRequest, BundleApiInvocation, BundleApiResult, BundleCatalogSummary,
    BundleClaimLease, BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostEventBus, HostExtensionRegistry, HostModelCaller,
    HostRuntime, HostSessionStore, HostTurnRunner, ManagedMemberActivateRequest,
    ManagedMemberActivationOutcome, ManagedMemberActivationStatus, ManagedMemberHandle,
    ManagedMemberSpawnRequest, MemberMailRevivalRequest, ModelCallRequest, ModelCallResponse,
    ModelRouteEntry, ModelStreamChunk, ModelToolCall, ModelWindowInfo, ResolvedModelRoute,
    ResolvedModelRouteCandidate, ScopeCatalogSnapshot, SessionListFilter, SessionSummaryMeta,
    SessionTreeArchive, TurnOutcome, TurnRouteFailureClass, TurnRouteOutcome, TurnSpawnRequest,
    TurnStart, TurnStartRequest, TurnStatus,
};
/// HTTP method and mount scope of a [`NativeApiEndpoint`].
pub use hya_bundle::{ApiMethod, ApiScope};
