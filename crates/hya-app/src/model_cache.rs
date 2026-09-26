//! Durable cache of each provider's remote model list: a dedicated SQLite
//! database at `$XDG_CACHE_HOME/hya/model_cache.db` (fallback
//! `~/.cache/hya/model_cache.db`).
//!
//! One row per `(provider_id, model_id)` holds what the provider's remote
//! `/models` list published: display name, context/output limits, reasoning
//! default and variants, tool support, and when it was fetched. Startup reads
//! the cache so no provider waits on discovery HTTP when rows exist; a
//! refresh (startup background discovery, `hya models --refresh`, or the
//! v1 `RefreshProvider`/`UpsertProvider` routes) replaces one provider's rows.
//! The effective model list merges these rows with the provider's config
//! `models:` entries (see [`crate::config`]).
//!
//! The cache replaces the YAML `models.yml.cache` beside `config.yaml`: the
//! first open of an empty database imports that file once, and nothing writes
//! it any more. This is a pure cache — deleting the file only costs one
//! discovery request per provider.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::time::Duration;

use anyhow::Context as _;
use hya_provider::DiscoveredModel;
use sqlx::Row as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// Meta key recording that the legacy YAML import already ran.
const LEGACY_IMPORT_KEY: &str = "legacy_models_yml_imported";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cache_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS provider_models (
    provider_id        TEXT    NOT NULL,
    model_id           TEXT    NOT NULL,
    position           INTEGER NOT NULL,
    display_name       TEXT,
    context_limit      INTEGER NOT NULL DEFAULT 0,
    output_limit       INTEGER NOT NULL DEFAULT 0,
    reasoning_default  TEXT,
    reasoning_variants TEXT    NOT NULL DEFAULT '[]',
    tools              INTEGER NOT NULL DEFAULT 1,
    fetched_at_ms      INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);
";

/// One cached remote model row.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CachedModel {
    /// Upstream model id.
    pub id: String,
    /// Display name the remote list published.
    pub display_name: Option<String>,
    /// Advertised context window in tokens (`0` = unknown).
    pub context_limit: u32,
    /// Advertised max output tokens (`0` = unknown).
    pub output_limit: u32,
    /// Default reasoning effort label the remote list published.
    pub reasoning_default: Option<String>,
    /// Advertised reasoning effort labels (empty = none published).
    pub reasoning_variants: Vec<String>,
    /// Whether the model advertises tool calling.
    pub tools: bool,
    /// Unix milliseconds of the fetch that produced this row.
    pub fetched_at_ms: i64,
}

impl CachedModel {
    /// Build a cache row from one discovered remote model.
    #[must_use]
    pub fn from_discovered(model: &DiscoveredModel, fetched_at_ms: i64) -> Self {
        Self {
            id: model.id.trim().to_string(),
            display_name: model
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            context_limit: model.context_limit.unwrap_or(0),
            output_limit: model.output_limit.unwrap_or(0),
            reasoning_default: model
                .reasoning_default
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            reasoning_variants: model
                .reasoning_variants
                .iter()
                .map(|variant| variant.trim().to_string())
                .filter(|variant| !variant.is_empty())
                .collect(),
            tools: true,
            fetched_at_ms,
        }
    }
}

/// Current time in unix milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// `$XDG_CACHE_HOME/hya/model_cache.db`, else `$HOME/.cache/hya/model_cache.db`;
/// `None` when neither variable is set.
#[must_use]
pub fn model_cache_path() -> Option<PathBuf> {
    hya_store::user_cache_dir().map(|dir| dir.join("model_cache.db"))
}

/// An open model cache database.
pub struct ModelCache {
    pool: sqlx::SqlitePool,
}

impl ModelCache {
    /// Open (creating when missing) the cache at [`model_cache_path`] and run
    /// the one-time `models.yml.cache` import.
    ///
    /// # Errors
    /// Returns when no cache location is known or the database cannot be
    /// opened.
    pub async fn open_default() -> anyhow::Result<Self> {
        let path =
            model_cache_path().context("no model cache location (set XDG_CACHE_HOME or HOME)")?;
        let cache = Self::open(&path).await?;
        let legacy = crate::models_cache::legacy_models_cache_path();
        if let Err(error) = cache.import_legacy_once(&legacy).await {
            tracing::warn!(%error, "model cache: legacy models.yml.cache import failed");
        }
        Ok(cache)
    }

