//! Durable installed-bundle registry (separate SQLite DB from the session store).

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hya_bundle::{Bundle, BundleCatalog, PreparedCatalog};

pub use crate::bundle_install::{
    BundleInstallAction, BundleInstallPlan, NamespaceInstallPolicy, is_downgrade,
};
use crate::bundle_install::{InstallScope, check_reserved_agents, plan_install};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, Sqlite, Transaction};

const FILE_POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Failures from the installed-bundle registry and shared bundle install policy.
#[derive(Clone, thiserror::Error, Debug)]
pub enum BundleRegistryError {
    /// Underlying sqlx/SQLite I/O or query error.
    #[error("sqlite: {0}")]
    Sqlite(#[source] Arc<sqlx::Error>),
    /// Migration runner failure while applying `migrations/`.
    #[error("migrate: {0}")]
    Migrate(#[source] Arc<sqlx::migrate::MigrateError>),
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
    /// Another installed bundle already owns the incoming namespace.
    #[error(
        "NAMESPACE_CONFLICT: namespace {namespace} is owned by {existing_bundle_id}; \
         incoming bundle {incoming_bundle_id} requires an explicit overwrite"
    )]
    NamespaceConflict {
        /// Contested namespace.
        namespace: String,
        /// Bundle id that currently owns the namespace.
        existing_bundle_id: String,
        /// Bundle id that tried to claim it.
        incoming_bundle_id: String,
    },
    /// The incoming bundle version is lower than the installed one.
    #[error(
        "BUNDLE_DOWNGRADE_REQUIRED: bundle {bundle_id} is installed at {installed_version}; \
         installing {incoming_version} requires an explicit overwrite"
    )]
    BundleDowngradeRequired {
        /// Bundle id.
        bundle_id: String,
        /// Currently installed version.
        installed_version: String,
        /// Lower incoming version.
        incoming_version: String,
    },
    /// An install candidate claims an agent id reserved by a built-in agent.
    #[error(
        "BUNDLE_AGENT_ID_RESERVED: bundle {bundle_id} declares agent {agent_id}, \
         which is a reserved built-in agent id"
    )]
    BundleAgentIdReserved {
        /// Bundle that tried to claim the id.
        bundle_id: String,
        /// Reserved built-in agent id.
        agent_id: String,
    },
}

impl From<sqlx::Error> for BundleRegistryError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sqlite(Arc::new(error))
    }
}

impl From<sqlx::migrate::MigrateError> for BundleRegistryError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate(Arc::new(error))
    }
}

/// SQLite registry of installed bundles and generation counter.
#[derive(Clone, Debug)]
pub struct BundleRegistry {
    pool: sqlx::SqlitePool,
}

/// Point-in-time view of generation + all installed rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleRegistrySnapshot {
    /// Monotonic registry generation after last install/uninstall.
    pub generation: u64,
    /// Readable installed mutable bundles (builtins are passed separately).
    pub bundles: Vec<BundleRegistryRecord>,
    /// Rows whose prepared payload cannot be read by this version.
    pub unreadable: Vec<UnreadableBundleRegistryRecord>,
}

/// Identity and diagnostic for an installed row that cannot be published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnreadableBundleRegistryRecord {
    /// Bundle identity id, read from registry metadata without decoding the payload.
    pub bundle_id: String,
    /// Installed version string.
    pub version: String,
    /// Installed publisher.
    pub publisher: String,
    /// The payload validation error and reinstall instruction.
    pub error: String,
}

/// One installed bundle row: identity, digests, and prepared catalog bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleRegistryRecord {
    /// Bundle identity id (`publisher/name`).
    pub bundle_id: String,
    /// Installed version string.
    pub version: String,
    /// Publisher field from the prepared bundle.
    pub publisher: String,
    /// Digest of the source package contents.
    pub source_digest: [u8; 32],
    /// Hex digest of `prepared_bytes`.
    pub prepared_digest: String,
    /// Canonical prepared-catalog JSON for this bundle alone.
    pub prepared_bytes: Vec<u8>,
    /// Install timestamp (unix millis).
    pub installed_at: i64,
}

