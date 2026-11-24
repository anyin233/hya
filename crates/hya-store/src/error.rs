//! Store-layer failures: SQLite, migrations, JSON, admission, and claims.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use hya_proto::{ProjectId, SessionId};

/// All errors returned by [`crate::SessionStore`].
#[derive(Clone, Error, Debug)]
pub enum StoreError {
    /// Underlying sqlx/SQLite I/O or query error.
    #[error("sqlite: {0}")]
    Sqlite(#[source] Arc<sqlx::Error>),
    /// Migration runner failure while applying `migrations/`.
    #[error("migrate: {0}")]
    Migrate(#[source] Arc<sqlx::migrate::MigrateError>),
    /// The session database predates the current schema baseline.
    #[error(
        "this database was created by an older version and is no longer supported; move it away"
    )]
    UnsupportedDatabase,
    /// JSON (de)serialization of event payloads.
    #[error("json: {0}")]
    Json(#[source] Arc<serde_json::Error>),
    /// An event-log row cannot be decoded under the current event contract.
    #[error("event log decode failed for session {session} at sequence {seq}: {source}")]
    EventDecode {
        /// Owning session of the stored row.
        session: SessionId,
        /// Global event-log sequence.
        seq: i64,
        /// JSON error, including an unrecognized event tag when applicable.
        #[source]
        source: Arc<serde_json::Error>,
    },
    /// A stored session key is not a current `hysec_` text key.
    #[error("session key data: {0}")]
    SessionKeyData(String),
    /// Registering or reviving members would exceed the root's live capacity.
    #[error(
        "spawn admission overloaded for root {root}: live {live} + requested {requested} exceeds {limit}"
    )]
    CapacityExceeded {
        /// Team root whose capacity is exhausted.
        root: SessionId,
        /// Active claims before registration.
        live: u64,
        /// New claims requested by this transaction.
        requested: u64,
        /// Configured live-member limit.
        limit: u64,
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
    /// Another runtime/store handle owns the exclusive runtime-owner lock.
    #[error("RUNTIME_OWNER_BUSY: runtime owner lock is already held")]
    RuntimeOwnerBusy,
    /// Startup recovery requires this store to hold the matching owner claim.
    #[error("RUNTIME_OWNER_CLAIM_REQUIRED: matching runtime owner claim is required")]
    RuntimeOwnerClaimRequired,
    /// Runtime-owner lock file I/O failed.
    #[error("runtime owner lock {path}: {source}")]
    RuntimeOwnerLock {
        /// Lock-file path used by this store.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: Arc<std::io::Error>,
    },
    /// Malformed or inconsistent Workflow control mutation.
    #[error("workflow control: {0}")]
    WorkflowData(String),
    /// Corrupt or invalid persisted model effort preference data.
    #[error("model effort preference {field}: {detail}")]
    InvalidPreferenceData {
        /// Field that failed validation.
        field: &'static str,
        /// Validation detail.
        detail: String,
    },
    /// An authorized DM targets an archived direct child and needs engine revival.
    #[error("sender `{sender}` to channel `{channel}` must revive `{target}`")]
    MailRevivalRequired {
        /// Acting handle.
        sender: String,
        /// Authorized channel.
        channel: String,
        /// Archived direct child.
        target: String,
    },
    /// A mail write failed channel authorization inside its transaction.
    #[error("mailbox rejected: sender `{sender}` to channel `{channel}`: {reason}")]
    MailAuthorizationRejected {
        /// Addressed channel.
        channel: String,
        /// Acting canonical handle.
        sender: String,
        /// Failed authorization condition.
        reason: String,
    },
    /// Mail append rejected by roster / permission / validation rules.
    #[error("mailbox rejected: {0}")]
    MailboxRejected(String),
    /// A Project name is empty or only whitespace.
    #[error("PROJECT_NAME_EMPTY: a project needs a non-empty name")]
    ProjectNameEmpty,
    /// A Project was given no roots; a Project has at least one.
    #[error("PROJECT_ROOTS_EMPTY: a project needs at least one root")]
    ProjectRootsEmpty,
    /// A Project root (or a path to resolve) is not an absolute path.
    #[error("PROJECT_ROOT_NOT_ABSOLUTE: {path:?} is not an absolute path")]
    ProjectRootNotAbsolute {
        /// Path as given.
        path: String,
    },
    /// A Project root (or a path to resolve) cannot be normalized lexically.
    #[error("PROJECT_ROOT_INVALID: {path:?}: {reason}")]
    ProjectRootInvalid {
        /// Path as given.
        path: String,
        /// Why the path was rejected.
        reason: &'static str,
    },
    /// No Project with this id exists.
    #[error("PROJECT_NOT_FOUND: project {project} does not exist")]
    ProjectNotFound {
        /// Requested Project.
        project: ProjectId,
    },
    /// A Project cannot be deleted while non-archived root sessions use it.
    #[error("PROJECT_IN_USE: project {project} has {sessions} non-archived session(s)")]
    ProjectInUse {
        /// Project whose delete was refused.
        project: ProjectId,
        /// Non-archived root sessions that reference it.
        sessions: u64,
    },
    /// Corrupt `project` / `project_root` row data.
    #[error("project data: {0}")]
    ProjectData(String),
    /// A bounded storage operation exceeded its timeout while the store was degraded or stalled.
    #[error("storage timeout during {operation} after {timeout:?}")]
    StorageTimeout {
        /// Bounded operation name.
        operation: &'static str,
        /// Configured wait timeout.
        timeout: Duration,
    },
    /// Operation refused because the write queue is degraded or halted.
    #[error("store is degraded: {reason}")]
    StoreDegraded {
        /// First fault or timeout reason recorded on the store.
        reason: String,
    },
}

pub(crate) fn is_sqlite_storage_fault(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => db
            .code()
            .and_then(|c| c.parse::<u32>().ok())
            .is_some_and(|c| matches!(c & 0xff, 3 | 5 | 6 | 8 | 10 | 11 | 13 | 14 | 15 | 26)),
        _ => false,
    }
}

impl StoreError {
    /// Whether this error is a transient storage, locking, or timeout fault
    /// rather than a constraint, decoding, or domain failure.
    #[must_use]
    pub fn is_storage_fault(&self) -> bool {
        match self {
            Self::StorageTimeout { .. } | Self::StoreDegraded { .. } => true,
            Self::Sqlite(err) => is_sqlite_storage_fault(err.as_ref()),
            _ => false,
        }
    }
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