    /// Open (creating when missing) a cache database at `path`.
    ///
    /// # Errors
    /// Returns directory, connection, or schema failures.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .with_context(|| format!("model cache path {}", path.display()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("open model cache {}", path.display()))?;
        sqlx::raw_sql(SCHEMA)
            .execute(&pool)
            .await
            .context("create model cache schema")?;
        Ok(Self { pool })
    }

    /// Close the connection pool.
    pub async fn close(self) {
        self.pool.close().await;
    }

    /// Cached rows for one provider in remote-list order.
    ///
    /// # Errors
    /// Returns query or decode failures.
    pub async fn provider_models(&self, provider_id: &str) -> anyhow::Result<Vec<CachedModel>> {
        let rows = sqlx::query(
            "SELECT model_id, display_name, context_limit, output_limit, reasoning_default,
                    reasoning_variants, tools, fetched_at_ms
             FROM provider_models WHERE provider_id = ?1 ORDER BY position, model_id",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
        .context("read model cache")?;
        rows.iter().map(decode_row).collect()
    }

    /// Every cached provider's rows, keyed by provider id.
    ///
    /// # Errors
    /// Returns query or decode failures.
    pub async fn all(&self) -> anyhow::Result<BTreeMap<String, Vec<CachedModel>>> {
        let rows = sqlx::query(
            "SELECT provider_id, model_id, display_name, context_limit, output_limit,
                    reasoning_default, reasoning_variants, tools, fetched_at_ms
             FROM provider_models ORDER BY provider_id, position, model_id",
        )
        .fetch_all(&self.pool)
        .await
        .context("read model cache")?;
        let mut out: BTreeMap<String, Vec<CachedModel>> = BTreeMap::new();
        for row in &rows {
            let provider: String = row.try_get("provider_id")?;
            out.entry(provider).or_default().push(decode_row(row)?);
        }
        Ok(out)
    }

    /// Replace one provider's rows (an empty slice deletes them).
    ///
    /// # Errors
    /// Returns transaction failures.
    pub async fn replace_provider(
        &self,
        provider_id: &str,
        models: &[CachedModel],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await.context("begin model cache write")?;
        sqlx::query("DELETE FROM provider_models WHERE provider_id = ?1")
            .bind(provider_id)
            .execute(&mut *tx)
            .await
            .context("clear provider models")?;
        let mut seen = std::collections::BTreeSet::new();
        for (position, model) in models.iter().enumerate() {
            let id = model.id.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }
            sqlx::query(
                "INSERT INTO provider_models (provider_id, model_id, position, display_name,
                    context_limit, output_limit, reasoning_default, reasoning_variants, tools,
                    fetched_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )
            .bind(provider_id)
            .bind(id)
            .bind(i64::try_from(position).unwrap_or(i64::MAX))
            .bind(model.display_name.as_deref())
            .bind(i64::from(model.context_limit))
            .bind(i64::from(model.output_limit))
            .bind(model.reasoning_default.as_deref())
            .bind(serde_json::to_string(&model.reasoning_variants).context("encode variants")?)
            .bind(model.tools)
            .bind(model.fetched_at_ms)
            .execute(&mut *tx)
            .await
            .context("insert provider model")?;
        }
        tx.commit().await.context("commit model cache write")?;
        Ok(())
    }

    /// Import `models.yml.cache` once: only when the database has no rows
    /// and the import never ran; afterwards the YAML file is ignored.
    /// Returns whether rows were imported.
    ///
    /// # Errors
    /// Returns query or write failures (a missing or unparsable YAML file is
    /// not an error; it is recorded as imported).
    pub async fn import_legacy_once(&self, legacy_path: &Path) -> anyhow::Result<bool> {
        let done = sqlx::query("SELECT value FROM cache_meta WHERE key = ?1")
            .bind(LEGACY_IMPORT_KEY)
            .fetch_optional(&self.pool)
            .await
            .context("read model cache meta")?
            .is_some();
        if done {
            return Ok(false);
        }
        let has_rows = sqlx::query("SELECT 1 FROM provider_models LIMIT 1")
            .fetch_optional(&self.pool)
            .await
            .context("probe model cache")?
            .is_some();
        let mut imported = false;
        if !has_rows && legacy_path.is_file() {
            let fetched_at_ms = std::fs::metadata(legacy_path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or_else(now_ms, |duration| {
                    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
                });
            match crate::models_cache::read_legacy_models_cache(legacy_path) {
                Ok(file) => {
                    for (provider_id, entries) in &file.providers {
                        let rows = entries
                            .iter()
                            .map(|entry| entry.to_cached_model(fetched_at_ms))
                            .collect::<Vec<_>>();
                        self.replace_provider(provider_id, &rows).await?;
                        imported |= !rows.is_empty();
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "model cache: skipping unreadable models.yml.cache");
                }
            }
        }
        sqlx::query("INSERT OR REPLACE INTO cache_meta (key, value) VALUES (?1, ?2)")
            .bind(LEGACY_IMPORT_KEY)
            .bind(now_ms().to_string())
            .execute(&self.pool)
            .await
            .context("record legacy import")?;
        Ok(imported)
    }
}

