//! Lossless SQLite `sessions.db` v1-to-v2 migration.
//!
//! Translates v1 domain `Event` rows in `event_log` into v2
//! `Event::Extension { session, bundle, kind, payload }`, migrates
//! `resident_actor_claim` rows into `bundle_claim` (plus migration metadata in
//! `bundle_state`), writes `schema_meta.format_version = 2` on a fresh v2
//! `hya-store` baseline, verifies `PRAGMA foreign_key_check` and
//! `PRAGMA integrity_check`, and atomically swaps the file into place.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_proto::{Event, SessionId, now_millis};
use hya_store::SessionStore;
use hya_tool::extension::{BUNDLE_CHANNELS, BUNDLE_MULTI_AGENT, BUNDLE_WORKFLOW};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Row as _};
use uuid::Uuid;

use crate::backup::{
    atomic_swap_file, remove_sqlite_sidecars, sqlite_sidecar_paths, unique_temp_path,
};
use crate::error::{MigrationError, Result};

/// Status of `sessions.db` returned by [`check_db`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbCheckStatus {
    /// Database file does not exist at the configured path.
    NotFound,
    /// Database is already a valid v2 `hya-store` database with zero legacy events.
    AlreadyV2,
    /// Database requires v1-to-v2 migration.
    NeedsMigration,
}

/// Read-only inspection summary for `sessions.db`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbCheckReport {
    /// Path inspected.
    pub path: PathBuf,
    /// Classification of the database.
    pub status: DbCheckStatus,
    /// Detected `schema_meta.format_version` value if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_version: Option<String>,
    /// Whether the legacy v1 `resident_actor_claim` table is present.
    pub has_resident_actor_claim: bool,
    /// Number of rows in `resident_actor_claim`.
    pub resident_claim_rows: u64,
    /// Total rows in `event_log`.
    pub total_events: u64,
    /// Number of v1 domain event rows that must be rewritten to `Event::Extension`.
    pub legacy_domain_events: u64,
    /// Breakdown of legacy domain events by v1 event type tag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_event_counts: BTreeMap<String, u64>,
    /// Number of sessions in the `session` table.
    pub session_count: u64,
    /// Human-readable reasons explaining why migration is or is not needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// Result summary for migrating `sessions.db` from v1 to v2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbMigrationReport {
    /// Target database path.
    pub path: PathBuf,
    /// Whether the database was actually modified (`false` for dry-run or already-v2).
    pub migrated: bool,
    /// Sessions copied.
    pub sessions_copied: u64,
    /// Total `event_log` rows copied.
    pub events_copied: u64,
    /// v1 domain events rewritten into `Event::Extension`.
    pub events_rewritten: u64,
    /// `resident_actor_claim` rows migrated into `bundle_claim`.
    pub claims_migrated: u64,
    /// Whether `PRAGMA integrity_check` passed on the migrated database.
    pub integrity_ok: bool,
    /// Whether v2 `hya-store` opened and verified the migrated database.
    pub store_verified: bool,
    /// Additional notes or diagnostics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Map a v1 domain `Event` `"type"` tag to its owning v2 bundle id.
///
/// Returns `None` for core kernel events and unknown/retired events that do not
/// translate to `Event::Extension`.
#[must_use]
pub fn legacy_event_bundle(event_type: &str) -> Option<&'static str> {
    match event_type {
        "context_evicted" | "context_status" => Some("hya/compaction"),
        "workflow_selected"
        | "workflow_run_started"
        | "workflow_stage_started"
        | "workflow_stage_member_linked"
        | "workflow_stage_route_outcome"
        | "workflow_stage_finished"
        | "workflow_run_finished" => Some(BUNDLE_WORKFLOW),
        "mail_sent" | "channel_joined" | "channel_created" | "mail_consumed" => {
            Some(BUNDLE_CHANNELS)
        }
        "session_status"
        | "member_spawned"
        | "member_status_changed"
        | "member_finished"
        | "agent_registered"
        | "agent_activity_changed"
        | "resident_work_started"
        | "resident_work_finished"
        | "agent_heartbeat"
        | "subagent_reported"
        | "handoff_committed"
        | "agent_archived"
        | "agent_restarted" => Some(BUNDLE_MULTI_AGENT),
        _ => None,
    }
}

