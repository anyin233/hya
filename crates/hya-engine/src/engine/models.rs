use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use hya_proto::{ModelRef, SessionId};
use hya_provider::{ProviderCatalogSnapshot, ProviderModel, ProviderRouter, ReasoningEffort};
use hya_tool::AgentDef;

use super::SessionEngine;
use crate::{CoreError, TurnBinding};

/// Per-turn memory of the last successful effort-preference reads. Healthy turns
/// still read SQLite every time; only a later storage fault or timeout reuses
/// the last value and emits one warning for the turn.
#[derive(Default)]
pub(super) struct TurnEffortFallback {
    agent_list: Option<BTreeMap<String, ReasoningEffort>>,
    agent_pref: Option<(String, Option<ReasoningEffort>)>,
    model_prefs: HashMap<String, Option<ReasoningEffort>>,
    warned: bool,
}

async fn cached_pref<T: Clone, F, Fut>(
    timeout: std::time::Duration,
    operation: &'static str,
    turn: Option<(SessionId, &mut TurnEffortFallback)>,
    read: F,
    get: impl FnOnce(&TurnEffortFallback) -> Option<&T>,
    put: impl FnOnce(&mut TurnEffortFallback, &T),
) -> Result<T, CoreError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, hya_store::StoreError>>,
{
    let Some((session, fb)) = turn else {
        return Ok(read().await?);
    };
    match tokio::time::timeout(timeout, read())
        .await
        .map_err(|_| hya_store::StoreError::StorageTimeout { operation, timeout })
        .and_then(|res| res)
    {
        Ok(val) => {
            put(fb, &val);
            Ok(val)
        }
        Err(error) => {
            let Some(prev) = get(fb).filter(|_| error.is_storage_fault()).cloned() else {
                return Err(error.into());
            };
            if !fb.warned {
                fb.warned = true;
                tracing::warn!(%session, operation, "effort preference store unavailable; reusing last read value for turn: {error}");
            }
            Ok(prev)
        }
    }
}

impl SessionEngine {
    /// An Agent's own default effort and its layer: the user's runtime choice
    /// (durable, per Agent) > `agents.<id>.reasoning` configuration > the
    /// bundle Agent's authored `model_policy.reasoning` (`authored`).
    ///
    /// # Errors
    ///
    /// Store read failure for the runtime choice.
    pub async fn agent_effort(
        &self,
        agent_id: &str,
        authored: Option<ReasoningEffort>,
        binding: Option<&TurnBinding>,
    ) -> Result<Option<(ReasoningEffort, crate::AgentEffortSource)>, CoreError> {
        self.agent_effort_with(agent_id, authored, binding, None)
            .await
    }

    async fn agent_effort_with(
        &self,
        agent_id: &str,
        authored: Option<ReasoningEffort>,
        binding: Option<&TurnBinding>,
        turn: Option<(SessionId, &mut TurnEffortFallback)>,
    ) -> Result<Option<(ReasoningEffort, crate::AgentEffortSource)>, CoreError> {
        let store = &self.store;
        let preference = cached_pref(
            store.read_timeout(),
            "get_agent_effort_preference",
            turn,
            || async {
                Ok(store
                    .get_agent_effort_preference(agent_id)
                    .await?
                    .and_then(|r| ReasoningEffort::parse(&r.effort)))
            },
            |fb| {
                fb.agent_pref
                    .as_ref()
                    .and_then(|(id, v)| (id == agent_id).then_some(v))
            },
            |fb, &val| match &mut fb.agent_pref {
                Some((id, prev)) if id == agent_id => *prev = val,
                slot => *slot = Some((agent_id.to_string(), val)),
            },
        )
        .await?;
        let configured = binding
            .and_then(|binding| binding.project_agent_effort(agent_id))
            .unwrap_or_else(|| {
                self.runtime
                    .agent_effort_configuration()
                    .get(agent_id)
                    .copied()
            });
        Ok(crate::agent_effort(preference, configured, authored))
    }

    /// Layer the user's runtime (durable) and configured Agent efforts over a
    /// roster's authored ones, so `list_agents` shows what each Agent would
    /// run at. One store read for the whole roster.
    ///
    /// # Errors
    ///
    /// Store read failure.
    pub async fn annotate_agent_efforts(
        &self,
        agents: Arc<[AgentDef]>,
        binding: Option<&TurnBinding>,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        self.annotate_agent_efforts_with(agents, binding, None)
            .await
    }

