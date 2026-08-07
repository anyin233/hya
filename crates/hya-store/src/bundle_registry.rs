//! Durable installed-bundle registry (separate SQLite DB from the session store).

use std::str::FromStr;
use std::time::Duration;

use hya_bundle::{BundleCatalog, PreparedBundle, PreparedCatalog};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, Sqlite, Transaction};

use crate::StoreError;

/// SQLite registry of installed AgentBundles and generation counter.
#[derive(Clone, Debug)]
pub struct BundleRegistry {
    pool: sqlx::SqlitePool,
}

/// Point-in-time view of generation + all installed rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleRegistrySnapshot {
    /// Monotonic registry generation after last install/uninstall.
    pub generation: u64,
    /// Installed mutable bundles (builtins are passed separately by callers).
    pub bundles: Vec<BundleRegistryRecord>,
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

#[derive(Debug)]
struct LoadedBundleRegistrySnapshot {
    generation: u64,
    bundles: Vec<LoadedBundleRegistryRecord>,
}

#[derive(Debug)]
struct LoadedBundleRegistryRecord {
    record: BundleRegistryRecord,
    prepared: PreparedBundle,
}

impl LoadedBundleRegistrySnapshot {
    fn into_public_snapshot(self) -> BundleRegistrySnapshot {
        BundleRegistrySnapshot {
            generation: self.generation,
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
    pub async fn connect(path: &str) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::ZERO);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    /// Read the current registry generation singleton.
    pub async fn generation(&self) -> Result<u64, StoreError> {
        let row =
            sqlx::query("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
                .fetch_one(&self.pool)
                .await?;
        let generation: i64 = row.try_get("generation")?;
        decode_generation(generation)
    }

    /// Transactional snapshot of generation + all installed bundle rows.
    pub async fn snapshot(&self) -> Result<BundleRegistrySnapshot, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        transaction.commit().await?;
        Ok(snapshot.into_public_snapshot())
    }

    /// Install from a package inspection: public packages go through [`Self::install`]; private is rejected.
    pub async fn install_inspection(
        &self,
        reserved_agent_ids: &[&str],
        inspection: hya_bundle::PackageInspection,
        installed_at: i64,
    ) -> Result<BundleInstallOutcome, StoreError> {
        match inspection {
            hya_bundle::PackageInspection::Private(_) => {
                Err(StoreError::PrivateActivationUnsupported)
            }
            hya_bundle::PackageInspection::Public(public) => {
                self.install(
                    reserved_agent_ids,
                    BundleInstallCandidate {
                        source_digest: public.source_digest,
                        prepared_digest: public.prepared.digest().to_owned(),
                        prepared_bytes: public.prepared.bytes().to_vec(),
                        installed_at,
                    },
                )
                .await
            }
        }
    }

    /// Validate the candidate against the installed catalog, then insert or
    /// replace under an immediate lock.
    ///
    /// `reserved_agent_ids` are the compiled-in built-in agent ids. A bundle
    /// that claims one is rejected here rather than at catalog publish time, so
    /// the operator sees the failure at install.
    pub async fn install(
        &self,
        reserved_agent_ids: &[&str],
        candidate: BundleInstallCandidate,
    ) -> Result<BundleInstallOutcome, StoreError> {
        let BundleInstallCandidate {
            source_digest,
            prepared_digest,
            prepared_bytes,
            installed_at,
        } = candidate;
        let prepared = PreparedCatalog::decode(&prepared_bytes, &prepared_digest)?;
        let [incoming] = prepared.bundles() else {
            return Err(StoreError::BundleRegistryData(
                "install candidate must contain exactly one mutable installed bundle".to_string(),
            ));
        };
        let incoming = incoming.clone();
        let bundle_id = incoming.identity.id.clone();
        if reserved_agent_ids.contains(&incoming.agent.id.as_str()) {
            return Err(StoreError::BundleAgentIdReserved {
                bundle_id,
                agent_id: incoming.agent.id.as_str().to_string(),
            });
        }
        let version = incoming.identity.version.clone();
        let publisher = incoming.identity.publisher.clone();

        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                if is_sqlite_busy_or_locked(&error) {
                    StoreError::BundleRegistryBusy
                } else {
                    StoreError::from(error)
                }
            })?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        let existing = snapshot
            .bundles
            .iter()
            .find(|loaded| loaded.record.bundle_id == bundle_id);
        let replaces = existing.is_some_and(|loaded| {
            loaded.record.source_digest != source_digest && loaded.record.version != version
        });

        let mut complete = Vec::new();
        for loaded in &snapshot.bundles {
            if replaces && loaded.record.bundle_id == bundle_id {
                continue;
            }
            complete.push(loaded.prepared.clone());
        }
        if existing.is_none() || replaces {
            complete.push(incoming);
        }
        BundleCatalog::from_prepared(&complete)?;

