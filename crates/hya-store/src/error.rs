use thiserror::Error;

use hya_proto::{OperationId, SessionId};

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bundle: {0}")]
    Bundle(#[from] hya_bundle::BundleError),
    #[error("bundle registry data: {0}")]
    BundleRegistryData(String),
    #[error("BUNDLE_REGISTRY_CORRUPT: bundle {bundle_id} stored prepared catalog is corrupt")]
    BundleRegistryCorrupt { bundle_id: String },
    #[error("BUNDLE_REGISTRY_BUSY: bundle registry writer is busy")]
    BundleRegistryBusy,
    #[error("BUNDLE_NOT_FOUND: bundle {bundle_id} is not installed")]
    BundleNotFound { bundle_id: String },
    #[error("BUNDLE_CONTENT_CONFLICT: bundle {bundle_id} version {version} has different content")]
    BundleContentConflict { bundle_id: String, version: String },
    #[error("PRIVATE_ACTIVATION_UNSUPPORTED")]
    PrivateActivationUnsupported,
    #[error("BUNDLE_IMMUTABLE: bundle {bundle_id} is builtin and immutable")]
    BundleImmutable { bundle_id: String },
    #[error("OPERATION_ID_CONFLICT: immutable request differs for {operation_id}")]
    OperationIdConflict { operation_id: OperationId },
    #[error("admission operation not found: {operation_id}")]
    AdmissionNotFound { operation_id: OperationId },
    #[error("admission transition conflict for {operation_id}: {from} -> {to}")]
    AdmissionTransitionConflict {
        operation_id: OperationId,
        from: &'static str,
        to: &'static str,
    },
    #[error("admission journal: {0}")]
    AdmissionData(String),
    #[error("resident actor is already claimed: {actor_id}")]
    ActorAlreadyClaimed { actor_id: SessionId },
    #[error("resident actor claim is stale: {actor_id}")]
    StaleActorClaim { actor_id: SessionId },
    #[error("resident actor has no recoverable active claim: {actor_id}")]
    ActorClaimUnavailable { actor_id: SessionId },
    #[error("resident actor claim: {0}")]
    ActorClaimData(String),
}
