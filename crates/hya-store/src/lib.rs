//! `hya-store` — SQLite event log + replay; projection folded on read via the
//! shared `hya_proto::Projection` reducer (materialized tables deferred to a
//! later phase — one reducer, no SQL/reducer divergence).
//!
//! NOTE: PRAGMAs (WAL etc.) are set via connect options, NOT a migration — `WAL`
//! cannot run inside the transaction sqlx wraps migrations in.

mod admission;
mod bundle_registry;
/// Typed store errors shared by session and bundle registry APIs.
pub mod error;
mod mailbox;
mod permission;
mod resident_claim;
mod sync;

/// Upper bound on durable `spawn_intent` bytes (1 MiB); mirrored by SQL CHECK.
pub const MAX_ADMISSION_INTENT_BYTES: usize = 1_048_576;

use std::str::FromStr;
use std::time::Duration;

use hya_proto::{Envelope, Event, EventSeq, Projection, SessionId, now_millis};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub use admission::{
    AdmissionActorBinding, AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionClaimOutcome,
    AdmissionCounts, AdmissionFinalizeOutcome, AdmissionIntent, AdmissionLaunch, AdmissionRecord,
    AdmissionReleaseOutcome, AdmissionStartOutcome, AdmissionState, AdmissionTerminal,
};
pub use bundle_registry::{
    BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, BundleRegistryRecord,
    BundleRegistrySnapshot, BundleUninstallOutcome,
};
pub use error::StoreError;
pub use hya_proto::{ActorClaim, OwnerRunId};
pub use mailbox::{RecoveredResidentOutcome, RecoveredResidentWork};
pub use permission::SavedPermission;
pub use resident_claim::RecoveredActorClaim;

/// SQLite-backed session event log, token ledger, admission journal, and related tables.
///
/// Construct with [`SessionStore::connect`] (file) or [`SessionStore::connect_memory`].
/// Projection is folded on read via `hya_proto::Projection` — there is no separate
/// materialized read model.
#[derive(Clone)]
pub struct SessionStore {
    pool: sqlx::SqlitePool,
}

/// One session row from `list_sessions`: id, time bounds, and event count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// Session identity.
    pub session: SessionId,
    /// Earliest event timestamp in the log (unix millis).
    pub started_millis: i64,
    /// Latest event timestamp in the log (unix millis).
    pub updated_millis: i64,
    /// Number of rows in `event_log` for this session.
    pub events: u64,
}

/// One token-usage row written by the engine after a completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Session the usage belongs to.
    pub session: SessionId,
    /// Role label (e.g. assistant / system accounting bucket).
    pub role: String,
    /// Optional multi-step iteration index.
    pub iteration: Option<i64>,
    /// Optional id correlating multiple ledger rows for one completion run.
    pub completion_run_id: Option<String>,
    /// Prompt-side token count.
    pub prompt_tokens: i64,
    /// Completion-side token count.
    pub completion_tokens: i64,
    /// Confidence or estimation quality label stored with the row.
    pub confidence: String,
}

impl SessionStore {
    /// Open or create a file-backed store at `sqlite://{path}`, run migrations, enable WAL.
    pub async fn connect(path: &str) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    /// Open an in-memory store (single connection) and run migrations.
    pub async fn connect_memory() -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    async fn migrate(pool: &sqlx::SqlitePool) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations").run(pool).await?;
        Ok(())
    }

    /// Append one domain event to the session log; returns the assigned sequence.
    pub async fn append_event(
        &self,
        session: SessionId,
        event: &Event,
    ) -> Result<EventSeq, StoreError> {
        let payload = serde_json::to_string(event)?;
        let key = session.storage_key();
        let row = sqlx::query(
            "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq",
        )
        .bind(key)
        .bind(payload)
        .bind(now_millis())
        .fetch_one(&self.pool)
        .await?;
        let seq: i64 = row.try_get("seq")?;
        Ok(EventSeq(seq.max(0) as u64))
    }

    /// Load all envelopes for a session in sequence order (payload JSON decoded to `Event`).
    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, StoreError> {
        let key = session.storage_key();
        let rows =
            sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
                .bind(key)
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let seq: i64 = r.try_get("seq")?;
            let ts: i64 = r.try_get("ts")?;
            let payload: String = r.try_get("payload")?;
            let event: Event = serde_json::from_str(&payload)?;
            out.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis: ts,
                event,
            });
        }
        Ok(out)
    }

    /// Delete ledger and event rows for a session; returns whether any event rows were removed.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, StoreError> {
        let key = session.storage_key();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM token_ledger WHERE session_id = ?")
            .bind(key.clone())
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM event_log WHERE session_id = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    /// Fold the session event log into a [`Projection`] via the shared reducer.
    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, StoreError> {
        Ok(Projection::from_events(&self.replay(session).await?))
    }

    /// Sessions present in the event log, newest-updated first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, MIN(ts) AS started, MAX(ts) AS updated, COUNT(*) AS n \
             FROM event_log GROUP BY session_id ORDER BY updated DESC, session_id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let key: Vec<u8> = r.try_get("session_id")?;
            let started: i64 = r.try_get("started")?;
            let updated: i64 = r.try_get("updated")?;
            let n: i64 = r.try_get("n")?;
            if let Some(session) = decode_session_key(&key) {
                out.push(SessionInfo {
                    session,
                    started_millis: started,
                    updated_millis: updated,
                    events: n.max(0) as u64,
                });
            }
        }
        Ok(out)
    }

    /// Insert one token-ledger row (new UUID primary key, current timestamp).
    pub async fn record_usage(&self, entry: &LedgerEntry) -> Result<(), StoreError> {
        let id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let session = entry.session.storage_key();
        sqlx::query(
            "INSERT INTO token_ledger \
             (id, session_id, iteration, completion_run_id, role, prompt_tokens, completion_tokens, confidence, ts) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(session)
        .bind(entry.iteration)
        .bind(entry.completion_run_id.clone())
        .bind(entry.role.clone())
        .bind(entry.prompt_tokens)
        .bind(entry.completion_tokens)
        .bind(entry.confidence.clone())
        .bind(now_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All token-ledger rows for a session in timestamp order.
    pub async fn read_usage(&self, session: SessionId) -> Result<Vec<LedgerEntry>, StoreError> {
        let key = session.storage_key();
        let rows = sqlx::query(
            "SELECT iteration, completion_run_id, role, prompt_tokens, completion_tokens, confidence \
             FROM token_ledger WHERE session_id = ? ORDER BY ts",
        )
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(LedgerEntry {
                session,
                iteration: r.try_get("iteration")?,
                completion_run_id: r.try_get("completion_run_id")?,
                role: r.try_get("role")?,
                prompt_tokens: r.try_get("prompt_tokens")?,
                completion_tokens: r.try_get("completion_tokens")?,
                confidence: r.try_get("confidence")?,
            });
        }
        Ok(out)
    }
}

pub(crate) fn decode_session_key(key: &[u8]) -> Option<SessionId> {
    if let Ok(raw) = std::str::from_utf8(key) {
        return raw.parse().ok();
    }
    uuid::Uuid::from_slice(key).ok().map(SessionId::from_uuid)
}