/// Rewrite one raw `event_log.payload` JSON string if it is a v1 domain event.
///
/// Returns `Ok(Some((rewritten_json, legacy_tag)))` when rewritten into v2
/// `Event::Extension`, or `Ok(None)` when the event should be preserved as-is.
pub fn rewrite_event_payload(
    raw_payload: &str,
    row_session_id: &[u8],
) -> Result<Option<(String, String)>> {
    let mut parsed: Value =
        serde_json::from_str(raw_payload).map_err(|err| MigrationError::Json {
            context: "decode event_log payload JSON".to_owned(),
            source: err,
        })?;
    let Some(obj) = parsed.as_object_mut() else {
        return Ok(None);
    };
    let Some(tag) = obj.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(None);
    };
    let Some(bundle) = legacy_event_bundle(&tag) else {
        return Ok(None);
    };

    obj.remove("type");
    let session_value = match obj.remove("session") {
        Some(val) => val,
        None => {
            let sid = decode_session_id_blob(row_session_id)?;
            Value::String(sid)
        }
    };

    let mut extension = Map::with_capacity(5);
    extension.insert("type".to_owned(), Value::String("extension".to_owned()));
    extension.insert("session".to_owned(), session_value);
    extension.insert("bundle".to_owned(), Value::String(bundle.to_owned()));
    extension.insert("kind".to_owned(), Value::String(tag.clone()));
    extension.insert("payload".to_owned(), Value::Object(std::mem::take(obj)));

    let encoded =
        serde_json::to_string(&Value::Object(extension)).map_err(|err| MigrationError::Json {
            context: format!("encode v2 extension event `{tag}`"),
            source: err,
        })?;

    // Sanity-check that v2 `hya_proto::Event` deserializes the rewritten payload cleanly.
    let decoded: Event = serde_json::from_str(&encoded).map_err(|err| MigrationError::Json {
        context: format!("validate rewritten v2 Event::Extension `{tag}`"),
        source: err,
    })?;
    if !matches!(decoded, Event::Extension { .. }) {
        return Err(MigrationError::InvalidArtifact {
            path: PathBuf::from("event_log"),
            detail: format!("rewritten event `{tag}` did not decode as Event::Extension"),
        });
    }

    Ok(Some((encoded, tag)))
}

fn decode_session_id_blob(blob: &[u8]) -> Result<String> {
    if let Ok(text) = std::str::from_utf8(blob) {
        if text.parse::<SessionId>().is_ok() {
            return Ok(text.to_owned());
        }
        if !text.trim().is_empty() && text.is_ascii() {
            return Ok(text.to_owned());
        }
    }
    if let Ok(uuid) = Uuid::from_slice(blob) {
        return Ok(uuid.to_string());
    }
    Err(MigrationError::InvalidArtifact {
        path: PathBuf::from("sessions.db"),
        detail: format!("unable to decode session id blob ({} bytes)", blob.len()),
    })
}

fn decode_owner_run_id_blob(blob: &[u8]) -> Result<String> {
    if let Ok(uuid) = Uuid::from_slice(blob) {
        return Ok(uuid.to_string());
    }
    if let Ok(text) = std::str::from_utf8(blob)
        && !text.trim().is_empty()
    {
        return Ok(text.to_owned());
    }
    Err(MigrationError::InvalidArtifact {
        path: PathBuf::from("sessions.db"),
        detail: format!("unable to decode owner_run_id blob ({} bytes)", blob.len()),
    })
}

/// Copy `db_path` (plus `-wal` and `-shm` if present) into a private staging
/// directory so reading never modifies the original file or WAL sidecars.
struct StagingDbCopy {
    dir: PathBuf,
    db_path: PathBuf,
}

impl StagingDbCopy {
    fn create(source: &Path) -> Result<Self> {
        let temp_name = unique_temp_path(source, "stage")?;
        let leaf = temp_name
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("hya-mig-stage");
        let dir = std::env::temp_dir().join(format!("hya-migration-{leaf}"));
        fs::create_dir_all(&dir)
            .map_err(|err| MigrationError::io(&dir, "create staging db dir", err))?;
        let dst = dir.join("source.db");
        fs::copy(source, &dst)
            .map_err(|err| MigrationError::io(source, "copy source db to staging", err))?;
        let [src_wal, src_shm] = sqlite_sidecar_paths(source);
        let [dst_wal, dst_shm] = sqlite_sidecar_paths(&dst);
        if src_wal.exists() {
            fs::copy(&src_wal, &dst_wal).map_err(|err| {
                MigrationError::io(&src_wal, "copy source db wal to staging", err)
            })?;
        }
        if src_shm.exists() {
            fs::copy(&src_shm, &dst_shm).map_err(|err| {
                MigrationError::io(&src_shm, "copy source db shm to staging", err)
            })?;
        }
        Ok(Self { dir, db_path: dst })
    }
}

