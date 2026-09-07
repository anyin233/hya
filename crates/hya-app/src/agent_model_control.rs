//! Durable control for per-Agent base-model preferences.
//!
//! The control owns the boundary between the owner-fenced auxiliary store and
//! the immutable preference view captured by [`hya_core::TurnBinding`]. Core
//! remains the execution-policy owner; the control retains the same category
//! registry only to project the effective server/TUI default before root
//! Session creation.

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_core::{
    AgentModelConfiguration, AgentOrigin, CategoryRegistry, RuntimeRegistry, TurnBinding,
    resolve_configured_agent_model,
};
use hya_proto::{AgentName, ModelRef, OwnerRunId};
use hya_provider::ProviderRouter;
use hya_store::{AgentModelPreference, SessionStore, StoreError};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::agent_model_config::AgentModelConfigFiles;

/// Stable provider/model identity persisted for one catalog Agent.
///
/// Only the provider id and provider-local model id are retained. Credentials,
/// request options, reasoning variants, and provider responses never cross
/// this control boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentModelIdentity {
    /// Provider route id that owns the model.
    pub provider_id: String,
    /// Provider-local model id, including any model-local slashes.
    pub model_id: String,
}

impl AgentModelIdentity {
    /// Construct a stable provider/model identity.
    ///
    /// # Arguments
    ///
    /// * `provider` - Exact provider route id from the current catalog.
    /// * `model` - Exact provider-local model id from the current catalog.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_id: provider.into(),
            model_id: model.into(),
        }
    }

    /// Borrow the provider route id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider_id
    }

    /// Borrow the provider-local model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model_id
    }

    /// Build the canonical `provider/model` reference without reparsing the
    /// provider-local model id.
    #[must_use]
    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(format!("{}/{}", self.provider_id, self.model_id))
    }
}

/// Typed failures from the durable Agent model preference control.
#[derive(Debug, Error)]
pub enum AgentModelControlError {
    /// The requested stable Agent id is absent from the supplied binding.
    #[error("unknown Agent `{agent_id}`")]
    UnknownAgent {
        /// Stable Agent id supplied by the caller.
        agent_id: String,
    },
    /// The Agent has an explicit direct model or category policy.
    #[error("Agent `{agent_id}` has an explicit model policy")]
    ConfiguredAgent {
        /// Stable Agent id whose configured policy cannot be replaced.
        agent_id: String,
    },
    /// The requested provider/model pair is absent from the current router
    /// catalog.
    #[error("model `{provider_id}/{model_id}` is unavailable in the provider catalog")]
    ModelUnavailable {
        /// Provider route id supplied by the caller.
        provider_id: String,
        /// Provider-local model id supplied by the caller.
        model_id: String,
    },
    /// The durable preference store rejected or could not complete an
    /// operation.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Owning configuration could not be read or atomically updated.
    #[error("Agent model configuration failed: {0}")]
    Configuration(#[source] anyhow::Error),
}

/// Cloneable owner-fenced control for durable Agent model preferences.
///
/// Clones share one asynchronous mutation lock and the complete published map.
/// Each mutation therefore serializes validation, durable commit, map update,
/// and live snapshot publication, which keeps attached clients from publishing
/// partial maps. Publication after a successful commit is infallible.
#[derive(Clone)]
pub struct PersistentAgentModelControl {
    store: SessionStore,
    owner: OwnerRunId,
    runtime: Arc<RuntimeRegistry>,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    preferences: Arc<Mutex<BTreeMap<String, ModelRef>>>,
    configuration_files: Option<Arc<AgentModelConfigFiles>>,
    configuration: Arc<Mutex<AgentModelConfiguration>>,
}

impl PersistentAgentModelControl {
    /// Load all stored preferences, publish the complete immutable map, and
    /// return an owner-fenced control handle.
    ///
    /// Stored rows are not checked against the provider catalog here. Stale
    /// rows remain durable and are filtered by core execution when a binding
    /// resolves an Agent.
    ///
    /// # Arguments
    ///
    /// * `store` - Session store whose runtime owner has already been claimed.
    /// * `owner` - Runtime owner identity used for subsequent mutations.
    /// * `runtime` - Registry receiving the immutable preference snapshot.
    /// * `router` - Provider router used for exact mutation-time catalog checks.
    ///
    /// # Errors
    /// Returns [`AgentModelControlError::Store`] when the preference rows cannot
    /// be listed or decoded. In that case no live preference snapshot is
    /// published.
    pub async fn load(
        store: SessionStore,
        owner: OwnerRunId,
        runtime: Arc<RuntimeRegistry>,
        router: Arc<ProviderRouter>,
    ) -> Result<Self, AgentModelControlError> {
        let preferences = stored_preferences(&store).await?;
        runtime.publish_agent_model_preferences(preferences.clone());
        Ok(Self {
            store,
            owner,
            runtime,
            router,
            categories: Arc::new(CategoryRegistry::default()),
            preferences: Arc::new(Mutex::new(preferences)),
            configuration_files: None,
            configuration: Arc::new(Mutex::new(AgentModelConfiguration::default())),
        })
    }

