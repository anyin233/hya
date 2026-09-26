//! Legacy YAML model cache (`models.yml.cache` beside `config.yaml`).
//!
//! Superseded by the SQLite [`crate::model_cache`] database. This module only
//! reads the old file so [`crate::model_cache::ModelCache`] can import it
//! once; nothing writes it any more.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::config::expected_config_path;

/// On-disk shape of the legacy `models.yml.cache`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelsCacheFile {
    /// Schema version for forward-compatible cache upgrades.
    #[serde(default = "cache_version_default")]
    pub version: u32,
    /// Provider id → ordered rich model entries from the last successful refresh.
    #[serde(default)]
    pub providers: BTreeMap<String, Vec<CachedModelEntry>>,
}

fn cache_version_default() -> u32 {
    1
}

/// One cached model row with limit and reasoning metadata.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CachedModelEntry {
    /// Upstream model id.
    pub id: String,
    /// Context / max-output token limits.
    #[serde(default)]
    pub limit: CachedModelLimit,
    /// Default reasoning effort label (`none`, `medium`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_default: Option<String>,
    /// Advertised reasoning effort variants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_variants: Vec<String>,
    /// Whether the model advertises tool calling.
    #[serde(default = "default_tools")]
    pub tools: bool,
}

fn default_tools() -> bool {
    true
}

/// Token limits persisted for one cached model.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CachedModelLimit {
    /// Advertised context window in tokens.
    #[serde(default)]
    pub context: u32,
    /// Advertised max output tokens.
    #[serde(default)]
    pub output: u32,
}

impl CachedModelEntry {
    /// Convert this legacy row into a [`crate::model_cache::CachedModel`].
    #[must_use]
    pub fn to_cached_model(&self, fetched_at_ms: i64) -> crate::model_cache::CachedModel {
        crate::model_cache::CachedModel {
            id: self.id.trim().to_string(),
            display_name: None,
            context_limit: self.limit.context,
            output_limit: self.limit.output,
            reasoning_default: self.reasoning_default.clone(),
            reasoning_variants: self.reasoning_variants.clone(),
            tools: self.tools,
            fetched_at_ms,
        }
    }
}

/// Legacy cache path next to the active/expected Hya config file.
#[must_use]
pub fn legacy_models_cache_path() -> PathBuf {
    expected_config_path().with_file_name("models.yml.cache")
}

/// Read and parse a legacy `models.yml.cache`. Missing or empty files yield
/// an empty document.
///
/// # Errors
/// Returns I/O or YAML parse failures for a present but unreadable file.
pub fn read_legacy_models_cache(path: &std::path::Path) -> anyhow::Result<ModelsCacheFile> {
    if !path.exists() {
        return Ok(ModelsCacheFile::default());
    }
    let yaml = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if yaml.trim().is_empty() {
        return Ok(ModelsCacheFile::default());
    }
    let parsed: ModelsCacheFile =
        serde_norway::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;
    Ok(normalize_cache(parsed))
}

fn normalize_cache(mut file: ModelsCacheFile) -> ModelsCacheFile {
    if file.version == 0 {
        file.version = 1;
    }
    let mut normalized = BTreeMap::new();
    for (provider_id, models) in std::mem::take(&mut file.providers) {
        let cleaned = models
            .into_iter()
            .map(|mut entry| {
                entry.id = entry.id.trim().to_string();
                entry.reasoning_variants = entry
                    .reasoning_variants
                    .into_iter()
                    .map(|variant| variant.trim().to_string())
                    .filter(|variant| !variant.is_empty())
                    .collect();
                if let Some(default) = entry.reasoning_default.as_mut() {
                    *default = default.trim().to_string();
                    if default.is_empty() {
                        entry.reasoning_default = None;
                    }
                }
                entry
            })
            .filter(|entry| !entry.id.is_empty())
            .collect::<Vec<_>>();
        if !cleaned.is_empty() {
            normalized.insert(provider_id, cleaned);
        }
    }
    file.providers = normalized;
    file
}