impl Drop for StagingDbCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Inspect `sessions.db` without modifying any file on disk.
pub async fn check_db(path: &Path) -> Result<DbCheckReport> {
    if !path.is_file() {
        return Ok(DbCheckReport {
            path: path.to_path_buf(),
            status: DbCheckStatus::NotFound,
            format_version: None,
            has_resident_actor_claim: false,
            resident_claim_rows: 0,
            total_events: 0,
            legacy_domain_events: 0,
            legacy_event_counts: BTreeMap::new(),
            session_count: 0,
            reasons: Vec::new(),
        });
    }

    let staging = StagingDbCopy::create(path)?;
    let opts = SqliteConnectOptions::new()
        .filename(&staging.db_path)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5));
    let mut conn =
        SqliteConnection::connect_with(&opts)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: format!("open staging copy of {}", path.display()),
                source: err,
            })?;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut conn)
    .await
    .map_err(|err| MigrationError::Sqlx {
        context: "list sqlite tables".to_owned(),
        source: err,
    })?;

    let has_table = |name: &str| tables.iter().any(|t| t == name);
    let has_resident_actor_claim = has_table("resident_actor_claim");
    let has_schema_meta = has_table("schema_meta");
    let has_event_log = has_table("event_log");
    let has_session = has_table("session");

    let format_version: Option<String> = if has_schema_meta {
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'format_version'")
            .fetch_optional(&mut conn)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read schema_meta.format_version".to_owned(),
                source: err,
            })?
    } else {
        None
    };

    let resident_claim_rows: u64 = if has_resident_actor_claim {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resident_actor_claim")
            .fetch_one(&mut conn)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "count resident_actor_claim".to_owned(),
                source: err,
            })?;
        u64::try_from(count).unwrap_or(0)
    } else {
        0
    };

    let session_count: u64 = if has_session {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session")
            .fetch_one(&mut conn)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "count session".to_owned(),
                source: err,
            })?;
        u64::try_from(count).unwrap_or(0)
    } else {
        0
    };

    let mut total_events = 0u64;
    let mut legacy_domain_events = 0u64;
    let mut legacy_event_counts: BTreeMap<String, u64> = BTreeMap::new();
    if has_event_log {
        let payloads: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM event_log ORDER BY seq ASC")
                .fetch_all(&mut conn)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "scan event_log payloads".to_owned(),
                    source: err,
                })?;
        total_events = u64::try_from(payloads.len()).unwrap_or(u64::MAX);
        for payload in payloads {
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&payload)
                && let Some(tag) = obj.get("type").and_then(Value::as_str)
                && legacy_event_bundle(tag).is_some()
            {
                legacy_domain_events += 1;
                *legacy_event_counts.entry(tag.to_owned()).or_insert(0) += 1;
            }
        }
    }

    let _ = conn.close().await;

    let mut reasons = Vec::new();
    if has_resident_actor_claim {
        reasons.push(format!(
            "legacy table `resident_actor_claim` is present ({resident_claim_rows} rows)"
        ));
    }
    if format_version.as_deref() != Some("2") {
        let found = format_version.as_deref().unwrap_or("missing");
        reasons.push(format!(
            "schema_meta.format_version is `{found}` (v2 requires `2`)"
        ));
    }
    if legacy_domain_events > 0 {
        reasons.push(format!(
            "{legacy_domain_events} legacy v1 domain event(s) in `event_log` require translation to `Event::Extension`"
        ));
    }

    // Even when schema_meta says 2 and no legacy table/events exist, verify v2 hya-store
    // accepts the `_sqlx_migrations` checksum cleanly.
    if reasons.is_empty() && !tables.is_empty() {
        let stage_str = staging.db_path.to_string_lossy().into_owned();
        if let Err(err) = SessionStore::connect_read_only(&stage_str).await {
            reasons.push(format!("v2 hya-store rejected existing schema: {err}"));
        }
    }

    let status = if reasons.is_empty() {
        DbCheckStatus::AlreadyV2
    } else {
        DbCheckStatus::NeedsMigration
    };

    Ok(DbCheckReport {
        path: path.to_path_buf(),
        status,
        format_version,
        has_resident_actor_claim,
        resident_claim_rows,
        total_events,
        legacy_domain_events,
        legacy_event_counts,
        session_count,
        reasons,
    })
}