/// Point-in-time registry view with validated prepared catalogs.
#[derive(Debug)]
pub struct LoadedBundleRegistrySnapshot {
    /// Monotonic registry generation after last install or uninstall.
    pub generation: u64,
    /// Readable installed bundles and their decoded catalogs.
    pub bundles: Vec<LoadedBundleRegistryRecord>,
    /// Rows whose prepared payload cannot be read by this version.
    pub unreadable: Vec<UnreadableBundleRegistryRecord>,
}

/// A registry row paired with its validated catalog.
#[derive(Debug)]
pub struct LoadedBundleRegistryRecord {
    /// The stored row metadata and bytes.
    pub record: BundleRegistryRecord,
    /// Decoded single-bundle catalog that matches the stored row.
    pub prepared: PreparedCatalog,
}

impl LoadedBundleRegistrySnapshot {
    fn into_public_snapshot(self) -> BundleRegistrySnapshot {
        BundleRegistrySnapshot {
            generation: self.generation,
            unreadable: self.unreadable,
            bundles: self
                .bundles
                .into_iter()
                .map(|loaded| loaded.record)
                .collect(),
        }
    }
}

/// Payload required to install or replace a public package in the registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleInstallCandidate {
    /// Digest of the source package (identity of content).
    pub source_digest: [u8; 32],
    /// Expected digest of `prepared_bytes` for decode verification.
    pub prepared_digest: String,
    /// Single-bundle prepared catalog encoding.
    pub prepared_bytes: Vec<u8>,
    /// Timestamp written to the row.
    pub installed_at: i64,
}

/// Result of an install attempt after catalog merge validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleInstallOutcome {
    /// New `bundle_id` inserted; registry generation advanced.
    Installed {
        /// Generation after insert.
        generation: u64,
    },
    /// Existing id replaced with a new version/content; generation advanced.
    Replaced {
        /// Generation after replace.
        generation: u64,
    },
    /// Same `source_digest` already present; generation unchanged.
    Unchanged {
        /// Current generation.
        generation: u64,
    },
}

/// Successful uninstall advances the registry generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleUninstallOutcome {
    /// Bundle row removed; generation advanced.
    Removed {
        /// Generation after removal.
        generation: u64,
    },
}

