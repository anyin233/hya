//! Native implementation of the `hya/workflow` built-in bundle exported via
//! Lockstep Native Runtime ABI v2.
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>` with
//! zero dependencies on `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`,
//! or `tonic`.

use std::sync::Arc;

/// Native HTTP/gRPC `BundleApi` handler for `hya/workflow`.
pub mod api;
/// Workflow filesystem & bundle catalog discovery.
pub mod catalog;
/// `hya workflow` command endpoints of the BundleApi handler.
mod cli;
/// Compiler for user-authored Workflow documents.
pub mod compile;
/// Workflow DAG preflight, scheduling, and loop verification.
pub mod dag;
/// Typed workflow execution and control error definitions.
pub mod error;
/// Durable `hya/workflow` extension events.
pub mod events;
/// Workflow identities, commands, and replay projections.
pub mod model;
/// Durable `Event::Extension` projection reducer for `hya/workflow`.
pub mod reducer;
/// Managed [`hya_runtime_abi::NativeService`] owning workflow admission and runs.
pub mod service;
/// Native `workflow` model tool.
pub mod tool;

pub use api::WorkflowBundleApiHandler;
pub use catalog::{
    FIRST_PARTY_BUNDLE_ID, ResolvedWorkflow, WorkflowCatalog, WorkflowCatalogOwner,
    WorkflowCatalogRoots, bundle_workflow_revision, discover_workflow_files_in_root,
    load_workflow_file, workflow_dirs_for_workdir, workflow_project_dir, workflow_user_dir,
};
pub use dag::{
    PreparedWorkflowRun, StageReport, StageStatus, WorkflowRunReport, WorkflowStatus,
    prepare_workflow_run,
};
pub use error::{
    WorkflowControlError, WorkflowError, bounded_error, map_workflow_error_with_detail,
    map_workflow_preflight_error,
};
pub use events::WorkflowEvent;
pub use reducer::{
    WorkflowProjectionReducer, decode_workflow_projection, extension_event_parts, historical_run,
    workflow_event_run,
};
pub use service::{
    WORKFLOW_BUNDLE_ID, WORKFLOW_SERVICE_ID, WorkflowInvocation, WorkflowService,
    workflow_request_hash,
};
pub use tool::WorkflowTool;

/// Register all `hya/workflow` services, tools, API handlers, and reducers into `out`.
pub fn register_bundle(out: &mut hya_runtime_abi::NativeRegistrations) {
    let service = Arc::new(WorkflowService::new());
    out.services
        .push(Arc::clone(&service) as Arc<dyn hya_runtime_abi::NativeService>);
    out.tools
        .push(Arc::new(WorkflowTool::new(Arc::clone(&service))) as Arc<dyn hya_tool::Tool>);
    out.api_handlers
        .push(Arc::new(WorkflowBundleApiHandler::new(service))
            as Arc<dyn hya_runtime_abi::NativeBundleApiHandler>);
    out.projection_reducers
        .push(Arc::new(WorkflowProjectionReducer) as Arc<dyn hya_runtime_abi::ProjectionReducer>);
}

#[cfg(test)]
mod tests;
