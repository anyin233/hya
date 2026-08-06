//! Store-layer failures: SQLite, migrations, JSON, admission, bundles, and claims.

use std::sync::Arc;

use thiserror::Error;

use hya_proto::{OperationId, SessionId};

/// All errors returned by [`crate::SessionStore`] and [`crate::BundleRegistry`].
#[derive(Clone, Error, Debug)]
pub enum StoreError {
    /// Underlying sqlx/SQLite I/O or query error.
    #[error("sqlite: {0}")]
    Sqlite(#[source] Arc<sqlx::Error>),
    /// Migration runner failure while applying `migrations/`.
    #[error("migrate: {0}")]
    Migrate(#[source] Arc<sqlx::migrate::MigrateError>),
    /// JSON (de)serialization of event payloads or sync envelopes.
    #[error("json: {0}")]
    Json(#[source] Arc<serde_json::Error>),
    /// Prepared-bundle / catalog validation failure from `hya-bundle`.
    #[error("bundle: {0}")]
    Bundle(#[from] hya_bundle::BundleError),
    /// Malformed or inconsistent installed-bundle registry row data.
    #[error("bundle registry data: {0}")]
    BundleRegistryData(String),
    /// Stored prepared catalog bytes for an installed bundle fail re-decode.
    #[error("BUNDLE_REGISTRY_CORRUPT: bundle {bundle_id} stored prepared catalog is corrupt")]
    BundleRegistryCorrupt {
        /// Bundle identity that failed validation.
        bundle_id: String,
    },
    /// Another writer holds the registry lock (`BEGIN IMMEDIATE` busy/locked).
    #[error("BUNDLE_REGISTRY_BUSY: bundle registry writer is busy")]
    BundleRegistryBusy,
    /// Uninstall or lookup named a bundle that is not installed.
    #[error("BUNDLE_NOT_FOUND: bundle {bundle_id} is not installed")]
    BundleNotFound {
        /// Requested bundle id.
        bundle_id: String,
    },
    /// Same version string already installed with a different content digest.
    #[error("BUNDLE_CONTENT_CONFLICT: bundle {bundle_id} version {version} has different content")]
    BundleContentConflict {
        /// Conflicting bundle id.
        bundle_id: String,
        /// Version that already exists with different bytes.
        version: String,
    },
    /// Private package inspection cannot be activated through the registry.
    #[error("PRIVATE_ACTIVATION_UNSUPPORTED")]
    PrivateActivationUnsupported,
    /// Attempt to install/uninstall a builtin immutable bundle id.
    #[error("BUNDLE_IMMUTABLE: bundle {bundle_id} is builtin and immutable")]
    BundleImmutable {
        /// Builtin bundle id that cannot be mutated.
        bundle_id: String,
    },
    /// Reclaim of an admission `operation_id` with a different request fingerprint.
    #[error("OPERATION_ID_CONFLICT: immutable request differs for {operation_id}")]
    OperationIdConflict {
        /// Operation whose durable claim does not match the new request.
        operation_id: OperationId,
    },
    /// No admission journal row for the given operation.
    #[error("admission operation not found: {operation_id}")]
    AdmissionNotFound {
        /// Missing operation id.
        operation_id: OperationId,
    },
    /// Illegal lifecycle transition (wrong `from` state for the requested `to`).
    #[error("admission transition conflict for {operation_id}: {from} -> {to}")]
    AdmissionTransitionConflict {
        /// Operation that failed to transition.
        operation_id: OperationId,
        /// Current wire state string.
        from: &'static str,
        /// Requested wire state string.
        to: &'static str,
    },
    /// Generic admission journal invariant or input validation failure.
    #[error("admission journal: {0}")]
    AdmissionData(String),
    /// Active or non-active admission caps would be exceeded by the request.
    #[error(
        "admission capacity exceeded: active={active}, non_active={non_active}, requested={requested}"
    )]
    AdmissionCapacityExceeded {
        /// Rows currently in `accepted` + `started`.
        active: u32,
        /// Rows currently in `queued` + `waiting`.
        non_active: u32,
        /// Units this claim asked to reserve.
        requested: u32,
    },
    /// Ordinary claim lost because another process already holds the actor.
    #[error("resident actor is already claimed: {actor_id}")]
    ActorAlreadyClaimed {
        /// Actor session id under contention.
        actor_id: SessionId,
    },
    /// Claim fence failed: epoch or owner no longer matches the active row.
    #[error("resident actor claim is stale: {actor_id}")]
    StaleActorClaim {
        /// Actor whose claim was fenced out.
        actor_id: SessionId,
    },
    /// Takeover / recover found no active claim to recover for this actor.
    #[error("resident actor has no recoverable active claim: {actor_id}")]
    ActorClaimUnavailable {
        /// Actor with no recoverable claim.
        actor_id: SessionId,
    },
    /// Corrupt or unparseable resident claim row data.
    #[error("resident actor claim: {0}")]
    ActorClaimData(String),
    /// Mail append rejected by roster / permission / validation rules.
    #[error("mailbox rejected: {0}")]
    MailboxRejected(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sqlite(Arc::new(error))
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate(Arc::new(error))
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(Arc::new(error))
    }
}
