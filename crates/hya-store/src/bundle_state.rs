//! Generic bundle-scoped key/value state storage (`bundle_state`).

use hya_proto::now_millis;
use sqlx::Row;

use crate::{SessionStore, StoreError, StoreTransaction};

/// One row stored in `bundle_state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleStateEntry {
    /// Owning bundle identifier.
    pub bundle: String,
    /// Key within the bundle's namespace.
    pub key: String,
    /// Raw stored bytes.
    pub value: Vec<u8>,
    /// Unix epoch milliseconds when the entry was last written.
    pub updated_at: i64,
}

fn validate_bundle_and_key(bundle: &str, key: &str) -> Result<(), StoreError> {
    if bundle.trim().is_empty() {
        return Err(StoreError::BundleStateData(
            "bundle identifier must not be empty".to_string(),
        ));
    }
    if key.is_empty() {
        return Err(StoreError::BundleStateData(
            "bundle_state key must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_bundle(bundle: &str) -> Result<(), StoreError> {
    if bundle.trim().is_empty() {
        return Err(StoreError::BundleStateData(
            "bundle identifier must not be empty".to_string(),
        ));
    }
    Ok(())
}

impl SessionStore {
    /// Read the raw value for `(bundle, key)`, or `None` if absent.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_state(
        &self,
        bundle: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        validate_bundle_and_key(bundle, key)?;
        let pool = self.read_pool_checked()?;
        let bundle = bundle.to_owned();
        let key = key.to_owned();
        self.run_read_bounded("get_bundle_state", async move {
            let value = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT value FROM bundle_state WHERE bundle = ? AND key = ?",
            )
            .bind(bundle)
            .bind(key)
            .fetch_optional(pool)
            .await?;
            Ok(value)
        })
        .await
    }

    /// Read the full [`BundleStateEntry`] (including `updated_at`) for `(bundle, key)`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_state_entry(
        &self,
        bundle: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, StoreError> {
        validate_bundle_and_key(bundle, key)?;
        let pool = self.read_pool_checked()?;
        let bundle_owned = bundle.to_owned();
        let key_owned = key.to_owned();
        self.run_read_bounded("get_bundle_state_entry", async move {
            let row = sqlx::query(
                "SELECT bundle, key, value, updated_at FROM bundle_state WHERE bundle = ? AND key = ?",
            )
            .bind(bundle_owned)
            .bind(key_owned)
            .fetch_optional(pool)
            .await?;
            row.map(decode_entry).transpose()
        })
        .await
    }

    /// Read and JSON-decode the value for `(bundle, key)`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`], [`StoreError::Json`], or SQLite errors.
    pub async fn get_bundle_state_json(
        &self,
        bundle: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let Some(bytes) = self.get_bundle_state(bundle, key).await? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&bytes)?;
        Ok(Some(value))
    }

    /// Upsert a raw byte value for `(bundle, key)` inside a `BEGIN IMMEDIATE` transaction.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn set_bundle_state(
        &self,
        bundle: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        let mut tx = self.begin_transaction().await?;
        Self::set_bundle_state_in_tx(&mut tx, bundle, key, value).await?;
        tx.commit().await?;
        Ok(())
    }

    /// JSON-encode and upsert a value for `(bundle, key)` inside a `BEGIN IMMEDIATE` transaction.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`], [`StoreError::Json`], or SQLite errors.
    pub async fn set_bundle_state_json(
        &self,
        bundle: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(value)?;
        self.set_bundle_state(bundle, key, &bytes).await
    }

    /// Atomically compare-and-set `(bundle, key)` inside a `BEGIN IMMEDIATE` transaction.
    ///
    /// - `expected == None`: succeeds only if the key is currently absent.
    /// - `expected == Some(prev)`: succeeds only if the key exists and equals `prev`.
    /// - `new_value == None`: deletes the key on match; `Some(next)` upserts `next`.
    ///
    /// Returns `true` when the transition was applied, `false` when the expected state did not match.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn compare_and_set_bundle_state(
        &self,
        bundle: &str,
        key: &str,
        expected: Option<&[u8]>,
        new_value: Option<&[u8]>,
    ) -> Result<bool, StoreError> {
        validate_bundle_and_key(bundle, key)?;
        let mut tx = self.begin_transaction().await?;
        let now = now_millis();
        let transitioned = match (expected, new_value) {
            (None, None) => {
                let exists: Option<i64> = sqlx::query_scalar(
                    "SELECT 1 FROM bundle_state WHERE bundle = ? AND key = ?",
                )
                .bind(bundle)
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?;
                exists.is_none()
            }
            (None, Some(next)) => {
                let res = sqlx::query(
                    "INSERT OR IGNORE INTO bundle_state (bundle, key, value, updated_at) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(bundle)
                .bind(key)
                .bind(next)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                res.rows_affected() == 1
            }
            (Some(prev), None) => {
                let res = sqlx::query(
                    "DELETE FROM bundle_state WHERE bundle = ? AND key = ? AND value = ?",
                )
                .bind(bundle)
                .bind(key)
                .bind(prev)
                .execute(&mut *tx)
                .await?;
                res.rows_affected() == 1
            }
            (Some(prev), Some(next)) => {
                let res = sqlx::query(
                    "UPDATE bundle_state SET value = ?, updated_at = ? \
                     WHERE bundle = ? AND key = ? AND value = ?",
                )
                .bind(next)
                .bind(now)
                .bind(bundle)
                .bind(key)
                .bind(prev)
                .execute(&mut *tx)
                .await?;
                res.rows_affected() == 1
            }
        };
        tx.commit().await?;
        Ok(transitioned)
    }

    /// Delete `(bundle, key)`. Returns `true` if a row existed.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn delete_bundle_state(&self, bundle: &str, key: &str) -> Result<bool, StoreError> {
        let mut tx = self.begin_transaction().await?;
        let deleted = Self::delete_bundle_state_in_tx(&mut tx, bundle, key).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    /// Delete all entries for `bundle` whose keys start with `prefix` (or all
    /// entries for `bundle` if `prefix` is empty). Returns the count of deleted rows.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn delete_bundle_state_prefix(
        &self,
        bundle: &str,
        prefix: &str,
    ) -> Result<u64, StoreError> {
        validate_bundle(bundle)?;
        let mut tx = self.begin_transaction().await?;
        let res = if prefix.is_empty() {
            sqlx::query("DELETE FROM bundle_state WHERE bundle = ?")
                .bind(bundle)
                .execute(&mut *tx)
                .await?
        } else {
            sqlx::query("DELETE FROM bundle_state WHERE bundle = ? AND instr(key, ?) = 1")
                .bind(bundle)
                .bind(prefix)
                .execute(&mut *tx)
                .await?
        };
        tx.commit().await?;
        Ok(res.rows_affected())
    }

    /// List keys for `bundle` matching optional `prefix`, sorted lexicographically.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn list_bundle_state_keys(
        &self,
        bundle: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<String>, StoreError> {
        validate_bundle(bundle)?;
        let pool = self.read_pool_checked()?;
        let bundle = bundle.to_owned();
        let prefix = prefix.filter(|p| !p.is_empty()).map(str::to_owned);
        self.run_read_bounded("list_bundle_state_keys", async move {
            let keys = match prefix {
                Some(prefix) => {
                    sqlx::query_scalar::<_, String>(
                        "SELECT key FROM bundle_state \
                         WHERE bundle = ? AND instr(key, ?) = 1 ORDER BY key",
                    )
                    .bind(bundle)
                    .bind(prefix)
                    .fetch_all(pool)
                    .await?
                }
                None => {
                    sqlx::query_scalar::<_, String>(
                        "SELECT key FROM bundle_state WHERE bundle = ? ORDER BY key",
                    )
                    .bind(bundle)
                    .fetch_all(pool)
                    .await?
                }
            };
            Ok(keys)
        })
        .await
    }

    /// List entries for `bundle` matching optional `prefix`, sorted by key.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn list_bundle_state(
        &self,
        bundle: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, StoreError> {
        validate_bundle(bundle)?;
        let pool = self.read_pool_checked()?;
        let bundle = bundle.to_owned();
        let prefix = prefix.filter(|p| !p.is_empty()).map(str::to_owned);
        self.run_read_bounded("list_bundle_state", async move {
            let rows = match prefix {
                Some(prefix) => {
                    sqlx::query(
                        "SELECT bundle, key, value, updated_at FROM bundle_state \
                         WHERE bundle = ? AND instr(key, ?) = 1 ORDER BY key",
                    )
                    .bind(bundle)
                    .bind(prefix)
                    .fetch_all(pool)
                    .await?
                }
                None => {
                    sqlx::query(
                        "SELECT bundle, key, value, updated_at FROM bundle_state \
                         WHERE bundle = ? ORDER BY key",
                    )
                    .bind(bundle)
                    .fetch_all(pool)
                    .await?
                }
            };
            rows.into_iter().map(decode_entry).collect()
        })
        .await
    }

    /// Read `(bundle, key)` inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_state_in_tx(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        validate_bundle_and_key(bundle, key)?;
        let value = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT value FROM bundle_state WHERE bundle = ? AND key = ?",
        )
        .bind(bundle)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(value)
    }

    /// Upsert `(bundle, key)` inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn set_bundle_state_in_tx(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        validate_bundle_and_key(bundle, key)?;
        sqlx::query(
            "INSERT INTO bundle_state (bundle, key, value, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(bundle, key) DO UPDATE SET \
                 value = excluded.value, \
                 updated_at = excluded.updated_at",
        )
        .bind(bundle)
        .bind(key)
        .bind(value)
        .bind(now_millis())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Delete `(bundle, key)` inside an open [`StoreTransaction`]. Returns `true` if a row existed.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleStateData`] for invalid inputs or SQLite errors.
    pub async fn delete_bundle_state_in_tx(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        validate_bundle_and_key(bundle, key)?;
        let res = sqlx::query("DELETE FROM bundle_state WHERE bundle = ? AND key = ?")
            .bind(bundle)
            .bind(key)
            .execute(&mut **tx)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

fn decode_entry(row: sqlx::sqlite::SqliteRow) -> Result<BundleStateEntry, StoreError> {
    Ok(BundleStateEntry {
        bundle: row.try_get("bundle")?,
        key: row.try_get("key")?,
        value: row.try_get("value")?,
        updated_at: row.try_get("updated_at")?,
    })
}
