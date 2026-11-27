//! Generic bundle-scoped lease/claim table (`bundle_claim`).
//!
//! Replaces the v1 domain-specific `resident_actor_claim` table with a generic
//! `(bundle, claim_key)` lease carrying `owner`, monotonic `epoch`, optional
//! TTL `expires_at`, and opaque JSON `metadata`.

use hya_proto::{OwnerRunId, now_millis};
use sqlx::Row;

use crate::{SessionStore, StoreError, StoreTransaction};

/// Active or recovered bundle-scoped lease/claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleClaim {
    /// Owning bundle identifier.
    pub bundle: String,
    /// Resource key claimed within the bundle.
    pub claim_key: String,
    /// Current owner identifier.
    pub owner: String,
    /// Monotonic claim incarnation epoch (`>= 1`).
    pub epoch: u64,
    /// Optional expiry timestamp in Unix epoch milliseconds (`None` = no TTL).
    pub expires_at: Option<i64>,
    /// Opaque JSON metadata attached to the lease.
    pub metadata: serde_json::Value,
    /// Unix epoch milliseconds when this incarnation was acquired.
    pub acquired_at: i64,
    /// Unix epoch milliseconds when this lease was last updated/renewed.
    pub updated_at: i64,
}

fn validate_claim_args(bundle: &str, claim_key: &str, owner: &str) -> Result<(), StoreError> {
    if bundle.trim().is_empty() {
        return Err(StoreError::BundleClaimData(
            "bundle identifier must not be empty".to_string(),
        ));
    }
    if claim_key.is_empty() {
        return Err(StoreError::BundleClaimData(
            "claim_key must not be empty".to_string(),
        ));
    }
    if owner.trim().is_empty() {
        return Err(StoreError::BundleClaimData(
            "owner must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_bundle_and_key(bundle: &str, claim_key: &str) -> Result<(), StoreError> {
    if bundle.trim().is_empty() {
        return Err(StoreError::BundleClaimData(
            "bundle identifier must not be empty".to_string(),
        ));
    }
    if claim_key.is_empty() {
        return Err(StoreError::BundleClaimData(
            "claim_key must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn compute_expires_at(now_ms: i64, ttl_ms: Option<u64>) -> Option<i64> {
    ttl_ms.map(|ttl| now_ms.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)))
}

fn epoch_to_i64(epoch: u64) -> Result<i64, StoreError> {
    if epoch == 0 {
        return Err(StoreError::BundleClaimData(
            "claim epoch must be >= 1".to_string(),
        ));
    }
    i64::try_from(epoch)
        .map_err(|_| StoreError::BundleClaimData("claim epoch exceeds SQLite INTEGER range".into()))
}

impl SessionStore {
    /// Atomically acquire a never-claimed, released, or expired lease at the current time.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimConflict`] if an active non-expired lease exists,
    /// [`StoreError::BundleClaimData`] for invalid inputs, or SQLite errors.
    pub async fn acquire_bundle_claim(
        &self,
        bundle: &str,
        claim_key: &str,
        owner: &str,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<BundleClaim, StoreError> {
        self.acquire_bundle_claim_at(bundle, claim_key, owner, ttl_ms, metadata, now_millis())
            .await
    }

    /// Atomically acquire a lease using an explicit `now_ms` timestamp (useful for deterministic tests).
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimConflict`] if an active non-expired lease exists,
    /// [`StoreError::BundleClaimData`] for invalid inputs, or SQLite errors.
    pub async fn acquire_bundle_claim_at(
        &self,
        bundle: &str,
        claim_key: &str,
        owner: &str,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
        now_ms: i64,
    ) -> Result<BundleClaim, StoreError> {
        let mut tx = self.begin_transaction().await?;
        let claim = Self::acquire_bundle_claim_in_tx_at(
            &mut tx, bundle, claim_key, owner, ttl_ms, metadata, now_ms,
        )
        .await?;
        tx.commit().await?;
        Ok(claim)
    }

    /// Atomically acquire a lease inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimConflict`] if an active non-expired lease exists,
    /// [`StoreError::BundleClaimData`] for invalid inputs, or SQLite errors.
    pub async fn acquire_bundle_claim_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        claim_key: &str,
        owner: &str,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
        now_ms: i64,
    ) -> Result<BundleClaim, StoreError> {
        validate_claim_args(bundle, claim_key, owner)?;
        let expires_at = compute_expires_at(now_ms, ttl_ms);
        let metadata_json = match metadata {
            Some(value) => serde_json::to_string(value)?,
            None => "{}".to_string(),
        };
        let row = sqlx::query(
            "INSERT INTO bundle_claim \
             (bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at, state) \
             VALUES (?, ?, ?, 1, ?, ?, ?, ?, 'active') \
             ON CONFLICT(bundle, claim_key) DO UPDATE SET \
                 owner = excluded.owner, \
                 epoch = bundle_claim.epoch + 1, \
                 expires_at = excluded.expires_at, \
                 metadata = excluded.metadata, \
                 acquired_at = excluded.acquired_at, \
                 updated_at = excluded.updated_at, \
                 state = 'active' \
             WHERE (bundle_claim.state = 'released' \
                    OR (bundle_claim.expires_at IS NOT NULL AND bundle_claim.expires_at <= ?)) \
               AND bundle_claim.epoch < 9223372036854775807 \
             RETURNING bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at",
        )
        .bind(bundle)
        .bind(claim_key)
        .bind(owner)
        .bind(expires_at)
        .bind(metadata_json)
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms)
        .fetch_optional(&mut **tx)
        .await?;

        match row {
            Some(row) => decode_claim(row),
            None => Err(StoreError::BundleClaimConflict {
                bundle: bundle.to_owned(),
                claim_key: claim_key.to_owned(),
            }),
        }
    }

    /// Renew an active, non-expired lease held by `claim`'s exact `(owner, epoch)`.
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] if the lease was released, expired, or taken over.
    pub async fn renew_bundle_claim(
        &self,
        claim: &BundleClaim,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<BundleClaim, StoreError> {
        self.renew_bundle_claim_at(claim, ttl_ms, metadata, now_millis())
            .await
    }

    /// Renew a lease using an explicit `now_ms` timestamp.
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] if the lease was released, expired, or taken over.
    pub async fn renew_bundle_claim_at(
        &self,
        claim: &BundleClaim,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
        now_ms: i64,
    ) -> Result<BundleClaim, StoreError> {
        validate_claim_args(&claim.bundle, &claim.claim_key, &claim.owner)?;
        let epoch = epoch_to_i64(claim.epoch)?;
        let expires_at = compute_expires_at(now_ms, ttl_ms);
        let metadata_json = metadata.map(serde_json::to_string).transpose()?;
        let mut tx = self.begin_transaction().await?;
        let row = sqlx::query(
            "UPDATE bundle_claim SET \
                 expires_at = ?, \
                 metadata = COALESCE(?, metadata), \
                 updated_at = ? \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ? \
               AND state = 'active' \
               AND (expires_at IS NULL OR expires_at > ?) \
             RETURNING bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at",
        )
        .bind(expires_at)
        .bind(metadata_json)
        .bind(now_ms)
        .bind(&claim.bundle)
        .bind(&claim.claim_key)
        .bind(&claim.owner)
        .bind(epoch)
        .bind(now_ms)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(StoreError::StaleBundleClaim {
                bundle: claim.bundle.clone(),
                claim_key: claim.claim_key.clone(),
            });
        };
        let renewed = decode_claim(row)?;
        tx.commit().await?;
        Ok(renewed)
    }

    /// Release a lease matching `claim`'s exact `(bundle, claim_key, owner, epoch)`.
    ///
    /// Repeating a matching release is idempotent; a newer owner or bumped epoch
    /// returns [`StoreError::StaleBundleClaim`].
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] when fenced out, or SQLite errors.
    pub async fn release_bundle_claim(&self, claim: &BundleClaim) -> Result<(), StoreError> {
        self.release_bundle_claim_at(claim, now_millis()).await
    }

    /// Release a lease using an explicit `now_ms` timestamp.
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] when fenced out, or SQLite errors.
    pub async fn release_bundle_claim_at(
        &self,
        claim: &BundleClaim,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let mut tx = self.begin_transaction().await?;
        Self::release_bundle_claim_in_tx_at(&mut tx, claim, now_ms).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Release a lease inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] when fenced out, or SQLite errors.
    pub async fn release_bundle_claim_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        claim: &BundleClaim,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_claim_args(&claim.bundle, &claim.claim_key, &claim.owner)?;
        let epoch = epoch_to_i64(claim.epoch)?;
        let updated = sqlx::query(
            "UPDATE bundle_claim SET state = 'released', updated_at = ? \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ? AND state = 'active'",
        )
        .bind(now_ms)
        .bind(&claim.bundle)
        .bind(&claim.claim_key)
        .bind(&claim.owner)
        .bind(epoch)
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM bundle_claim \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ?",
        )
        .bind(&claim.bundle)
        .bind(&claim.claim_key)
        .bind(&claim.owner)
        .bind(epoch)
        .fetch_optional(&mut **tx)
        .await?;
        if state.as_deref() == Some("released") {
            return Ok(());
        }
        Err(StoreError::StaleBundleClaim {
            bundle: claim.bundle.clone(),
            claim_key: claim.claim_key.clone(),
        })
    }

    /// Transition all expired active leases (optionally scoped to `bundle`) to `released`.
    /// Returns the number of expired leases released.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn expire_bundle_claims(&self, bundle: Option<&str>) -> Result<u64, StoreError> {
        self.expire_bundle_claims_at(bundle, now_millis()).await
    }

    /// Transition all expired active leases at `now_ms` to `released`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn expire_bundle_claims_at(
        &self,
        bundle: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, StoreError> {
        if let Some(bundle) = bundle
            && bundle.trim().is_empty()
        {
            return Err(StoreError::BundleClaimData(
                "bundle identifier must not be empty".to_string(),
            ));
        }
        let mut tx = self.begin_transaction().await?;
        let res = match bundle {
            Some(bundle) => {
                sqlx::query(
                    "UPDATE bundle_claim SET state = 'released', updated_at = ? \
                     WHERE bundle = ? AND state = 'active' \
                       AND expires_at IS NOT NULL AND expires_at <= ?",
                )
                .bind(now_ms)
                .bind(bundle)
                .bind(now_ms)
                .execute(&mut *tx)
                .await?
            }
            None => {
                sqlx::query(
                    "UPDATE bundle_claim SET state = 'released', updated_at = ? \
                     WHERE state = 'active' AND expires_at IS NOT NULL AND expires_at <= ?",
                )
                .bind(now_ms)
                .bind(now_ms)
                .execute(&mut *tx)
                .await?
            }
        };
        tx.commit().await?;
        Ok(res.rows_affected())
    }

    /// Take over an active claim under runtime ownership (startup crash recovery),
    /// atomically bumping its epoch and returning `(new_claim, previous_epoch)`.
    ///
    /// # Errors
    /// Returns [`StoreError::RuntimeOwnerClaimRequired`] if this store does not hold
    /// `owner_run_id`, [`StoreError::BundleClaimUnavailable`] if no active row exists,
    /// or SQLite errors.
    pub async fn takeover_bundle_claim(
        &self,
        bundle: &str,
        claim_key: &str,
        new_owner: &str,
        owner_run_id: OwnerRunId,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<(BundleClaim, u64), StoreError> {
        self.require_runtime_owner(owner_run_id)?;
        let mut tx = self.begin_transaction().await?;
        let res = Self::takeover_bundle_claim_in_tx_at(
            &mut tx,
            bundle,
            claim_key,
            new_owner,
            ttl_ms,
            metadata,
            now_millis(),
        )
        .await?;
        tx.commit().await?;
        Ok(res)
    }

    /// Take over an active claim inside an open [`StoreTransaction`] (caller must hold runtime ownership).
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimUnavailable`] if no active row exists,
    /// [`StoreError::BundleClaimData`] for invalid inputs, or SQLite errors.
    pub async fn takeover_bundle_claim_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        claim_key: &str,
        new_owner: &str,
        ttl_ms: Option<u64>,
        metadata: Option<&serde_json::Value>,
        now_ms: i64,
    ) -> Result<(BundleClaim, u64), StoreError> {
        validate_claim_args(bundle, claim_key, new_owner)?;
        let expires_at = compute_expires_at(now_ms, ttl_ms);
        let metadata_json = metadata.map(serde_json::to_string).transpose()?;
        let row = sqlx::query(
            "UPDATE bundle_claim SET \
                 owner = ?, \
                 epoch = epoch + 1, \
                 expires_at = ?, \
                 metadata = COALESCE(?, metadata), \
                 updated_at = ? \
             WHERE bundle = ? AND claim_key = ? AND state = 'active' \
               AND epoch < 9223372036854775807 \
             RETURNING bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at",
        )
        .bind(new_owner)
        .bind(expires_at)
        .bind(metadata_json)
        .bind(now_ms)
        .bind(bundle)
        .bind(claim_key)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Err(StoreError::BundleClaimUnavailable {
                bundle: bundle.to_owned(),
                claim_key: claim_key.to_owned(),
            });
        };
        let claim = decode_claim(row)?;
        let previous_epoch = claim.epoch.saturating_sub(1);
        Ok((claim, previous_epoch))
    }

    /// Point-check whether `claim` is still active, un-expired, and owned by `(owner, epoch)`.
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] if stale or expired, or SQLite errors.
    pub async fn validate_bundle_claim(&self, claim: &BundleClaim) -> Result<(), StoreError> {
        self.validate_bundle_claim_at(claim, now_millis()).await
    }

    /// Point-check `claim` against `now_ms`.
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] if stale or expired, or SQLite errors.
    pub async fn validate_bundle_claim_at(
        &self,
        claim: &BundleClaim,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_claim_args(&claim.bundle, &claim.claim_key, &claim.owner)?;
        let epoch = epoch_to_i64(claim.epoch)?;
        let matched: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM bundle_claim \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ? \
               AND state = 'active' AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(&claim.bundle)
        .bind(&claim.claim_key)
        .bind(&claim.owner)
        .bind(epoch)
        .bind(now_ms)
        .fetch_optional(self.read_pool_checked()?)
        .await?;
        if matched.is_some() {
            Ok(())
        } else {
            Err(StoreError::StaleBundleClaim {
                bundle: claim.bundle.clone(),
                claim_key: claim.claim_key.clone(),
            })
        }
    }

    /// Fence `claim` inside an open [`StoreTransaction`] (acquiring SQLite's writer lock).
    ///
    /// # Errors
    /// Returns [`StoreError::StaleBundleClaim`] if stale or expired, or SQLite errors.
    pub async fn fence_bundle_claim_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        claim: &BundleClaim,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_claim_args(&claim.bundle, &claim.claim_key, &claim.owner)?;
        let epoch = epoch_to_i64(claim.epoch)?;
        let current = sqlx::query(
            "UPDATE bundle_claim SET state = state \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ? \
               AND state = 'active' AND (expires_at IS NULL OR expires_at > ?) \
             RETURNING claim_key",
        )
        .bind(&claim.bundle)
        .bind(&claim.claim_key)
        .bind(&claim.owner)
        .bind(epoch)
        .bind(now_ms)
        .fetch_optional(&mut **tx)
        .await?;
        if current.is_some() {
            Ok(())
        } else {
            Err(StoreError::StaleBundleClaim {
                bundle: claim.bundle.clone(),
                claim_key: claim.claim_key.clone(),
            })
        }
    }

    /// Fetch the active, non-expired claim for `(bundle, claim_key)`, if any.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_claim(
        &self,
        bundle: &str,
        claim_key: &str,
    ) -> Result<Option<BundleClaim>, StoreError> {
        self.get_bundle_claim_at(bundle, claim_key, now_millis())
            .await
    }

    /// Fetch the active, non-expired claim for `(bundle, claim_key)` at `now_ms`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_claim_at(
        &self,
        bundle: &str,
        claim_key: &str,
        now_ms: i64,
    ) -> Result<Option<BundleClaim>, StoreError> {
        validate_bundle_and_key(bundle, claim_key)?;
        let row = sqlx::query(
            "SELECT bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at \
             FROM bundle_claim \
             WHERE bundle = ? AND claim_key = ? AND state = 'active' \
               AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(bundle)
        .bind(claim_key)
        .bind(now_ms)
        .fetch_optional(self.read_pool_checked()?)
        .await?;
        row.map(decode_claim).transpose()
    }

    /// Fetch the active, non-expired claim for `(bundle, claim_key)` inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn get_bundle_claim_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        claim_key: &str,
        now_ms: i64,
    ) -> Result<Option<BundleClaim>, StoreError> {
        validate_bundle_and_key(bundle, claim_key)?;
        let row = sqlx::query(
            "SELECT bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at \
             FROM bundle_claim \
             WHERE bundle = ? AND claim_key = ? AND state = 'active' \
               AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(bundle)
        .bind(claim_key)
        .bind(now_ms)
        .fetch_optional(&mut **tx)
        .await?;
        row.map(decode_claim).transpose()
    }

    /// List all active, non-expired claims for `bundle`, ordered by `claim_key`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn list_active_bundle_claims(
        &self,
        bundle: &str,
    ) -> Result<Vec<BundleClaim>, StoreError> {
        self.list_active_bundle_claims_at(bundle, now_millis())
            .await
    }

    /// List all active, non-expired claims for `bundle` at `now_ms`.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn list_active_bundle_claims_at(
        &self,
        bundle: &str,
        now_ms: i64,
    ) -> Result<Vec<BundleClaim>, StoreError> {
        if bundle.trim().is_empty() {
            return Err(StoreError::BundleClaimData(
                "bundle identifier must not be empty".to_string(),
            ));
        }
        let rows = sqlx::query(
            "SELECT bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at \
             FROM bundle_claim \
             WHERE bundle = ? AND state = 'active' \
               AND (expires_at IS NULL OR expires_at > ?) \
             ORDER BY claim_key",
        )
        .bind(bundle)
        .bind(now_ms)
        .fetch_all(self.read_pool_checked()?)
        .await?;
        rows.into_iter().map(decode_claim).collect()
    }

    /// Return whether a specific `(bundle, claim_key, owner, epoch)` row is in the `released` state.
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn is_bundle_claim_released_in_tx(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        claim_key: &str,
        owner: &str,
        epoch: u64,
    ) -> Result<bool, StoreError> {
        validate_claim_args(bundle, claim_key, owner)?;
        let epoch = epoch_to_i64(epoch)?;
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM bundle_claim \
             WHERE bundle = ? AND claim_key = ? AND owner = ? AND epoch = ?",
        )
        .bind(bundle)
        .bind(claim_key)
        .bind(owner)
        .bind(epoch)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(state.as_deref() == Some("released"))
    }

    /// List all active, non-expired claims for `bundle` inside an open [`StoreTransaction`].
    ///
    /// # Errors
    /// Returns [`StoreError::BundleClaimData`] for invalid inputs or SQLite errors.
    pub async fn list_active_bundle_claims_in_tx_at(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        now_ms: i64,
    ) -> Result<Vec<BundleClaim>, StoreError> {
        if bundle.trim().is_empty() {
            return Err(StoreError::BundleClaimData(
                "bundle identifier must not be empty".to_string(),
            ));
        }
        let rows = sqlx::query(
            "SELECT bundle, claim_key, owner, epoch, expires_at, metadata, acquired_at, updated_at \
             FROM bundle_claim \
             WHERE bundle = ? AND state = 'active' \
               AND (expires_at IS NULL OR expires_at > ?) \
             ORDER BY claim_key",
        )
        .bind(bundle)
        .bind(now_ms)
        .fetch_all(&mut **tx)
        .await?;
        rows.into_iter().map(decode_claim).collect()
    }
}

fn decode_claim(row: sqlx::sqlite::SqliteRow) -> Result<BundleClaim, StoreError> {
    let epoch_raw: i64 = row.try_get("epoch")?;
    let epoch = u64::try_from(epoch_raw)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| StoreError::BundleClaimData("invalid claim epoch".to_string()))?;
    let metadata_str: String = row.try_get("metadata")?;
    let metadata = serde_json::from_str(&metadata_str).map_err(|err| {
        StoreError::BundleClaimData(format!("invalid claim metadata JSON: {err}"))
    })?;
    Ok(BundleClaim {
        bundle: row.try_get("bundle")?,
        claim_key: row.try_get("claim_key")?,
        owner: row.try_get("owner")?,
        epoch,
        expires_at: row.try_get("expires_at")?,
        metadata,
        acquired_at: row.try_get("acquired_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
