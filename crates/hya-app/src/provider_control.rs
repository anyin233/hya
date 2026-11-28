//! Live provider management behind the v1 provider routes: saved keys
//! (`auth/<id>.yaml`), provider declarations and model overrides
//! (`config.yaml`), and remote model refresh (the model cache).
//!
//! Every mutation re-reads `config.yaml` and the provider's credential,
//! rebuilds that provider's route and catalog rows (model cache ∪ config
//! entries), splices them into the engine's current router/catalog, and
//! publishes the result on the engine before returning — no restart. One
//! async mutex serializes mutations and the startup background refresh so
//! concurrent rebuilds never lose each other's routes.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use hya_engine::SessionEngine;
use hya_provider::ProviderKinds;
use hya_server::{
    PROVIDER_CONTROL_FAILURE, PROVIDER_INVALID_REQUEST, PROVIDER_NOT_FOUND, ProviderChange,
    ProviderControl, ProviderControlError, ProviderControlFuture, ProviderDiscoveryReport,
    ProviderKeySource, ProviderModelOverride, ProviderSettings, ProviderUpsert,
};

use crate::config::{self, ConfigEditError, DiscoverMode, PendingCatalogDiscovery};

/// App-owned [`ProviderControl`] over one shared engine.
#[derive(Clone)]
pub struct ProviderManager {
    engine: Arc<SessionEngine>,
    kinds: Arc<ProviderKinds>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

fn failure(error: impl std::fmt::Display) -> ProviderControlError {
    ProviderControlError::new(PROVIDER_CONTROL_FAILURE, format!("{error:#}"))
}

fn invalid(message: impl Into<String>) -> ProviderControlError {
    ProviderControlError::new(PROVIDER_INVALID_REQUEST, message)
}

fn edit_error(error: ConfigEditError) -> ProviderControlError {
    match error {
        ConfigEditError::Invalid(message) => invalid(message),
        ConfigEditError::NotFound(message) => {
            ProviderControlError::new(PROVIDER_NOT_FOUND, message)
        }
        ConfigEditError::Io(error) => failure(error),
    }
}

/// How a provider upsert changes its separately saved credential.
pub enum SavedProviderKey<'a> {
    /// Leave the current credential in place.
    Keep,
    /// Save this API key, replacing any saved credential.
    Set(&'a str),
    /// Remove the saved credential, if any.
    Remove,
}

/// Persist a provider declaration, its saved credential, and any preflight
/// discovery result. A live server upsert passes no preflight result and
/// discovers after saving. The CLI can reuse the result shown before saving.
/// `kind` must be one of `kinds`.
///
/// # Errors
/// Returns config edit or credential file failures. If a credential update
/// fails, the earlier config edit has already been written.
pub async fn persist_provider_upsert(
    config_path: &Path,
    id: &str,
    kind: &str,
    base_url: &str,
    key: SavedProviderKey<'_>,
    discovery: Option<&config::DiscoveryResult>,
    kinds: &ProviderKinds,
) -> Result<(), ConfigEditError> {
    if !hya_provider::valid_provider_id(id) || id == "hya" {
        return Err(ConfigEditError::Invalid(format!(
            "invalid provider id `{id}`"
        )));
    }
    if kinds.protocol(kind).is_none() {
        return Err(ConfigEditError::Invalid(config::unknown_kind(kind, kinds)));
    }
    config::validate_base_url(base_url).map_err(ConfigEditError::Invalid)?;
    config::upsert_provider_entry(config_path, id, kind, base_url)?;
    match key {
        SavedProviderKey::Keep => {}
        SavedProviderKey::Set(key) => {
            crate::auth::save_token(id, key).with_context(|| format!("save the key of `{id}`"))?;
        }
        SavedProviderKey::Remove => {
            crate::auth::remove_token(id)
                .with_context(|| format!("remove the old key of `{id}`"))?;
        }
    }
    if let Some(discovery) = discovery {
        config::store_discovery(id, discovery).await;
    }
    Ok(())
}

fn report(discovery: &config::DiscoveryResult) -> ProviderDiscoveryReport {
    ProviderDiscoveryReport {
        ok: discovery.ok(),
        result: discovery.label().to_string(),
        error_message: discovery.error_message().map(str::to_string),
        model_count: discovery.model_count(),
    }
}

impl ProviderManager {
    /// Manage providers on `engine`, resolving kinds against `kinds`.
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>, kinds: Arc<ProviderKinds>) -> Self {
        Self {
            engine,
            kinds,
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Run the startup background refresh for `pending` providers under the
    /// mutation lock and publish the result. Returns whether anything was
    /// published.
    ///
    /// # Errors
    /// Returns route-build failures.
    pub async fn refresh_pending(
        &self,
        pending: Vec<PendingCatalogDiscovery>,
    ) -> anyhow::Result<bool> {
        if pending.is_empty() {
            return Ok(false);
        }
        let _guard = self.lock.lock().await;
        let snapshot = self.engine.provider_snapshot();
        let (router, catalog) =
            config::refresh_pending_catalogs(pending, snapshot.catalog(), snapshot.router())
                .await?;
        self.engine
            .publish_provider_catalog(Arc::new(router), catalog);
        Ok(true)
    }

    /// Rebuild one provider from current config/credential/cache and publish.
    async fn rebuild(
        &self,
        provider_id: &str,
        mode: DiscoverMode,
    ) -> Result<ProviderChange, ProviderControlError> {
        let snapshot = self.engine.provider_snapshot();
        let ids = BTreeSet::from([provider_id.to_string()]);
        let rebuilt = config::rebuild_providers(
            Some(&ids),
            mode,
            snapshot.catalog(),
            snapshot.router(),
            &self.kinds,
        )
        .await
        .map_err(failure)?;
        self.engine
            .publish_provider_catalog(Arc::new(rebuilt.router), rebuilt.catalog);
        Ok(ProviderChange {
            configured: rebuilt.configured.contains(provider_id),
            discovery: rebuilt.discovery.get(provider_id).map(report),
        })
    }

    fn require_configured(provider_id: &str) -> Result<(), ProviderControlError> {
        let declared = config::provider_declarations().map_err(failure)?;
        if declared.iter().any(|provider| provider.id == provider_id) {
            Ok(())
        } else {
            Err(ProviderControlError::new(
                PROVIDER_NOT_FOUND,
                format!("provider not configured: {provider_id}"),
            ))
        }
    }

    fn key_source(
        provider_id: &str,
        inline_api_key: bool,
    ) -> Result<ProviderKeySource, ProviderControlError> {
        Ok(
            match crate::auth::load_credential(provider_id).map_err(failure)? {
                Some(credential) if credential.oauth().is_some() => ProviderKeySource::Oauth,
                Some(_) => ProviderKeySource::Saved,
                None if inline_api_key => ProviderKeySource::Config,
                None => ProviderKeySource::None,
            },
        )
    }
}

impl ProviderControl for ProviderManager {
    fn available(&self) -> bool {
        true
    }

    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { crate::auth::list_tokens().map_err(failure) })
    }

    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async {
            let declared = config::provider_declarations().map_err(failure)?;
            declared
                .into_iter()
                .map(|provider| {
                    Ok(ProviderSettings {
                        key_source: Self::key_source(&provider.id, provider.inline_api_key)?,
                        id: provider.id,
                        kind: provider.kind,
                        base_url: provider.base_url,
                    })
                })
                .collect()
        })
    }

    fn set_key(
        &self,
        provider_id: String,
        key: String,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            crate::auth::save_token(&provider_id, &key).map_err(failure)?;
            self.rebuild(&provider_id, DiscoverMode::IfUncached).await
        })
    }

    fn remove_key(&self, provider_id: String) -> ProviderControlFuture<'_, (bool, ProviderChange)> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            let removed = crate::auth::remove_token(&provider_id).map_err(failure)?;
            let change = self.rebuild(&provider_id, DiscoverMode::Never).await?;
            Ok((removed, change))
        })
    }

    fn upsert_provider(
        &self,
        request: ProviderUpsert,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            persist_provider_upsert(
                &config::active_config_path(),
                &request.id,
                &request.kind,
                &request.base_url,
                request
                    .api_key
                    .as_deref()
                    .map_or(SavedProviderKey::Keep, SavedProviderKey::Set),
                None,
                &self.kinds,
            )
            .await
            .map_err(edit_error)?;
            self.rebuild(&request.id, DiscoverMode::Always).await
        })
    }

    fn refresh_provider(&self, provider_id: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            Self::require_configured(&provider_id)?;
            self.rebuild(&provider_id, DiscoverMode::Always).await
        })
    }

    fn set_model(
        &self,
        provider_id: String,
        model_id: String,
        metadata: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            config::set_model_entry(
                &config::active_config_path(),
                &provider_id,
                &model_id,
                &config::ModelEntryOverride {
                    display_name: metadata.display_name,
                    context_limit: metadata.context_limit,
                    output_limit: metadata.output_limit,
                    reasoning: metadata.reasoning,
                },
            )
            .map_err(edit_error)?;
            self.rebuild(&provider_id, DiscoverMode::Never).await
        })
    }

    fn remove_model(
        &self,
        provider_id: String,
        model_id: String,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let _guard = self.lock.lock().await;
            let removed =
                config::remove_model_entry(&config::active_config_path(), &provider_id, &model_id)
                    .map_err(edit_error)?;
            if !removed {
                return Err(ProviderControlError::new(
                    PROVIDER_NOT_FOUND,
                    format!("no config entry for model {model_id} on provider {provider_id}"),
                ));
            }
            self.rebuild(&provider_id, DiscoverMode::Never).await
        })
    }
}