    /// Install the runtime's model-category registry for root and state resolution.
    ///
    /// The registry is immutable for this runtime generation and resolves both
    /// new root Session defaults and the same effective state reported to the
    /// server/TUI. Durable preference eligibility remains unchanged.
    #[must_use]
    pub fn with_categories(mut self, categories: Arc<CategoryRegistry>) -> Self {
        self.categories = categories;
        self
    }

    /// Load user-owned model configuration before publishing runtime readiness.
    ///
    /// Pure preference controls remain filesystem-independent until this is called.
    /// Configuration read/parse failures leave the prior runtime snapshot intact.
    pub async fn with_configuration(
        mut self,
        files: AgentModelConfigFiles,
    ) -> Result<Self, AgentModelControlError> {
        let files = Arc::new(files);
        let reader = files.clone();
        let configuration = tokio::task::spawn_blocking(move || reader.load())
            .await
            .map_err(|error| AgentModelControlError::Configuration(error.into()))?
            .map_err(AgentModelControlError::Configuration)?;
        self.runtime
            .publish_agent_model_configuration(configuration.clone());
        *self.configuration.lock().await = configuration;
        self.configuration_files = Some(files);
        Ok(self)
    }

    fn configuration_path(
        &self,
        binding: &TurnBinding,
        agent_id: &str,
    ) -> Result<Option<String>, AgentModelControlError> {
        let Some(files) = &self.configuration_files else {
            return Ok(None);
        };
        let definition = binding.agent_catalog().resolve(agent_id).ok_or_else(|| {
            AgentModelControlError::UnknownAgent {
                agent_id: agent_id.to_string(),
            }
        })?;
        files
            .path_for(definition.origin)
            .map(|path| Some(path.to_string_lossy().into_owned()))
            .map_err(AgentModelControlError::Configuration)
    }

    /// Resolve one Agent's effective model from a captured runtime binding.
    ///
    /// Captured Session and user-file models override authored direct/category
    /// policy. An available remembered model wins over the process base only
    /// when none of those explicit model layers applies.
    ///
    /// # Errors
    /// Returns [`AgentModelControlError::UnknownAgent`] when `stable_id` does
    /// not exact-resolve in `binding`.
    pub fn effective_model(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        base_model: &ModelRef,
    ) -> Result<ModelRef, AgentModelControlError> {
        let definition = binding
            .resolve_agent(stable_id)
            .filter(|definition| definition.stable_id == stable_id)
            .ok_or_else(|| AgentModelControlError::UnknownAgent {
                agent_id: stable_id.to_string(),
            })?;
        Ok(effective_model_for_definition(
            &self.categories,
            &self.router,
            base_model,
            binding.agent_model_preference(definition.stable_id),
            &definition,
        ))
    }

    /// Set or clear one Agent's remembered base model.
    ///
    /// The Agent is resolved against the caller's immutable [`TurnBinding`]. A
    /// set operation rejects direct/category Agent policy and requires an exact
    /// provider-catalog row. A clear operation is idempotent, including for a
    /// stale stored row, but still requires the Agent to resolve exactly.
    ///
    /// Durable mutation happens before the cached complete map is changed and
    /// published. A failed store operation therefore leaves both cached and
    /// live snapshots unchanged; post-commit publication is infallible.
    ///
    /// # Arguments
    ///
    /// * `binding` - Immutable catalog view used for exact Agent resolution.
    /// * `stable_id` - Exact stable catalog Agent id to mutate.
    /// * `identity` - New provider/model identity, or `None` to clear it.
    ///
    /// # Errors
    /// Returns [`AgentModelControlError::UnknownAgent`] for an unknown Agent,
    /// [`AgentModelControlError::ConfiguredAgent`] when setting an explicitly
    /// routed Agent, [`AgentModelControlError::ModelUnavailable`] when the
    /// identity is absent from the current catalog, or
    /// [`AgentModelControlError::Store`] for a durable mutation failure.
    pub async fn set(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        identity: Option<AgentModelIdentity>,
    ) -> Result<(), AgentModelControlError> {
        self.set_and_publish(binding, stable_id, identity)
            .await
            .map(|_| ())
    }