impl BundleRegistry {
    /// Open or create the registry database at `path` and run migrations (synchronous=Full).
    /// Connection setup retries transient SQLite locks for up to five seconds;
    /// registry mutations retain their zero busy timeout.
    pub async fn connect(path: &str) -> Result<Self, BundleRegistryError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match Self::connect_once(path).await {
                Err(error) if setup_busy(&error) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                result => return result,
            }
        }
    }

    async fn connect_once(path: &str) -> Result<Self, BundleRegistryError> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::ZERO);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(FILE_POOL_WAIT_TIMEOUT)
            // Closing the last connection of a WAL database checkpoints under an exclusive lock.
            .max_lifetime(None)
            .idle_timeout(None)
            .connect_with(options)
            .await?;
        if let Err(error) = Self::migrate(&pool).await {
            pool.close().await;
            return Err(error);
        }
        Ok(Self { pool })
    }

    /// Read the current registry generation singleton.
    pub async fn generation(&self) -> Result<u64, BundleRegistryError> {
        let row =
            sqlx::query("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
                .fetch_one(&self.pool)
                .await?;
        let generation: i64 = row.try_get("generation")?;
        decode_generation(generation)
    }
    /// Return the ids disabled for every catalog scope.
    pub async fn disabled_bundle_ids(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, BundleRegistryError> {
        let rows = sqlx::query("SELECT bundle_id FROM bundle_disabled ORDER BY bundle_id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<String, _>("bundle_id")
                    .map_err(BundleRegistryError::from)
            })
            .collect()
    }

    /// Persist whether a bundle is disabled and advance the watched generation.
    pub async fn set_bundle_enabled(
        &self,
        bundle_id: &str,
        enabled: bool,
    ) -> Result<u64, BundleRegistryError> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let current =
            sqlx::query("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
                .fetch_one(&mut *transaction)
                .await?
                .try_get::<i64, _>("generation")?;
        let changed = if enabled {
            sqlx::query("DELETE FROM bundle_disabled WHERE bundle_id = ?")
                .bind(bundle_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
                != 0
        } else {
            sqlx::query("INSERT OR IGNORE INTO bundle_disabled(bundle_id) VALUES (?)")
                .bind(bundle_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
                != 0
        };
        let generation = if changed {
            advance_generation(&mut transaction, decode_generation(current)?).await?
        } else {
            decode_generation(current)?
        };
        transaction.commit().await?;
        Ok(generation)
    }

    /// Transactional snapshot of generation + all installed bundle rows.
    pub async fn snapshot(&self) -> Result<BundleRegistrySnapshot, BundleRegistryError> {
        Ok(self.snapshot_with_catalogs().await?.into_public_snapshot())
    }

    /// Transactional snapshot with validated catalogs for consumers that need bundle content.
    pub async fn snapshot_with_catalogs(
        &self,
    ) -> Result<LoadedBundleRegistrySnapshot, BundleRegistryError> {
        let mut transaction = self.pool.begin().await?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        transaction.commit().await?;
        Ok(snapshot)
    }

    /// Validate the candidate against the installed catalog, then insert or
    /// replace under an immediate lock.
    ///
    /// `reserved_agent_ids` are the compiled-in built-in Agent ids. A bundle
    /// that claims any one in its complete Agent closure is rejected here rather
    /// than at catalog publish time, so the operator sees the failure at install.
    pub async fn install(
        &self,
        reserved_agent_ids: &[&str],
        policy: NamespaceInstallPolicy,
        candidate: BundleInstallCandidate,
    ) -> Result<BundleInstallOutcome, BundleRegistryError> {
        let incoming = decode_candidate(reserved_agent_ids, &candidate)?;
        let BundleInstallCandidate {
            source_digest,
            prepared_digest,
            prepared_bytes,
            installed_at,
        } = candidate;
        let identity = incoming.identity();
        let bundle_id = identity.id.clone();
        let version = identity.version.clone();
        let publisher = identity.publisher.clone();

        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                if is_sqlite_busy_or_locked(&error) {
                    BundleRegistryError::BundleRegistryBusy
                } else {
                    BundleRegistryError::from(error)
                }
            })?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        let plan = match plan_registry_install(&snapshot, policy, &incoming, &source_digest) {
            Ok(plan) => plan,
            Err(refused) => return refuse(transaction, refused).await,
        };

        for loser in &plan.displaced {
            sqlx::query("DELETE FROM installed_bundle WHERE bundle_id = ?")
                .bind(loser)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM bundle_disabled WHERE bundle_id = ?")
                .bind(loser)
                .execute(&mut *transaction)
                .await?;
        }

        match plan.action {
            BundleInstallAction::Unchanged => {
                transaction.commit().await?;
                Ok(BundleInstallOutcome::Unchanged {
                    generation: snapshot.generation,
                })
            }
            BundleInstallAction::Replace { .. } => {
                let replaced = sqlx::query(
                    "UPDATE installed_bundle SET version = ?, publisher = ?, source_digest = ?, prepared_digest = ?,
                     prepared_bytes = ?, installed_at = ? WHERE bundle_id = ?",
                )
                .bind(version)
                .bind(publisher)
                .bind(source_digest.to_vec())
                .bind(prepared_digest)
                .bind(prepared_bytes)
                .bind(installed_at)
                .bind(bundle_id)
                .execute(&mut *transaction)
                .await?;
                if replaced.rows_affected() != 1 {
                    return Err(BundleRegistryError::BundleRegistryData(
                        "bundle registry replacement row is missing".to_string(),
                    ));
                }
                let generation = advance_generation(&mut transaction, snapshot.generation).await?;
                transaction.commit().await?;
                Ok(BundleInstallOutcome::Replaced { generation })
            }
            BundleInstallAction::Install => {
                sqlx::query(
                    "INSERT INTO installed_bundle
                     (bundle_id, version, publisher, source_digest, prepared_digest, prepared_bytes, installed_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(bundle_id)
                .bind(version)
                .bind(publisher)
                .bind(source_digest.to_vec())
                .bind(prepared_digest)
                .bind(prepared_bytes)
                .bind(installed_at)
                .execute(&mut *transaction)
                .await?;
                let generation = advance_generation(&mut transaction, snapshot.generation).await?;
                transaction.commit().await?;
                Ok(BundleInstallOutcome::Installed { generation })
            }
        }
    }

    /// Report what [`Self::install`] would do with `candidate` under `policy`,
    /// running the same validation (reserved Agent ids, downgrade, namespace,
    /// content, and merged-catalog checks) against a read snapshot. Nothing is
    /// written and the generation never moves; a concurrent writer can still
    /// change the outcome before a later install.
    pub async fn plan_install(
        &self,
        reserved_agent_ids: &[&str],
        policy: NamespaceInstallPolicy,
        candidate: &BundleInstallCandidate,
    ) -> Result<BundleInstallPlan, BundleRegistryError> {
        let incoming = decode_candidate(reserved_agent_ids, candidate)?;
        let mut transaction = self.pool.begin().await?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        transaction.commit().await?;
        plan_registry_install(&snapshot, policy, &incoming, &candidate.source_digest)
    }

    /// Remove an installed bundle after re-validating the remaining catalog.
    pub async fn uninstall(
        &self,
        bundle_id: &str,
    ) -> Result<BundleUninstallOutcome, BundleRegistryError> {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                if is_sqlite_busy_or_locked(&error) {
                    BundleRegistryError::BundleRegistryBusy
                } else {
                    BundleRegistryError::from(error)
                }
            })?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        if !snapshot
            .bundles
            .iter()
            .any(|loaded| loaded.record.bundle_id == bundle_id)
            && !snapshot
                .unreadable
                .iter()
                .any(|row| row.bundle_id == bundle_id)
        {
            let missing = BundleRegistryError::BundleNotFound {
                bundle_id: bundle_id.to_string(),
            };
            return refuse(transaction, missing).await;
        }

        let mut complete = Vec::new();
        for loaded in &snapshot.bundles {
            if loaded.record.bundle_id == bundle_id {
                continue;
            }
            complete.push(loaded.prepared.bundles()[0].clone());
        }
        if !complete.is_empty()
            && let Err(invalid) = BundleCatalog::from_prepared(&complete)
        {
            return refuse(transaction, invalid.into()).await;
        }

        let deleted = sqlx::query("DELETE FROM installed_bundle WHERE bundle_id = ?")
            .bind(bundle_id)
            .execute(&mut *transaction)
            .await?;
        if deleted.rows_affected() != 1 {
            return Err(BundleRegistryError::BundleRegistryData(
                "bundle registry uninstall row is missing".to_string(),
            ));
        }
        sqlx::query("DELETE FROM bundle_disabled WHERE bundle_id = ?")
            .bind(bundle_id)
            .execute(&mut *transaction)
            .await?;
        let generation = advance_generation(&mut transaction, snapshot.generation).await?;
        transaction.commit().await?;
        Ok(BundleUninstallOutcome::Removed { generation })
    }

    async fn snapshot_from_transaction(
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> Result<LoadedBundleRegistrySnapshot, BundleRegistryError> {
        let generation_row =
            sqlx::query("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
                .fetch_one(&mut **transaction)
                .await?;
        let generation: i64 = generation_row.try_get("generation")?;
        let generation = decode_generation(generation)?;

        let rows = sqlx::query(
            "SELECT bundle_id, version, publisher, source_digest, prepared_digest, prepared_bytes, installed_at
             FROM installed_bundle ORDER BY bundle_id COLLATE BINARY",
        )
        .fetch_all(&mut **transaction)
        .await?;
        let mut bundles = Vec::with_capacity(rows.len());
        let mut unreadable = Vec::new();
        for row in rows {
            let bundle_id: String = row.try_get("bundle_id")?;
            let source_digest: Vec<u8> = row.try_get("source_digest")?;
            let source_digest: [u8; 32] = source_digest.try_into().map_err(|digest: Vec<u8>| {
                BundleRegistryError::BundleRegistryData(format!(
                    "bundle `{bundle_id}` source digest has {} bytes, expected 32",
                    digest.len()
                ))
            })?;
            let record = BundleRegistryRecord {
                version: row.try_get("version")?,
                publisher: row.try_get("publisher")?,
                prepared_digest: row.try_get("prepared_digest")?,
                prepared_bytes: row.try_get("prepared_bytes")?,
                installed_at: row.try_get("installed_at")?,
                bundle_id,
                source_digest,
            };
            let decoded = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
                .map_err(|error| error.to_string())
                .and_then(|prepared| {
                    let [bundle] = prepared.bundles() else {
                        return Err("prepared catalog must contain exactly one bundle".to_string());
                    };
                    let identity = bundle.identity();
                    if identity.id != record.bundle_id
                        || identity.version != record.version
                        || identity.publisher != record.publisher
                    {
                        return Err(
                            "registry metadata does not match the prepared catalog".to_string()
                        );
                    }
                    Ok(prepared)
                });
            match decoded {
                Ok(prepared) => bundles.push(LoadedBundleRegistryRecord { record, prepared }),
                Err(error) => unreadable.push(UnreadableBundleRegistryRecord {
                    bundle_id: record.bundle_id,
                    version: record.version,
                    publisher: record.publisher,
                    error: format!(
                        "{error}; please reinstall the bundle with `hya bundle install`"
                    ),
                }),
            }
        }
        Ok(LoadedBundleRegistrySnapshot {
            generation,
            bundles,
            unreadable,
        })
    }

    async fn migrate(pool: &sqlx::SqlitePool) -> Result<(), BundleRegistryError> {
        // SQLite's sqlx migration lock is a no-op. Reserve the writer before
        // reading migration history so concurrent openers cannot apply the
        // same version from stale history. Individual migrations use savepoints.
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::migrate!("./bundle_migrations")
            .run_direct(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

/// Decode an install candidate into its single bundle and reject any Agent id
/// that collides with a compiled-in built-in.
fn decode_candidate(
    reserved_agent_ids: &[&str],
    candidate: &BundleInstallCandidate,
) -> Result<Bundle, BundleRegistryError> {
    let prepared = PreparedCatalog::decode(&candidate.prepared_bytes, &candidate.prepared_digest)?;
    let [incoming] = prepared.bundles() else {
        return Err(BundleRegistryError::BundleRegistryData(
            "install candidate must contain exactly one mutable installed bundle".to_string(),
        ));
    };
    check_reserved_agents(incoming, reserved_agent_ids)?;
    Ok(incoming.clone())
}

fn plan_registry_install(
    snapshot: &LoadedBundleRegistrySnapshot,
    policy: NamespaceInstallPolicy,
    incoming: &Bundle,
    source_digest: &[u8; 32],
) -> Result<BundleInstallPlan, BundleRegistryError> {
    let installed = snapshot
        .bundles
        .iter()
        .map(|loaded| (&loaded.prepared.bundles()[0], &loaded.record.source_digest));
    let mut plan = plan_install(
        installed,
        incoming,
        source_digest,
        policy,
        InstallScope::User,
    )?;
    // An unreadable row has no validated version to compare; replacing it repairs that id.
    if plan.action == BundleInstallAction::Install
        && let Some(row) = snapshot
            .unreadable
            .iter()
            .find(|row| row.bundle_id == incoming.identity().id)
    {
        plan.action = BundleInstallAction::Replace {
            installed_version: row.version.clone(),
        };
    }
    Ok(plan)
}

fn decode_generation(generation: i64) -> Result<u64, BundleRegistryError> {
    u64::try_from(generation).map_err(|_| {
        BundleRegistryError::BundleRegistryData(format!(
            "invalid bundle registry generation `{generation}`"
        ))
    })
}

async fn advance_generation(
    transaction: &mut Transaction<'_, Sqlite>,
    current_generation: u64,
) -> Result<u64, BundleRegistryError> {
    let generation = current_generation.checked_add(1).ok_or_else(|| {
        BundleRegistryError::BundleRegistryData("bundle registry generation overflow".to_string())
    })?;
    let stored_generation = i64::try_from(generation).map_err(|_| {
        BundleRegistryError::BundleRegistryData(
            "bundle registry generation exceeds SQLite range".to_string(),
        )
    })?;
    let updated =
        sqlx::query("UPDATE bundle_registry_generation SET generation = ? WHERE singleton = 1")
            .bind(stored_generation)
            .execute(&mut **transaction)
            .await?;
    if updated.rows_affected() != 1 {
        return Err(BundleRegistryError::BundleRegistryData(
            "bundle registry generation row is missing".to_string(),
        ));
    }
    Ok(generation)
}

/// End a write transaction that refused its change: roll it back now and
/// return `refused`. A dropped transaction rolls back only when its pooled
/// connection is returned later, and keeps the `BEGIN IMMEDIATE` write lock
/// until then; with `busy_timeout(0)` a retry right after the refusal (for
/// example `install --overwrite`) would fail as `BundleRegistryBusy`.
async fn refuse<T>(
    transaction: Transaction<'_, Sqlite>,
    refused: BundleRegistryError,
) -> Result<T, BundleRegistryError> {
    transaction.rollback().await?;
    Err(refused)
}

fn setup_busy(error: &BundleRegistryError) -> bool {
    match error {
        BundleRegistryError::Sqlite(error) => is_sqlite_busy_or_locked(error),
        BundleRegistryError::Migrate(error) => match error.as_ref() {
            sqlx::migrate::MigrateError::Execute(error)
            | sqlx::migrate::MigrateError::ExecuteMigration(error, _) => {
                is_sqlite_busy_or_locked(error)
            }
            _ => false,
        },
        _ => false,
    }
}

fn is_sqlite_busy_or_locked(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(database_error) = error else {
        return false;
    };
    database_error
        .code()
        .and_then(|code| code.parse::<u32>().ok())
        .is_some_and(|code| matches!(code & 0xff, 5 | 6))
}

#[cfg(test)]
mod disabled_tests {
    use super::*;

    #[tokio::test]
    async fn replacement_preserves_disabled_and_removal_clears_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "hya-registry-disabled-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let registry = BundleRegistry::connect(&path.to_string_lossy()).await?;
        for (id, version, digest) in [
            ("test/original", "1.0.0", 1),
            ("test/original", "2.0.0", 2),
            ("test/original", "1.0.0", 4),
            ("test/replacement", "1.0.0", 3),
        ] {
            let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
                id,
                vec![hya_bundle::SourceFile::new(
                    "bundle.yaml",
                    format!(
                        "kind: Bundle\nidentity: {{ id: {id}, version: {version}, publisher: test }}\nnamespace: shared\n"
                    ),
                )],
            ))?;
            registry
                .install(
                    &[],
                    if id == "test/replacement" {
                        NamespaceInstallPolicy::OverwriteConflicts
                    } else {
                        NamespaceInstallPolicy::DenyConflicts
                    },
                    BundleInstallCandidate {
                        source_digest: [digest; 32],
                        prepared_digest: prepared.digest().to_string(),
                        prepared_bytes: prepared.bytes().to_vec(),
                        installed_at: 1,
                    },
                )
                .await?;
            if version == "2.0.0" || digest == 4 {
                assert!(registry.disabled_bundle_ids().await?.contains(id));
            }
            if version == "2.0.0" {
                sqlx::query("UPDATE installed_bundle SET prepared_bytes = ? WHERE bundle_id = ?")
                    .bind(vec![0_u8])
                    .bind(id)
                    .execute(&registry.pool)
                    .await?;
                assert_eq!(registry.snapshot().await?.unreadable.len(), 1);
            }
            if id == "test/replacement" {
                assert!(
                    !registry
                        .disabled_bundle_ids()
                        .await?
                        .contains("test/original")
                );
            }
            registry.set_bundle_enabled(id, false).await?;
        }
        registry.uninstall("test/replacement").await?;
        assert!(registry.disabled_bundle_ids().await?.is_empty());
        registry.pool.close().await;
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn bundle_registry_pool_disables_lifetime_and_idle_retirement()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "hya-registry-pool-opts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("registry.sqlite3");
        let registry = BundleRegistry::connect(&path.to_string_lossy()).await?;
        assert_eq!(
            registry.pool.options().get_acquire_timeout(),
            FILE_POOL_WAIT_TIMEOUT
        );
        assert_eq!(registry.pool.options().get_max_lifetime(), None);
        assert_eq!(registry.pool.options().get_idle_timeout(), None);
        registry.pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }
}
