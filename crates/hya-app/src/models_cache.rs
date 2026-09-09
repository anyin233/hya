//! Durable provider model catalog cache beside `config.yaml`.
//!
//! Path: `$XDG_CONFIG_HOME/hya/models.yml.cache` (same directory as config).
//! Startup reads this file for empty-`models` providers; background discovery
//! rewrites it. Explicit `providers.*.models` in config always wins.
//!
//! Each cached model stores id plus limit/effort metadata so TUI context and
//! reasoning menus survive cold start without waiting on discovery HTTP.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use hya_provider::{Capabilities, ModelCatalogSource, ProviderModel, ReasoningEffort};
use serde::{Deserialize, Serialize};

use crate::config::expected_config_path;

/// On-disk shape of `models.yml.cache`.
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
    /// Build a cache row from a live catalog model.
    #[must_use]
    pub fn from_provider_model(model: &ProviderModel) -> Self {
        Self {
            id: model.model_id.clone(),
            limit: CachedModelLimit {
                context: model.capabilities.max_context,
                output: model.capabilities.max_output,
            },
            reasoning_default: model
                .reasoning_default
                .map(ReasoningEffort::as_str)
                .map(str::to_string),
            reasoning_variants: model.reasoning_variants.clone(),
            tools: model.capabilities.streaming_tool_calls,
        }
    }

    /// Convert this cache row into a catalog model for `provider_id`.
    #[must_use]
    pub fn to_provider_model(&self, provider_id: &str) -> ProviderModel {
        let mut capabilities = Capabilities {
            streaming_tool_calls: self.tools,
            parallel_tool_calls: self.tools,
            usage_reporting: true,
            reasoning_request: !self.reasoning_variants.is_empty()
                || self.reasoning_default.is_some(),
            max_context: self.limit.context,
            max_output: self.limit.output,
            ..Capabilities::default()
        };
        if capabilities.max_context == 0 {
            capabilities.max_context = 200_000;
        }
        ProviderModel {
            provider_id: provider_id.to_string(),
            model_id: self.id.trim().to_string(),
            capabilities,
            reasoning_variants: self
                .reasoning_variants
                .iter()
                .map(|variant| variant.trim().to_string())
                .filter(|variant| !variant.is_empty())
                .collect(),
            reasoning_default: self
                .reasoning_default
                .as_deref()
                .and_then(ReasoningEffort::parse),
            source: ModelCatalogSource::Discovered,
        }
    }
}

/// Resolve the cache path next to the active/expected Hya config file.
#[must_use]
pub fn models_cache_path() -> PathBuf {
    expected_config_path().with_file_name("models.yml.cache")
}

/// Read and parse `models.yml.cache`. Missing or empty files yield an empty map.
///
/// # Errors
/// Returns I/O or YAML parse failures for a present but unreadable file.
pub fn read_models_cache() -> anyhow::Result<ModelsCacheFile> {
    let path = models_cache_path();
    if !path.exists() {
        return Ok(ModelsCacheFile::default());
    }
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if yaml.trim().is_empty() {
        return Ok(ModelsCacheFile::default());
    }
    let parsed: ModelsCacheFile =
        serde_norway::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;
    Ok(normalize_cache(parsed))
}

/// Write a full cache document.
///
/// # Errors
/// Returns I/O or serialization failures.
pub fn write_models_cache_file(file: &ModelsCacheFile) -> anyhow::Result<()> {
    let path = models_cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut normalized = normalize_cache(file.clone());
    if normalized.version == 0 {
        normalized.version = 1;
    }
    let yaml = serde_norway::to_string(&normalized).context("serialize models.yml.cache")?;
    let tmp = path.with_extension("cache.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Replace one provider's cached models from live catalog rows.
pub fn upsert_provider_models(
    file: &mut ModelsCacheFile,
    provider_id: &str,
    models: &[ProviderModel],
) {
    let entries = models
        .iter()
        .filter(|model| model.provider_id == provider_id)
        .map(CachedModelEntry::from_provider_model)
        .filter(|entry| !entry.id.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        file.providers.remove(provider_id);
    } else {
        file.providers.insert(provider_id.to_string(), entries);
    }
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