    async fn set_and_publish(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        identity: Option<AgentModelIdentity>,
    ) -> Result<Option<ModelRef>, AgentModelControlError> {
        let mut preferences = self.preferences.lock().await;
        let definition = binding
            .agent_catalog()
            .resolve(stable_id)
            .filter(|definition| definition.stable_id == stable_id)
            .ok_or_else(|| AgentModelControlError::UnknownAgent {
                agent_id: stable_id.to_string(),
            })?;

        let published = match identity {
            Some(identity) => {
                if binding.configured_agent_model(stable_id).is_some()
                    || definition.model_policy.model.is_some()
                    || definition.model_policy.category.is_some()
                {
                    return Err(AgentModelControlError::ConfiguredAgent {
                        agent_id: stable_id.to_string(),
                    });
                }
                if !self.router.catalog().iter().any(|row| {
                    row.provider_id == identity.provider_id && row.model_id == identity.model_id
                }) {
                    return Err(AgentModelControlError::ModelUnavailable {
                        provider_id: identity.provider_id,
                        model_id: identity.model_id,
                    });
                }
                let model = identity.model_ref();
                self.store
                    .upsert_agent_model_preference(
                        self.owner,
                        &AgentModelPreference {
                            agent: AgentName::new(stable_id),
                            provider_id: identity.provider_id,
                            model_id: identity.model_id,
                        },
                    )
                    .await?;
                preferences.insert(stable_id.to_string(), model.clone());
                Some(model)
            }
            None => {
                self.store
                    .remove_agent_model_preference(self.owner, &AgentName::new(stable_id))
                    .await?;
                preferences.remove(stable_id);
                None
            }
        };
        self.runtime
            .publish_agent_model_preferences(preferences.clone());
        Ok(published)
    }
}

/// Adapt the app-owned durable control to the dependency-inverted server port.
impl hya_server::AgentModelControl for PersistentAgentModelControl {
    fn available(&self) -> bool {
        true
    }

    fn configuration_available(&self) -> bool {
        self.configuration_files.is_some()
    }

    fn list(
        &self,
        binding: TurnBinding,
        base_model: ModelRef,
    ) -> hya_server::AgentModelControlFuture<'_, Vec<hya_server::AgentModelState>> {
        Box::pin(async move {
            let mut rows =
                project_agent_models(&binding, &self.categories, &self.router, &base_model);
            for row in &mut rows {
                row.configuration_path = self
                    .configuration_path(&binding, &row.agent_id)
                    .map_err(server_control_error)?;
            }
            Ok(rows)
        })
    }

