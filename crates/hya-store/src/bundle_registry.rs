use std::str::FromStr;
use std::time::Duration;

use hya_bundle::{BundleCatalog, BundleOrigin, PreparedBundle, PreparedCatalog};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, Sqlite, Transaction};

use crate::StoreError;

#[derive(Clone, Debug)]
pub struct BundleRegistry {
    pool: sqlx::SqlitePool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleRegistrySnapshot {
    pub generation: u64,
    pub bundles: Vec<BundleRegistryRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleRegistryRecord {
    pub bundle_id: String,
    pub version: String,
    pub publisher: String,
    pub source_digest: [u8; 32],
    pub prepared_digest: String,
    pub prepared_bytes: Vec<u8>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleInstallCandidate {
    pub source_digest: [u8; 32],
    pub prepared_digest: String,
    pub prepared_bytes: Vec<u8>,
    pub installed_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleInstallOutcome {
    Installed { generation: u64 },
    Replaced { generation: u64 },
    Unchanged { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleUninstallOutcome {
    Removed { generation: u64 },
}

impl BundleRegistry {
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

    pub async fn generation(&self) -> Result<u64, StoreError> {
        let row =
            sqlx::query("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
                .fetch_one(&self.pool)
                .await?;
        let generation: i64 = row.try_get("generation")?;
        decode_generation(generation)
    }

    pub async fn snapshot(&self) -> Result<BundleRegistrySnapshot, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let snapshot = Self::snapshot_from_transaction(&mut transaction).await?;
        transaction.commit().await?;
        Ok(snapshot.into_public_snapshot())
    }

    pub async fn install_inspection(
        &self,
        builtins: &[hya_bundle::PreparedBundle],
        inspection: hya_bundle::PackageInspection,
        installed_at: i64,
    ) -> Result<BundleInstallOutcome, StoreError> {
        match inspection {
            hya_bundle::PackageInspection::Private(_) => {
                Err(StoreError::PrivateActivationUnsupported)
            }
            hya_bundle::PackageInspection::Public(public) => {
                self.install(
                    builtins,
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

    pub async fn install(
        &self,
        builtins: &[hya_bundle::PreparedBundle],
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
        if incoming.origin != BundleOrigin::Installed || incoming.immutable {
            return Err(StoreError::BundleRegistryData(
                "install candidate must contain exactly one mutable installed bundle".to_string(),
            ));
        }
        let incoming = incoming.clone();
        let bundle_id = incoming.identity.id.clone();
        if is_immutable_builtin(builtins, &bundle_id) {
            return Err(StoreError::BundleImmutable { bundle_id });
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

        let mut complete = builtins.to_vec();
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

    pub async fn uninstall(
        &self,
        builtins: &[hya_bundle::PreparedBundle],
        bundle_id: &str,
    ) -> Result<BundleUninstallOutcome, StoreError> {
        if is_immutable_builtin(builtins, bundle_id) {
            return Err(StoreError::BundleImmutable {
                bundle_id: bundle_id.to_owned(),
            });
        }
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

        let mut complete = builtins.to_vec();
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
            if prepared_bundle.origin != BundleOrigin::Installed
                || prepared_bundle.immutable
                || prepared_bundle.identity.id.as_str() != record.bundle_id.as_str()
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

fn is_immutable_builtin(builtins: &[hya_bundle::PreparedBundle], bundle_id: &str) -> bool {
    builtins.iter().any(|builtin| {
        builtin.identity.id.as_str() == bundle_id
            && builtin.origin == BundleOrigin::Builtin
            && builtin.immutable
    })
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
