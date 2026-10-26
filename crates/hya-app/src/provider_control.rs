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
use std::sync::Arc;

use hya_core::SessionEngine;
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
    lock: Arc<tokio::sync::Mutex<()>>,
    oauth: Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, crate::oauth::chatgpt::ChatgptLogin>>,
    >,
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

/// Validate an `http(s)://host…` base URL.
fn check_base_url(base_url: &str) -> Result<(), ProviderControlError> {
    config::validate_base_url(base_url).map_err(invalid)
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
    /// Manage providers on `engine`.
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>) -> Self {
        Self {
            engine,
            lock: Arc::new(tokio::sync::Mutex::new(())),
            oauth: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
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
        let snapshot = self.engine.provider_catalog_snapshot();
        let router = self.engine.provider_router();
        let (router, catalog) =
            config::refresh_pending_catalogs(pending, snapshot.as_ref(), router.as_ref()).await?;
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
        let snapshot = self.engine.provider_catalog_snapshot();
        let router = self.engine.provider_router();
        let ids = BTreeSet::from([provider_id.to_string()]);
        let rebuilt =
            config::rebuild_providers(Some(&ids), mode, snapshot.as_ref(), router.as_ref())
                .await
                .map_err(failure)?;
        self.engine
            .publish_provider_catalog(Arc::new(rebuilt.router), rebuilt.catalog);
        Ok(ProviderChange {
            warning: None,
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

    fn key_source(provider_id: &str, inline_api_key: bool) -> ProviderKeySource {
        match crate::auth::load_credential(provider_id) {
            Some(credential) if credential.oauth().is_some() => ProviderKeySource::Oauth,
            Some(_) => ProviderKeySource::Saved,
            None if inline_api_key => ProviderKeySource::Config,
            None => ProviderKeySource::None,
        }
    }
}

impl ProviderControl for ProviderManager {
    fn start_oauth(
        &self,
        provider_id: String,
    ) -> ProviderControlFuture<'_, hya_server::ProviderOauthStart> {
        Box::pin(async move {
            if !hya_server::valid_provider_id(&provider_id) {
                return Err(invalid("invalid provider id"));
            }
            let declarations = config::provider_declarations().map_err(failure)?;
            if declarations.iter().any(|p| {
                p.id == provider_id
                    && (p.kind != "openai-chatgpt" || p.base_url != crate::oauth::chatgpt::RESOURCE)
            }) {
                return Err(invalid(
                    "provider name already belongs to another route; use a new ChatGPT provider name",
                ));
            }
            let mut pending = self.oauth.lock().await;
            pending.retain(|_, p| p.expires > std::time::Instant::now());
            if pending.len() >= 16 {
                return Err(invalid("too many pending logins; wait for expiry"));
            }
            let dir = crate::auth::auth_dir().ok_or_else(|| failure("no auth directory"))?;
            let login = crate::oauth::chatgpt::ChatgptLogin::start(
                &dir,
                &provider_id,
                std::time::Duration::from_secs(600),
            )
            .await
            .map_err(failure)?;
            let result = hya_server::ProviderOauthStart {
                authorization_url: login.authorization_url.clone(),
                state: login.state.clone(),
            };
            pending.insert(provider_id, login);
            Ok(result)
        })
    }
    fn complete_oauth(
        &self,
        provider_id: String,
        state: String,
        callback: String,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            let login = {
                let mut pending = self.oauth.lock().await;
                if !pending
                    .get(&provider_id)
                    .is_some_and(|p| p.state == state && p.expires > std::time::Instant::now())
                {
                    return Err(invalid(
                        "missing, expired or mismatched ChatGPT login; start again",
                    ));
                }
                pending
                    .remove(&provider_id)
                    .ok_or_else(|| invalid("login already consumed"))?
            };
            let credential = login.finish(&callback).await.map_err(failure)?;
            let _guard = self.lock.lock().await;
            if config::provider_declarations()
                .map_err(failure)?
                .iter()
                .any(|p| {
                    p.id == provider_id
                        && (p.kind != "openai-chatgpt"
                            || p.base_url != crate::oauth::chatgpt::RESOURCE)
                })
            {
                return Err(invalid(
                    "provider configuration changed during ChatGPT sign-in; no credentials replaced",
                ));
            }
            let dir = crate::auth::auth_dir().ok_or_else(|| failure("no auth directory"))?;
            let _credential_guard = crate::oauth::chatgpt::lock_refresh(&dir, &provider_id)
                .await
                .map_err(failure)?;
            crate::auth::save_credential_in(
                &dir,
                &provider_id,
                &crate::auth::AuthCredential::OAuth(credential),
            )
            .map_err(failure)?;
            config::upsert_oauth_provider(
                &config::expected_config_path(),
                &provider_id,
                "openai-chatgpt",
                crate::oauth::chatgpt::RESOURCE,
            )
            .map_err(failure)?;
            self.rebuild(&provider_id, DiscoverMode::Always).await
        })
    }

    fn available(&self) -> bool {
        true
    }

    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { crate::auth::list_tokens().map_err(failure) })
    }

    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async {
            let declared = config::provider_declarations().map_err(failure)?;
            Ok(declared
                .into_iter()
                .map(|provider| ProviderSettings {
                    key_source: Self::key_source(&provider.id, provider.inline_api_key),
                    id: provider.id,
                    kind: provider.kind,
                    base_url: provider.base_url,
                })
                .collect())
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
            let dir = crate::auth::auth_dir().ok_or_else(|| failure("no auth directory"))?;
            let (removed, warning) = crate::oauth::chatgpt::logout_in(&dir, &provider_id)
                .await
                .map_err(failure)?;
            let mut change = self.rebuild(&provider_id, DiscoverMode::Never).await?;
            change.warning = warning;
            Ok((removed, change))
        })
    }

    fn upsert_provider(
        &self,
        request: ProviderUpsert,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async move {
            if !hya_server::valid_provider_id(&request.id) || request.id == "hya" {
                return Err(invalid(format!("invalid provider id `{}`", request.id)));
            }
            if !config::PROVIDER_KIND_LABELS.contains(&request.kind.as_str()) {
                return Err(invalid(format!(
                    "unknown provider kind `{}` (expected one of {})",
                    request.kind,
                    config::PROVIDER_KIND_LABELS.join(", ")
                )));
            }
            check_base_url(&request.base_url)?;
            if request.kind == "openai-chatgpt"
                && request.base_url != crate::oauth::chatgpt::RESOURCE
            {
                return Err(invalid(
                    "ChatGPT plan providers require https://api.openai.com/v1",
                ));
            }
            let _guard = self.lock.lock().await;
            config::upsert_provider_entry(
                &config::active_config_path(),
                &request.id,
                &request.kind,
                &request.base_url,
            )
            .map_err(edit_error)?;
            if let Some(key) = request.api_key.as_deref() {
                crate::auth::save_token(&request.id, key).map_err(failure)?;
            }
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