    fn set(
        &self,
        binding: TurnBinding,
        agent_id: String,
        preference: Option<hya_server::AgentModelIdentity>,
        base_model: ModelRef,
    ) -> hya_server::AgentModelControlFuture<'_, hya_server::AgentModelState> {
        Box::pin(async move {
            let app_preference = preference
                .map(|identity| AgentModelIdentity::new(identity.provider_id, identity.model_id));
            let preference = self
                .set_and_publish(&binding, &agent_id, app_preference)
                .await
                .map_err(server_control_error)?;
            let definition = binding
                .agent_catalog()
                .resolve(&agent_id)
                .filter(|definition| definition.stable_id == agent_id)
                .ok_or_else(|| {
                    hya_server::AgentModelControlError::new(
                        hya_server::AGENT_MODEL_UNKNOWN_AGENT,
                        format!("unknown Agent `{agent_id}`"),
                    )
                })?;
            let models = self.router.catalog();
            let mut row = project_agent_model(
                &binding,
                &self.categories,
                &self.router,
                &models,
                &base_model,
                preference.as_ref(),
                definition,
            );
            row.configuration_path = self
                .configuration_path(&binding, &agent_id)
                .map_err(server_control_error)?;
            Ok(row)
        })
    }

    fn save_configuration(
        &self,
        binding: TurnBinding,
        agent_id: String,
        preference: Option<hya_server::AgentModelIdentity>,
        base_model: ModelRef,
    ) -> hya_server::AgentModelControlFuture<'_, hya_server::AgentModelState> {
        Box::pin(async move {
            let files = self
                .configuration_files
                .clone()
                .ok_or_else(hya_server::AgentModelControlError::unavailable)?;
            let definition = binding
                .agent_catalog()
                .resolve(&agent_id)
                .filter(|definition| definition.stable_id == agent_id)
                .ok_or_else(|| {
                    server_control_error(AgentModelControlError::UnknownAgent {
                        agent_id: agent_id.clone(),
                    })
                })?;
            let models = self.router.catalog();
            if let Some(identity) = &preference
                && !model_is_available(&models, identity)
            {
                return Err(server_control_error(
                    AgentModelControlError::ModelUnavailable {
                        provider_id: identity.provider_id.clone(),
                        model_id: identity.model_id.clone(),
                    },
                ));
            }
            let model = preference.map(|identity| {
                AgentModelIdentity::new(identity.provider_id, identity.model_id).model_ref()
            });
            let bundle_id = definition.origin.bundle_id().map(str::to_string);
            let writer_bundle = bundle_id.clone();
            let writer_agent = agent_id.clone();
            let writer_model = model.clone();
            let _mutation = self.preferences.lock().await;
            let mut configuration = self.configuration.lock().await;
            let path = tokio::task::spawn_blocking(move || {
                let origin = writer_bundle
                    .as_deref()
                    .map_or(AgentOrigin::Builtin, |bundle_id| AgentOrigin::Bundle {
                        bundle_id,
                    });
                files.set_model(origin, &writer_agent, writer_model.as_ref())
            })
            .await
            .map_err(|error| {
                server_control_error(AgentModelControlError::Configuration(error.into()))
            })?
            .map_err(|error| server_control_error(AgentModelControlError::Configuration(error)))?;
            let configured_models = match bundle_id {
                Some(bundle_id) => configuration.bundles.entry(bundle_id).or_default(),
                None => &mut configuration.builtin,
            };
            if let Some(model) = model {
                configured_models.insert(agent_id.clone(), model);
            } else {
                configured_models.remove(&agent_id);
            }
            self.runtime
                .publish_agent_model_configuration(configuration.clone());
            let fresh = binding
                .clone()
                .with_agent_model_configuration(configuration.clone());
            let mut row = project_agent_model(
                &fresh,
                &self.categories,
                &self.router,
                &models,
                &base_model,
                fresh.agent_model_preference(&agent_id),
                definition,
            );
            row.configuration_path = Some(path.to_string_lossy().into_owned());
            Ok(row)
        })
    }
}

/// Convert app control failures to stable server codes and bounded messages.
fn server_control_error(error: AgentModelControlError) -> hya_server::AgentModelControlError {
    let (code, message) = match error {
        AgentModelControlError::UnknownAgent { agent_id } => (
            hya_server::AGENT_MODEL_UNKNOWN_AGENT,
            format!("unknown Agent `{agent_id}`"),
        ),
        AgentModelControlError::ConfiguredAgent { agent_id } => (
            hya_server::AGENT_MODEL_CONFIGURED,
            format!("Agent `{agent_id}` has an explicit model policy"),
        ),
        AgentModelControlError::ModelUnavailable {
            provider_id,
            model_id,
        } => (
            hya_server::AGENT_MODEL_UNAVAILABLE,
            format!("model `{provider_id}/{model_id}` is unavailable in the provider catalog"),
        ),
        AgentModelControlError::Store(error) => (
            hya_server::AGENT_MODEL_CONTROL_FAILURE,
            format!("Agent model preference store failed: {error}"),
        ),
        AgentModelControlError::Configuration(error) => (
            hya_server::AGENT_MODEL_CONTROL_FAILURE,
            format!("Agent model configuration failed: {error}"),
        ),
    };
    hya_server::AgentModelControlError::new(code, message)
}