        if let Some(existing) = existing {
            if existing.record.source_digest == source_digest {
                transaction.commit().await?;
                return Ok(BundleInstallOutcome::Unchanged {
                    generation: snapshot.generation,
                });
            }
            if existing.record.version == version {
                return Err(StoreError::BundleContentConflict { bundle_id, version });
            }
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
                return Err(StoreError::BundleRegistryData(
                    "bundle registry replacement row is missing".to_string(),
                ));
            }
            let generation = advance_generation(&mut transaction, snapshot.generation).await?;
            transaction.commit().await?;
            return Ok(BundleInstallOutcome::Replaced { generation });
        }

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

    /// Remove an installed bundle after re-validating the remaining catalog.
    pub async fn uninstall(
        &self,
        bundle_id: &str,
    ) -> Result<BundleUninstallOutcome, StoreError> {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| {
                if is_sqlite_busy_or_locked(&error) {
                    StoreError::BundleRegistryBusy
                } else {
                    StoreError::from(error)
                }
            })?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        if !snapshot
            .bundles
            .iter()
            .any(|loaded| loaded.record.bundle_id == bundle_id)
        {
            return Err(StoreError::BundleNotFound {
                bundle_id: bundle_id.to_string(),
            });
        }

        let mut complete = Vec::new();
        for loaded in &snapshot.bundles {
            if loaded.record.bundle_id == bundle_id {
                continue;
            }
            complete.push(loaded.prepared.clone());
        }
        if !complete.is_empty() {
            BundleCatalog::from_prepared(&complete)?;
        }

        let deleted = sqlx::query("DELETE FROM installed_bundle WHERE bundle_id = ?")
            .bind(bundle_id)
            .execute(&mut *transaction)
            .await?;
        if deleted.rows_affected() != 1 {
            return Err(StoreError::BundleRegistryData(
                "bundle registry uninstall row is missing".to_string(),
            ));
        }
        let generation = advance_generation(&mut transaction, snapshot.generation).await?;
        transaction.commit().await?;
        Ok(BundleUninstallOutcome::Removed { generation })
    }

    async fn snapshot_from_transaction(
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> Result<LoadedBundleRegistrySnapshot, StoreError> {
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
        for row in rows {
            let bundle_id: String = row.try_get("bundle_id")?;
            let source_digest: Vec<u8> = row.try_get("source_digest")?;
            let source_digest: [u8; 32] = source_digest.try_into().map_err(|digest: Vec<u8>| {
                StoreError::BundleRegistryData(format!(
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
            let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
                .map_err(|_| StoreError::BundleRegistryCorrupt {
                    bundle_id: record.bundle_id.clone(),
                })?;
            let [prepared_bundle] = prepared.bundles() else {
                return Err(StoreError::BundleRegistryCorrupt {
                    bundle_id: record.bundle_id.clone(),
                });
            };
            if prepared_bundle.identity.id.as_str() != record.bundle_id.as_str()
                || prepared_bundle.identity.version.as_str() != record.version.as_str()
                || prepared_bundle.identity.publisher.as_str() != record.publisher.as_str()
            {
                return Err(StoreError::BundleRegistryCorrupt {
                    bundle_id: record.bundle_id.clone(),
                });
            }
            bundles.push(LoadedBundleRegistryRecord {
                record,
                prepared: prepared_bundle.clone(),
            });
        }
        Ok(LoadedBundleRegistrySnapshot {
            generation,
            bundles,
        })
    }

    async fn migrate(pool: &sqlx::SqlitePool) -> Result<(), StoreError> {
        sqlx::migrate!("./bundle_migrations").run(pool).await?;
        Ok(())
    }
}

fn decode_generation(generation: i64) -> Result<u64, StoreError> {
    u64::try_from(generation).map_err(|_| {
        StoreError::BundleRegistryData(format!("invalid bundle registry generation `{generation}`"))
    })
}

async fn advance_generation(
    transaction: &mut Transaction<'_, Sqlite>,
    current_generation: u64,
) -> Result<u64, StoreError> {
    let generation = current_generation.checked_add(1).ok_or_else(|| {
        StoreError::BundleRegistryData("bundle registry generation overflow".to_string())
    })?;
    let stored_generation = i64::try_from(generation).map_err(|_| {
        StoreError::BundleRegistryData(
            "bundle registry generation exceeds SQLite range".to_string(),
        )
    })?;
    let updated =
        sqlx::query("UPDATE bundle_registry_generation SET generation = ? WHERE singleton = 1")
            .bind(stored_generation)
            .execute(&mut **transaction)
            .await?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::BundleRegistryData(
            "bundle registry generation row is missing".to_string(),
        ));
    }
    Ok(generation)
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