/// Migrate `sessions.db` from v1 to v2 using an atomic temp-file swap.
///
/// When `dry_run` is `true`, simulates all event/claim conversions against a
/// staging copy without writing to `path`.
pub async fn migrate_db(path: &Path, dry_run: bool) -> Result<DbMigrationReport> {
    let check = check_db(path).await?;
    match check.status {
        DbCheckStatus::NotFound => {
            return Ok(DbMigrationReport {
                path: path.to_path_buf(),
                migrated: false,
                sessions_copied: 0,
                events_copied: 0,
                events_rewritten: 0,
                claims_migrated: 0,
                integrity_ok: true,
                store_verified: false,
                notes: vec!["sessions.db not found; skipped".to_owned()],
            });
        }
        DbCheckStatus::AlreadyV2 => {
            return Ok(DbMigrationReport {
                path: path.to_path_buf(),
                migrated: false,
                sessions_copied: check.session_count,
                events_copied: check.total_events,
                events_rewritten: 0,
                claims_migrated: 0,
                integrity_ok: true,
                store_verified: true,
                notes: vec!["sessions.db is already v2; no changes needed".to_owned()],
            });
        }
        DbCheckStatus::NeedsMigration => {}
    }

    if dry_run {
        return Ok(DbMigrationReport {
            path: path.to_path_buf(),
            migrated: false,
            sessions_copied: check.session_count,
            events_copied: check.total_events,
            events_rewritten: check.legacy_domain_events,
            claims_migrated: check.resident_claim_rows,
            integrity_ok: true,
            store_verified: false,
            notes: vec![format!(
                "dry-run: would rewrite {} legacy events, migrate {} resident claims, and set format_version = 2",
                check.legacy_domain_events, check.resident_claim_rows
            )],
        });
    }

    let staging = StagingDbCopy::create(path)?;
    let temp_target = unique_temp_path(path, "v2db")?;
    let migrate_result = run_db_migration(&staging.db_path, &temp_target, path).await;
    match migrate_result {
        Ok(report) => {
            atomic_swap_file(&temp_target, path)?;
            remove_sqlite_sidecars(path)?;
            Ok(report)
        }
        Err(err) => {
            let _ = fs::remove_file(&temp_target);
            let _ = remove_sqlite_sidecars(&temp_target);
            Err(err)
        }
    }
}