/// Project every row from one immutable catalog/preference binding.
fn project_agent_models(
    binding: &TurnBinding,
    categories: &CategoryRegistry,
    router: &ProviderRouter,
    base_model: &ModelRef,
) -> Vec<hya_server::AgentModelState> {
    let models = router.catalog();
    binding
        .agent_catalog()
        .all()
        .into_iter()
        .map(|agent| {
            project_agent_model(
                binding,
                categories,
                router,
                &models,
                base_model,
                binding.agent_model_preference(agent.stable_id),
                agent,
            )
        })
        .collect()
}

/// Project one catalog Agent with an explicitly selected preference snapshot.
fn project_agent_model(
    binding: &TurnBinding,
    categories: &CategoryRegistry,
    router: &ProviderRouter,
    models: &[hya_provider::ProviderModel],
    base_model: &ModelRef,
    preference_model: Option<&ModelRef>,
    agent: hya_core::AgentDefinition<'_>,
) -> hya_server::AgentModelState {
    let configuration = binding
        .configured_agent_model(agent.stable_id)
        .map(|model| model_identity(model.as_str()));
    let configured = configuration.is_some()
        || agent.model_policy.model.is_some()
        || agent.model_policy.category.is_some();
    let session_override = binding
        .session_agent_model(agent.stable_id)
        .map(|model| model_identity(model.as_str()));
    let agent = binding.resolve_agent(agent.stable_id).unwrap_or(agent);
    let preference = preference_model.map(|model| model_identity(model.as_str()));
    let preference_available = preference
        .as_ref()
        .is_some_and(|identity| model_is_available(models, identity));

    let effective_model =
        effective_model_for_definition(categories, router, base_model, preference_model, &agent);
    let effective = model_identity(effective_model.as_str());
    let source = if session_override.is_some() {
        hya_server::AgentModelSource::Session
    } else if configured {
        hya_server::AgentModelSource::Configured
    } else if preference_available {
        hya_server::AgentModelSource::Remembered
    } else {
        hya_server::AgentModelSource::Default
    };

    hya_server::AgentModelState {
        agent_id: agent.stable_id.to_string(),
        description: agent.description.map(str::to_string),
        mode: agent.selector_mode().to_string(),
        hidden: binding.agent_catalog().is_reserved(agent.stable_id),
        configured,
        settable: !configured,
        preference,
        preference_available,
        effective: hya_server::AgentModelEffective {
            model: effective,
            source,
        },
        configuration,
        configuration_path: None,
        session_override,
    }
}

/// Resolve configured, remembered, and process-base tiers for one Agent.
fn effective_model_for_definition(
    categories: &CategoryRegistry,
    router: &ProviderRouter,
    base_model: &ModelRef,
    preference: Option<&ModelRef>,
    agent: &hya_core::AgentDefinition<'_>,
) -> ModelRef {
    if agent.model_policy.model.is_some() || agent.model_policy.category.is_some() {
        return resolve_configured_agent_model(&agent.model_policy, categories, &|model| {
            router.resolve(model).is_some()
        })
        .unwrap_or_else(|| base_model.clone());
    }
    preference
        .filter(|model| router.resolve(model).is_some())
        .cloned()
        .unwrap_or_else(|| base_model.clone())
}

/// Split one canonical model reference at only its provider separator.
fn model_identity(model: &str) -> hya_server::AgentModelIdentity {
    let (provider, model) = model.split_once('/').unwrap_or(("hya", model));
    hya_server::AgentModelIdentity::new(provider, model)
}

/// Return whether a normalized identity exists in the exact provider catalog.
fn model_is_available(
    models: &[hya_provider::ProviderModel],
    identity: &hya_server::AgentModelIdentity,
) -> bool {
    models.iter().any(|model| {
        model.provider_id == identity.provider_id && model.model_id == identity.model_id
    })
}

/// Load the complete durable preference map for startup publication.
async fn stored_preferences(
    store: &SessionStore,
) -> Result<BTreeMap<String, ModelRef>, AgentModelControlError> {
    Ok(store
        .list_agent_model_preferences()
        .await?
        .into_iter()
        .map(|row| {
            (
                row.agent.as_str().to_string(),
                ModelRef::new(format!("{}/{}", row.provider_id, row.model_id)),
            )
        })
        .collect())
}
