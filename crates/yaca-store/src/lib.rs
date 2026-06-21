//! `yaca-store` — SQLite event log + replay; projection folded on read via the
//! shared `yaca_proto::Projection` reducer (materialized tables deferred to a
//! later phase — one reducer, no SQL/reducer divergence).
//!
//! NOTE: PRAGMAs (WAL etc.) are set via connect options, NOT a migration — `WAL`
//! cannot run inside the transaction sqlx wraps migrations in.

pub mod error;

use std::str::FromStr;
use std::time::Duration;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use yaca_proto::{Envelope, Event, EventSeq, Projection, SessionId, now_millis};

pub use error::StoreError;

pub struct SessionStore {
    pool: sqlx::SqlitePool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session: SessionId,
    pub started_millis: i64,
    pub events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub session: SessionId,
    pub role: String,
    pub iteration: Option<i64>,
    pub completion_run_id: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub confidence: String,
}

impl SessionStore {
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

    pub async fn append_event(
        &self,
        session: SessionId,
        event: &Event,
    ) -> Result<EventSeq, StoreError> {
        let payload = serde_json::to_string(event)?;
        let key = session.as_uuid().as_bytes().to_vec();
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

    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, StoreError> {
        let key = session.as_uuid().as_bytes().to_vec();
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

    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, StoreError> {
        Ok(Projection::from_events(&self.replay(session).await?))
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, MIN(ts) AS started, COUNT(*) AS n \
             FROM event_log GROUP BY session_id ORDER BY started DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let key: Vec<u8> = r.try_get("session_id")?;
            let started: i64 = r.try_get("started")?;
            let n: i64 = r.try_get("n")?;
            if let Ok(uuid) = uuid::Uuid::from_slice(&key) {
                out.push(SessionInfo {
                    session: SessionId::from_uuid(uuid),
                    started_millis: started,
                    events: n.max(0) as u64,
                });
            }
        }
        Ok(out)
    }

    pub async fn record_usage(&self, entry: &LedgerEntry) -> Result<(), StoreError> {
        let id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let session = entry.session.as_uuid().as_bytes().to_vec();
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

    pub async fn read_usage(&self, session: SessionId) -> Result<Vec<LedgerEntry>, StoreError> {
        let key = session.as_uuid().as_bytes().to_vec();
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