fn decode_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<CachedModel> {
    let variants: String = row.try_get("reasoning_variants")?;
    Ok(CachedModel {
        id: row.try_get("model_id")?,
        display_name: row.try_get("display_name")?,
        context_limit: u32::try_from(row.try_get::<i64, _>("context_limit")?).unwrap_or(0),
        output_limit: u32::try_from(row.try_get::<i64, _>("output_limit")?).unwrap_or(0),
        reasoning_default: row.try_get("reasoning_default")?,
        reasoning_variants: serde_json::from_str(&variants).unwrap_or_default(),
        tools: row.try_get("tools")?,
        fetched_at_ms: row.try_get("fetched_at_ms")?,
    })
}

/// Read every cached provider's rows from the default cache; any failure
/// (no location, unreadable database) degrades to an empty cache.
pub async fn read_all_or_empty() -> BTreeMap<String, Vec<CachedModel>> {
    match ModelCache::open_default().await {
        Ok(cache) => {
            let rows = cache.all().await.unwrap_or_else(|error| {
                tracing::warn!(%error, "model cache: read failed");
                BTreeMap::new()
            });
            cache.close().await;
            rows
        }
        Err(error) => {
            tracing::warn!(%error, "model cache: unavailable");
            BTreeMap::new()
        }
    }
}

/// Replace one provider's rows in the default cache, logging failures.
pub async fn store_provider_or_warn(provider_id: &str, models: &[CachedModel]) {
    match ModelCache::open_default().await {
        Ok(cache) => {
            if let Err(error) = cache.replace_provider(provider_id, models).await {
                tracing::warn!(%error, provider_id, "model cache: write failed");
            }
            cache.close().await;
        }
        Err(error) => tracing::warn!(%error, "model cache: unavailable"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "hya-model-cache-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn replace_and_read_round_trip_metadata_in_remote_order() {
        let dir = temp_path("roundtrip");
        let cache = ModelCache::open(&dir.join("model_cache.db")).await.unwrap();
        let rows = vec![
            CachedModel {
                id: "zeta".into(),
                display_name: Some("Zeta".into()),
                context_limit: 128_000,
                output_limit: 8_192,
                reasoning_default: Some("high".into()),
                reasoning_variants: vec!["low".into(), "high".into()],
                tools: false,
                fetched_at_ms: 42,
            },
            CachedModel {
                id: "alpha".into(),
                tools: true,
                fetched_at_ms: 42,
                ..CachedModel::default()
            },
        ];
        cache.replace_provider("gw", &rows).await.unwrap();
        assert_eq!(cache.provider_models("gw").await.unwrap(), rows);
        assert_eq!(cache.all().await.unwrap().get("gw"), Some(&rows));

        cache.replace_provider("gw", &rows[1..]).await.unwrap();
        assert_eq!(cache.provider_models("gw").await.unwrap(), rows[1..]);
        cache.replace_provider("gw", &[]).await.unwrap();
        assert!(cache.provider_models("gw").await.unwrap().is_empty());
        cache.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn legacy_yaml_imports_once_into_an_empty_database() {
        let dir = temp_path("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("models.yml.cache");
        std::fs::write(
            &legacy,
            "version: 1\nproviders:\n  gw:\n    - id: old-model\n      limit:\n        context: 64000\n        output: 4096\n      reasoning_default: medium\n      reasoning_variants: [low, medium]\n",
        )
        .unwrap();
        let cache = ModelCache::open(&dir.join("model_cache.db")).await.unwrap();
        assert!(cache.import_legacy_once(&legacy).await.unwrap());
        let rows = cache.provider_models("gw").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "old-model");
        assert_eq!(rows[0].context_limit, 64_000);
        assert_eq!(rows[0].output_limit, 4_096);
        assert_eq!(rows[0].reasoning_default.as_deref(), Some("medium"));
        assert_eq!(rows[0].reasoning_variants, vec!["low", "medium"]);

        // Once only: a later refresh that clears the provider is not undone.
        cache.replace_provider("gw", &[]).await.unwrap();
        assert!(!cache.import_legacy_once(&legacy).await.unwrap());
        assert!(cache.provider_models("gw").await.unwrap().is_empty());
        cache.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