    pub(super) async fn annotate_agent_efforts_with(
        &self,
        agents: Arc<[AgentDef]>,
        binding: Option<&TurnBinding>,
        turn: Option<(SessionId, &mut TurnEffortFallback)>,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        let store = &self.store;
        let preferences = cached_pref(
            store.read_timeout(),
            "list_agent_effort_preferences",
            turn,
            || async {
                Ok(store
                    .list_agent_effort_preferences()
                    .await?
                    .into_iter()
                    .filter_map(|row| Some((row.agent_id, ReasoningEffort::parse(&row.effort)?)))
                    .collect::<BTreeMap<String, ReasoningEffort>>())
            },
            |fb| fb.agent_list.as_ref(),
            |fb, val| fb.agent_list = Some(val.clone()),
        )
        .await?;
        let configured = self.runtime.agent_effort_configuration();
        if preferences.is_empty() && configured.is_empty() && binding.is_none() {
            return Ok(agents);
        }
        Ok(agents
            .iter()
            .map(|agent| {
                let authored = agent.effort.as_deref().and_then(ReasoningEffort::parse);
                let mut agent = agent.clone();
                if let Some((effort, source)) = crate::agent_effort(
                    preferences.get(&agent.name).copied(),
                    binding
                        .and_then(|binding| binding.project_agent_effort(&agent.name))
                        .unwrap_or_else(|| configured.get(&agent.name).copied()),
                    authored,
                ) {
                    agent.effort = Some(effort.as_str().to_string());
                    agent.effort_source = Some(
                        match source {
                            crate::AgentEffortSource::Preference => "preference",
                            crate::AgentEffortSource::Configured => "configured",
                            crate::AgentEffortSource::Authored => "authored",
                        }
                        .to_string(),
                    );
                }
                agent
            })
            .collect::<Vec<_>>()
            .into())
    }

    /// The effort a request for `model` by Agent `agent_id` carries, and
    /// which layer chose it: `model#variant` > the Agent's own default
    /// ([`Self::agent_effort`]) > the user's stored per-model preference > the
    /// model's configured default > the global `reasoning:` default > none.
    /// The turn loop and the v1 session info both call this, so what clients
    /// display is what requests send.
    ///
    /// # Errors
    ///
    /// Store read failure for a preference.
    pub async fn effective_effort(
        &self,
        model: &ModelRef,
        agent_id: Option<&str>,
        authored: Option<ReasoningEffort>,
        binding: Option<&TurnBinding>,
    ) -> Result<crate::EffectiveEffort, CoreError> {
        self.effective_effort_with(model, agent_id, authored, binding, None)
            .await
    }

    pub(super) async fn effective_effort_with(
        &self,
        model: &ModelRef,
        agent_id: Option<&str>,
        authored: Option<ReasoningEffort>,
        binding: Option<&TurnBinding>,
        mut turn: Option<(SessionId, &mut TurnEffortFallback)>,
    ) -> Result<crate::EffectiveEffort, CoreError> {
        let agent = match agent_id {
            Some(id) => self
                .agent_effort_with(
                    id,
                    authored,
                    binding,
                    turn.as_mut().map(|(s, fb)| (*s, &mut **fb)),
                )
                .await?
                .map(|(effort, _)| effort),
            None => authored,
        };
        let base = model
            .as_str()
            .rsplit_once('#')
            .map_or(model.as_str(), |(b, _)| b);
        let preference = match base.split_once('/') {
            Some((provider, model_id)) => {
                let store = &self.store;
                cached_pref(
                    store.read_timeout(),
                    "get_model_effort_preference",
                    turn,
                    || async {
                        Ok(store
                            .get_model_effort_preference(provider, model_id)
                            .await?
                            .and_then(|r| ReasoningEffort::parse(&r.effort)))
                    },
                    |fb| fb.model_prefs.get(base),
                    |fb, &val| {
                        if let Some(prev) = fb.model_prefs.get_mut(base) {
                            *prev = val;
                        } else {
                            fb.model_prefs.insert(base.to_string(), val);
                        }
                    },
                )
                .await?
            }
            None => None,
        };
        Ok(crate::resolve_effort(
            model,
            agent,
            preference,
            self.provider_router().reasoning_default(model),
            self.global_reasoning,
            None,
        ))
    }

    /// Ordered cross-model candidates for a preferred model (preferred
    /// first), or `None` when no chain is configured for it.
    pub(crate) fn model_fallback_chain(&self, preferred: &ModelRef) -> Option<&[ModelRef]> {
        Some(self.model_fallbacks.get(preferred)?.as_slice())
    }

    /// Provider catalog models exposed to the UI/API.
    #[must_use]
    pub fn provider_catalog(&self) -> Vec<ProviderModel> {
        self.provider_catalog_snapshot().models().to_vec()
    }

    /// Shared provider catalog snapshot including declared provider statuses.
    #[must_use]
    pub fn provider_catalog_snapshot(&self) -> Arc<ProviderCatalogSnapshot> {
        match self.catalog.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Shared provider router retained by this engine for request-local Workflow routes.
    #[must_use]
    pub fn provider_router(&self) -> Arc<ProviderRouter> {
        match self.providers.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Atomically replace live provider routes and the presentation catalog.
    ///
    /// Used after background `models.yml.cache` refresh so HTTP/TUI surfaces
    /// observe the new rows without restarting the process.
    pub fn publish_provider_catalog(
        &self,
        providers: Arc<ProviderRouter>,
        catalog: Arc<ProviderCatalogSnapshot>,
    ) {
        match self.providers.write() {
            Ok(mut guard) => *guard = providers,
            Err(poisoned) => *poisoned.into_inner() = providers,
        }
        match self.catalog.write() {
            Ok(mut guard) => *guard = catalog,
            Err(poisoned) => *poisoned.into_inner() = catalog,
        }
    }
}