async fn run_db_migration(
    source_db: &Path,
    temp_target: &Path,
    display_path: &Path,
) -> Result<DbMigrationReport> {
    // 1. Initialize a brand-new v2 database using `hya-store`'s own baseline
    //    migrations so `_sqlx_migrations` carries the exact v2 checksum.
    let temp_str = temp_target.to_string_lossy().into_owned();
    {
        let store =
            SessionStore::connect(&temp_str)
                .await
                .map_err(|err| MigrationError::Store {
                    context: format!("initialize v2 temp database {}", temp_target.display()),
                    source: err,
                })?;
        let _ = store.drain_for_exit(Duration::from_secs(5)).await;
    }

    // 2. Open source (staging copy) and destination (fresh v2 temp DB).
    let src_opts = SqliteConnectOptions::new()
        .filename(source_db)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5));
    let mut src = SqliteConnection::connect_with(&src_opts)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: format!("open v1 staging db {}", source_db.display()),
            source: err,
        })?;

    let dst_opts = SqliteConnectOptions::new()
        .filename(temp_target)
        .create_if_missing(false)
        .foreign_keys(false)
        .busy_timeout(Duration::from_secs(10));
    let mut dst = SqliteConnection::connect_with(&dst_opts)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: format!("open v2 temp db {}", temp_target.display()),
            source: err,
        })?;

    let src_tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut src)
    .await
    .map_err(|err| MigrationError::Sqlx {
        context: "list source sqlite tables".to_owned(),
        source: err,
    })?;
    let src_has = |name: &str| src_tables.iter().any(|t| t == name);

    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut dst)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "disable foreign_keys for bulk migration".to_owned(),
            source: err,
        })?;

    let mut tx = dst.begin().await.map_err(|err| MigrationError::Sqlx {
        context: "begin v2 migration transaction".to_owned(),
        source: err,
    })?;

    // Copy `project` and `project_root`.
    if src_has("project") {
        let rows = sqlx::query("SELECT id, name, created_at, updated_at, archived FROM project")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read project table".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO project (id, name, created_at, updated_at, archived) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("id").map_err(sqlx_col("project.id"))?)
            .bind(row.try_get::<String, _>("name").map_err(sqlx_col("project.name"))?)
            .bind(row.try_get::<i64, _>("created_at").map_err(sqlx_col("project.created_at"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("project.updated_at"))?)
            .bind(row.try_get::<i64, _>("archived").map_err(sqlx_col("project.archived"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert project row".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("project_root") {
        let rows = sqlx::query("SELECT project_id, path, ord FROM project_root")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read project_root table".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query("INSERT INTO project_root (project_id, path, ord) VALUES (?, ?, ?)")
                .bind(
                    row.try_get::<String, _>("project_id")
                        .map_err(sqlx_col("project_root.project_id"))?,
                )
                .bind(
                    row.try_get::<String, _>("path")
                        .map_err(sqlx_col("project_root.path"))?,
                )
                .bind(
                    row.try_get::<i64, _>("ord")
                        .map_err(sqlx_col("project_root.ord"))?,
                )
                .execute(&mut *tx)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "insert project_root row".to_owned(),
                    source: err,
                })?;
        }
    }

    // Copy `session`.
    let mut sessions_copied = 0u64;
    if src_has("session") {
        let rows = sqlx::query(
            "SELECT id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, project_id, kind FROM session",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read session table".to_owned(),
            source: err,
        })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO session (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, project_id, kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(row.try_get::<Vec<u8>, _>("id").map_err(sqlx_col("session.id"))?)
            .bind(row.try_get::<Option<Vec<u8>>, _>("parent_id").map_err(sqlx_col("session.parent_id"))?)
            .bind(row.try_get::<String, _>("agent").map_err(sqlx_col("session.agent"))?)
            .bind(row.try_get::<String, _>("model").map_err(sqlx_col("session.model"))?)
            .bind(row.try_get::<String, _>("workdir").map_err(sqlx_col("session.workdir"))?)
            .bind(row.try_get::<Option<String>, _>("title").map_err(sqlx_col("session.title"))?)
            .bind(row.try_get::<String, _>("permission").map_err(sqlx_col("session.permission"))?)
            .bind(row.try_get::<i64, _>("created_at").map_err(sqlx_col("session.created_at"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("session.updated_at"))?)
            .bind(row.try_get::<Option<String>, _>("project_id").map_err(sqlx_col("session.project_id"))?)
            .bind(row.try_get::<String, _>("kind").map_err(sqlx_col("session.kind"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert session row".to_owned(),
                source: err,
            })?;
            sessions_copied += 1;
        }
    }

    // Copy & translate `event_log` preserving exact `seq` and `ts`.
    let mut events_copied = 0u64;
    let mut events_rewritten = 0u64;
    if src_has("event_log") {
        let rows =
            sqlx::query("SELECT seq, session_id, payload, ts FROM event_log ORDER BY seq ASC")
                .fetch_all(&mut src)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "read event_log table".to_owned(),
                    source: err,
                })?;
        for row in rows {
            let seq: i64 = row.try_get("seq").map_err(sqlx_col("event_log.seq"))?;
            let session_id: Vec<u8> = row
                .try_get("session_id")
                .map_err(sqlx_col("event_log.session_id"))?;
            let payload: String = row
                .try_get("payload")
                .map_err(sqlx_col("event_log.payload"))?;
            let ts: i64 = row.try_get("ts").map_err(sqlx_col("event_log.ts"))?;

            let final_payload = match rewrite_event_payload(&payload, &session_id)? {
                Some((rewritten, _tag)) => {
                    events_rewritten += 1;
                    rewritten
                }
                None => payload,
            };

            sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
                .bind(seq)
                .bind(session_id)
                .bind(final_payload)
                .bind(ts)
                .execute(&mut *tx)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: format!("insert event_log seq={seq}"),
                    source: err,
                })?;
            events_copied += 1;
        }
    }

    // Copy `saved_permission`.
    if src_has("saved_permission") {
        let rows = sqlx::query(
            "SELECT id, project_id, action, resource, time_created FROM saved_permission",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read saved_permission table".to_owned(),
            source: err,
        })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO saved_permission (id, project_id, action, resource, time_created) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("id").map_err(sqlx_col("saved_permission.id"))?)
            .bind(row.try_get::<String, _>("project_id").map_err(sqlx_col("saved_permission.project_id"))?)
            .bind(row.try_get::<String, _>("action").map_err(sqlx_col("saved_permission.action"))?)
            .bind(row.try_get::<String, _>("resource").map_err(sqlx_col("saved_permission.resource"))?)
            .bind(row.try_get::<Option<i64>, _>("time_created").map_err(sqlx_col("saved_permission.time_created"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert saved_permission row".to_owned(),
                source: err,
            })?;
        }
    }

    // Copy existing `bundle_state` and `bundle_claim` if source already had any.
    if src_has("bundle_state") {
        let rows = sqlx::query("SELECT bundle, key, value, updated_at FROM bundle_state")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read bundle_state table".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query(
                "INSERT OR REPLACE INTO bundle_state (bundle, key, value, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("bundle").map_err(sqlx_col("bundle_state.bundle"))?)
            .bind(row.try_get::<String, _>("key").map_err(sqlx_col("bundle_state.key"))?)
            .bind(row.try_get::<Vec<u8>, _>("value").map_err(sqlx_col("bundle_state.value"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("bundle_state.updated_at"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert bundle_state row".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("bundle_claim") {
        let rows = sqlx::query(
            "SELECT bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at, state FROM bundle_claim",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read bundle_claim table".to_owned(),
            source: err,
        })?;
        for row in rows {
            sqlx::query(
                "INSERT OR REPLACE INTO bundle_claim (bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at, state)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("bundle").map_err(sqlx_col("bundle_claim.bundle"))?)
            .bind(row.try_get::<String, _>("claim_key").map_err(sqlx_col("bundle_claim.claim_key"))?)
            .bind(row.try_get::<String, _>("owner").map_err(sqlx_col("bundle_claim.owner"))?)
            .bind(row.try_get::<i64, _>("epoch").map_err(sqlx_col("bundle_claim.epoch"))?)
            .bind(row.try_get::<Option<i64>, _>("expires_at").map_err(sqlx_col("bundle_claim.expires_at"))?)
            .bind(row.try_get::<String, _>("metadata").map_err(sqlx_col("bundle_claim.metadata"))?)
            .bind(row.try_get::<i64, _>("acquired_at").map_err(sqlx_col("bundle_claim.acquired_at"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("bundle_claim.updated_at"))?)
            .bind(row.try_get::<String, _>("state").map_err(sqlx_col("bundle_claim.state"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert bundle_claim row".to_owned(),
                source: err,
            })?;
        }
    }

    // Migrate `resident_actor_claim` -> `bundle_claim` + provenance entry in `bundle_state`.
    let mut claims_migrated = 0u64;
    let now = now_millis();
    if src_has("resident_actor_claim") {
        let rows = sqlx::query(
            "SELECT actor_id, root_session_id, epoch, owner_run_id, state FROM resident_actor_claim",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read resident_actor_claim table".to_owned(),
            source: err,
        })?;
        for row in rows {
            let actor_blob: Vec<u8> = row
                .try_get("actor_id")
                .map_err(sqlx_col("resident_actor_claim.actor_id"))?;
            let root_blob: Vec<u8> = row
                .try_get("root_session_id")
                .map_err(sqlx_col("resident_actor_claim.root_session_id"))?;
            let epoch: i64 = row
                .try_get("epoch")
                .map_err(sqlx_col("resident_actor_claim.epoch"))?;
            let owner_blob: Vec<u8> = row
                .try_get("owner_run_id")
                .map_err(sqlx_col("resident_actor_claim.owner_run_id"))?;
            let state: String = row
                .try_get("state")
                .map_err(sqlx_col("resident_actor_claim.state"))?;

            let claim_key = decode_session_id_blob(&actor_blob)?;
            let root_str = decode_session_id_blob(&root_blob)?;
            let owner = decode_owner_run_id_blob(&owner_blob)?;
            let metadata = json!({ "root_session_id": root_str }).to_string();

            sqlx::query(
                "INSERT OR REPLACE INTO bundle_claim (bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at, state)
                 VALUES (?, ?, ?, ?, NULL, ?, ?, ?, ?)",
            )
            .bind(BUNDLE_MULTI_AGENT)
            .bind(&claim_key)
            .bind(&owner)
            .bind(epoch)
            .bind(&metadata)
            .bind(now)
            .bind(now)
            .bind(&state)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: format!("migrate resident_actor_claim `{claim_key}` to bundle_claim"),
                source: err,
            })?;
            claims_migrated += 1;
        }
        if claims_migrated > 0 {
            let provenance = serde_json::to_vec(&json!({
                "source_table": "resident_actor_claim",
                "migrated_claims": claims_migrated,
                "migrated_at_ms": now,
            }))
            .map_err(|err| MigrationError::Json {
                context: "serialize bundle_state migration provenance".to_owned(),
                source: err,
            })?;
            sqlx::query(
                "INSERT OR REPLACE INTO bundle_state (bundle, key, value, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(BUNDLE_MULTI_AGENT)
            .bind("migration/v1_resident_claims")
            .bind(provenance)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "record migrated claims provenance in bundle_state".to_owned(),
                source: err,
            })?;
        }
    }

    // Copy remaining unchanged tables.
    if src_has("agent_model_preference") {
        let rows =
            sqlx::query("SELECT agent_id, provider_id, model_id FROM agent_model_preference")
                .fetch_all(&mut src)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "read agent_model_preference".to_owned(),
                    source: err,
                })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO agent_model_preference (agent_id, provider_id, model_id) VALUES (?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("agent_id").map_err(sqlx_col("agent_model_preference.agent_id"))?)
            .bind(row.try_get::<String, _>("provider_id").map_err(sqlx_col("agent_model_preference.provider_id"))?)
            .bind(row.try_get::<String, _>("model_id").map_err(sqlx_col("agent_model_preference.model_id"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert agent_model_preference".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("open_assistant_message") {
        let rows = sqlx::query("SELECT session_id, message_id FROM open_assistant_message")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read open_assistant_message".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO open_assistant_message (session_id, message_id) VALUES (?, ?)",
            )
            .bind(
                row.try_get::<Vec<u8>, _>("session_id")
                    .map_err(sqlx_col("open_assistant_message.session_id"))?,
            )
            .bind(
                row.try_get::<String, _>("message_id")
                    .map_err(sqlx_col("open_assistant_message.message_id"))?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert open_assistant_message".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("file_blob") {
        let rows = sqlx::query("SELECT session_id, hash, size, content FROM file_blob")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read file_blob".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO file_blob (session_id, hash, size, content) VALUES (?, ?, ?, ?)",
            )
            .bind(
                row.try_get::<Vec<u8>, _>("session_id")
                    .map_err(sqlx_col("file_blob.session_id"))?,
            )
            .bind(
                row.try_get::<String, _>("hash")
                    .map_err(sqlx_col("file_blob.hash"))?,
            )
            .bind(
                row.try_get::<i64, _>("size")
                    .map_err(sqlx_col("file_blob.size"))?,
            )
            .bind(
                row.try_get::<Vec<u8>, _>("content")
                    .map_err(sqlx_col("file_blob.content"))?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert file_blob".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("model_effort_preference") {
        let rows = sqlx::query(
            "SELECT provider_id, model_id, effort, updated_at FROM model_effort_preference",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read model_effort_preference".to_owned(),
            source: err,
        })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO model_effort_preference (provider_id, model_id, effort, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("provider_id").map_err(sqlx_col("model_effort_preference.provider_id"))?)
            .bind(row.try_get::<String, _>("model_id").map_err(sqlx_col("model_effort_preference.model_id"))?)
            .bind(row.try_get::<String, _>("effort").map_err(sqlx_col("model_effort_preference.effort"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("model_effort_preference.updated_at"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert model_effort_preference".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("pending_resume") {
        let rows = sqlx::query(
            "SELECT session_id, message_id, prompt_message_id, created_at FROM pending_resume",
        )
        .fetch_all(&mut src)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read pending_resume".to_owned(),
            source: err,
        })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO pending_resume (session_id, message_id, prompt_message_id, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(row.try_get::<Vec<u8>, _>("session_id").map_err(sqlx_col("pending_resume.session_id"))?)
            .bind(row.try_get::<String, _>("message_id").map_err(sqlx_col("pending_resume.message_id"))?)
            .bind(row.try_get::<Option<Vec<u8>>, _>("prompt_message_id").map_err(sqlx_col("pending_resume.prompt_message_id"))?)
            .bind(row.try_get::<i64, _>("created_at").map_err(sqlx_col("pending_resume.created_at"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert pending_resume".to_owned(),
                source: err,
            })?;
        }
    }

    if src_has("agent_effort_preference") {
        let rows = sqlx::query("SELECT agent_id, effort, updated_at FROM agent_effort_preference")
            .fetch_all(&mut src)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "read agent_effort_preference".to_owned(),
                source: err,
            })?;
        for row in rows {
            sqlx::query(
                "INSERT INTO agent_effort_preference (agent_id, effort, updated_at) VALUES (?, ?, ?)",
            )
            .bind(row.try_get::<String, _>("agent_id").map_err(sqlx_col("agent_effort_preference.agent_id"))?)
            .bind(row.try_get::<String, _>("effort").map_err(sqlx_col("agent_effort_preference.effort"))?)
            .bind(row.try_get::<i64, _>("updated_at").map_err(sqlx_col("agent_effort_preference.updated_at"))?)
            .execute(&mut *tx)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: "insert agent_effort_preference".to_owned(),
                source: err,
            })?;
        }
    }

    // Explicitly guarantee `schema_meta.format_version = '2'`.
    sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('format_version', '2')")
        .execute(&mut *tx)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "set schema_meta.format_version = 2".to_owned(),
            source: err,
        })?;

    tx.commit().await.map_err(|err| MigrationError::Sqlx {
        context: "commit v2 database migration".to_owned(),
        source: err,
    })?;

    // Note: `projection_snapshot` is intentionally left empty so v2 `hya-store`
    // folds fresh projections with v2 reducers from the migrated `event_log`.

    // 3. Verify foreign keys, integrity_check, and consolidate into a single file.
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut dst)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "re-enable foreign_keys".to_owned(),
            source: err,
        })?;

    let fk_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut dst)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "run PRAGMA foreign_key_check".to_owned(),
            source: err,
        })?;
    if !fk_violations.is_empty() {
        return Err(MigrationError::DatabaseIntegrity {
            path: display_path.to_path_buf(),
            detail: format!(
                "PRAGMA foreign_key_check reported {} violation(s)",
                fk_violations.len()
            ),
        });
    }

    let integrity_rows: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut dst)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "run PRAGMA integrity_check".to_owned(),
            source: err,
        })?;
    if integrity_rows != ["ok"] {
        return Err(MigrationError::DatabaseIntegrity {
            path: display_path.to_path_buf(),
            detail: format!(
                "PRAGMA integrity_check returned: {}",
                integrity_rows.join("; ")
            ),
        });
    }

    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut dst)
        .await;
    let _ = sqlx::query("PRAGMA journal_mode = DELETE")
        .execute(&mut dst)
        .await;

    let _ = src.close().await;
    let _ = dst.close().await;
    let _ = remove_sqlite_sidecars(temp_target);

    // 4. Verify v2 `hya-store` opens and replays every migrated session cleanly.
    let ro_store = SessionStore::connect_read_only(&temp_str)
        .await
        .map_err(|err| MigrationError::Store {
            context: format!(
                "verify migrated v2 database `{}` with hya-store",
                display_path.display()
            ),
            source: err,
        })?;
    let sessions = ro_store
        .list_sessions()
        .await
        .map_err(|err| MigrationError::Store {
            context: "list sessions during v2 verification".to_owned(),
            source: err,
        })?;
    for summary in &sessions {
        let _ = ro_store
            .replay(summary.session)
            .await
            .map_err(|err| MigrationError::Store {
                context: format!(
                    "replay session `{}` during v2 verification",
                    summary.session
                ),
                source: err,
            })?;
    }
    drop(ro_store);
    let _ = remove_sqlite_sidecars(temp_target);

    let mut notes = vec![
        "dropped stale v1 `projection_snapshot` cache; v2 projections will rebuild on first read"
            .to_owned(),
    ];
    if claims_migrated > 0 {
        notes.push(format!(
            "migrated {claims_migrated} `resident_actor_claim` row(s) to `bundle_claim` (`{BUNDLE_MULTI_AGENT}`)"
        ));
    }

    Ok(DbMigrationReport {
        path: display_path.to_path_buf(),
        migrated: true,
        sessions_copied,
        events_copied,
        events_rewritten,
        claims_migrated,
        integrity_ok: true,
        store_verified: true,
        notes,
    })
}

fn sqlx_col(column: &'static str) -> impl FnOnce(sqlx::Error) -> MigrationError {
    move |source| MigrationError::Sqlx {
        context: format!("decode column `{column}`"),
        source,
    }
}
