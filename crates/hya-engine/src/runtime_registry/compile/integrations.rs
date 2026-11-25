use super::*;

/// One hook dispatcher bound to an agent (see
/// [`TurnBinding::bound_hooks_for_agent`]).
#[derive(Clone)]
pub(crate) struct BoundAgentHook {
    /// What to dispatch to (possibly a per-agent filter over `source`).
    pub(crate) dispatcher: Arc<dyn crate::hooks::HookDispatcher>,
    /// The retained source dispatcher: its pointer identifies the process.
    pub(crate) source: Arc<dyn crate::hooks::HookDispatcher>,
    /// Hook ids the filter admits; `None` for an unfiltered entry.
    pub(crate) hook_ids: Option<Arc<[String]>>,
}

impl TurnBinding {
    /// The API endpoints `bundle_id` serves in this binding's generation
    /// (its scope's view: a Project binding sees the Project's bundles).
    /// `None` when the bundle is not in the catalog or declares no endpoints.
    #[must_use]
    pub fn bundle_apis(&self, bundle_id: &str) -> Option<crate::bundle_apis::SourceApis> {
        snapshot_bundle_apis(&self.snapshot, bundle_id)
    }

    /// Every bundle's declared endpoints in this binding's generation,
    /// sorted by bundle id.
    #[must_use]
    pub fn published_bundle_apis(&self) -> Vec<crate::bundle_apis::PublishedBundleApis> {
        snapshot_published_bundle_apis(&self.snapshot)
    }

    /// Every bundle's declared session permission modes in this binding's
    /// generation, sorted by bundle id then mode id.
    #[must_use]
    pub fn published_permission_modes(
        &self,
    ) -> Vec<crate::permission_mode::PublishedPermissionMode> {
        published_permission_modes(&self.snapshot)
    }

    /// The declaring bundle's process hooks when `bundle_id` publishes the
    /// session permission mode `mode` in this generation; `None` when the
    /// bundle is gone, no longer declares the mode, or has no process.
    #[must_use]
    pub fn permission_mode_hooks(
        &self,
        bundle_id: &str,
        mode: &str,
    ) -> Option<Arc<dyn crate::hooks::HookDispatcher>> {
        let source = self
            .snapshot
            .sources
            .get(&RuntimeSourceId::bundle(bundle_id))?;
        if !source
            .permission_modes
            .iter()
            .any(|declared| declared.id == mode)
        {
            return None;
        }
        self.bundle_hooks(bundle_id)
    }

    /// Retain one explicitly selected bundle's process hooks from this generation.
    #[must_use]
    pub fn bundle_hooks(&self, bundle_id: &str) -> Option<Arc<dyn crate::hooks::HookDispatcher>> {
        if !self
            .snapshot
            .catalog
            .bundles()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == bundle_id)
        {
            return None;
        }
        self.snapshot
            .sources
            .get(&RuntimeSourceId::bundle(bundle_id))?
            .hooks
            .clone()
    }

    /// Read a bundle-owned Skill from this immutable generation.
    #[must_use]
    pub fn bundle_skill_content(&self, bundle_id: &str, local_id: &str) -> Option<&str> {
        self.snapshot
            .catalog
            .bundles()
            .bundles()
            .iter()
            .find(|bundle| bundle.identity().id == bundle_id)?
            .skills()
            .iter()
            .find(|skill| skill.local_id == local_id)
            .map(|skill| skill.content.as_str())
    }

    /// Restrict a private process's complete exports to this agent's hook view.
    pub fn sidecar_hook_view(
        &self,
        stable_agent_id: &str,
        dispatcher: Arc<dyn crate::hooks::HookDispatcher>,
    ) -> Result<Option<Arc<dyn crate::hooks::HookDispatcher>>, BundleError> {
        let policy = self.agent_resource_policy(stable_agent_id)?;
        Ok((!policy.canonical_hook_ids.is_empty()).then(|| {
            Arc::new(crate::bundle_hooks::ScopedBundleHooks::new(
                dispatcher,
                &policy.canonical_hook_ids,
            )) as Arc<dyn crate::hooks::HookDispatcher>
        }))
    }

    /// Hooks bound to one agent, in dispatch order.
    ///
    /// Every agent gets user-global config plugins, installed Plugin-kind
    /// bundles, then project plugins, each in ascending source-id order.
    /// A bundle agent (AgentBundle, AgentSetBundle,
    /// WorkflowBundle) additionally gets its own bundle's process hooks,
    /// filtered to the agent's `hook_refs`, after the Plugin hooks. An unknown
    /// agent gets none. Global config plugin entries omit events, which the
    /// engine publishes independently. Other Plugin entries are the retained
    /// source dispatchers, so callers can deduplicate them by pointer. No live
    /// registry read occurs.
    #[must_use]
    pub fn bundle_hooks_for_agent(
        &self,
        stable_agent_id: &str,
    ) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        self.bound_hooks_for_agent(stable_agent_id)
            .into_iter()
            .map(|hook| hook.dispatcher)
            .collect()
    }

    /// [`Self::bundle_hooks_for_agent`] with each entry's process identity:
    /// the retained source dispatcher plus, for the owner bundle's filtered
    /// entry, the hook ids it admits. Two bindings' entries with equal
    /// identities dispatch to the same process in the same way.
    pub(crate) fn bound_hooks_for_agent(&self, stable_agent_id: &str) -> Vec<BoundAgentHook> {
        let Some(agent) = self.resolve_agent(stable_agent_id) else {
            return Vec::new();
        };
        let owner = agent.origin.bundle_id();
        let global = self.global_plugin_hooks();
        let mut hooks = self
            .plugin_bundle_hooks(owner)
            .into_iter()
            .map(|dispatcher| BoundAgentHook {
                source: Arc::clone(&dispatcher),
                // Global events have one unconditional delivery path in
                // publish_envelope; bindings retain ordered non-event hooks.
                dispatcher: if global.iter().any(|hooks| Arc::ptr_eq(hooks, &dispatcher)) {
                    Arc::new(crate::bundle_hooks::ScopedBundleHooks::without_events(
                        dispatcher,
                    ))
                } else {
                    dispatcher
                },
                hook_ids: None,
            })
            .collect::<Vec<_>>();
        if let Some(bundle_id) = owner
            && let Some(owner_hooks) = self.bundle_hooks(bundle_id)
            && let Ok(policy) = self.agent_resource_policy(stable_agent_id)
            && !policy.canonical_hook_ids.is_empty()
        {
            hooks.push(BoundAgentHook {
                dispatcher: Arc::new(crate::bundle_hooks::ScopedBundleHooks::new(
                    Arc::clone(&owner_hooks),
                    &policy.canonical_hook_ids,
                )),
                source: owner_hooks,
                hook_ids: Some(Arc::clone(&policy.canonical_hook_ids)),
            });
        }
        hooks
    }

    /// User-global hooks retained by this immutable turn binding.
    #[must_use]
    pub fn global_plugin_hooks(&self) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        self.snapshot
            .sources
            .values()
            .filter(|source| {
                source.id.kind() == RuntimeSourceKind::Plugin && !source.project_plugin
            })
            .filter_map(|source| source.hooks.clone())
            .collect()
    }

    /// Captured idle-session hooks exclude process-wide config plugins.
    pub(crate) fn scope_hooks_for_agent(&self, agent: &str) -> Vec<BoundAgentHook> {
        let global = self.global_plugin_hooks();
        self.bound_hooks_for_agent(agent)
            .into_iter()
            .filter(|hook| {
                !global
                    .iter()
                    .any(|source| Arc::ptr_eq(source, &hook.source))
            })
            .collect()
    }

    /// User-global plugins, installed Plugin-kind bundles, then project
    /// plugins, each in stable ascending
    /// source-id order, excluding bundle `exclude` so an owner is never
    /// dispatched twice.
    fn plugin_bundle_hooks(
        &self,
        exclude: Option<&str>,
    ) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        let mut sources = self.snapshot.sources.values().collect::<Vec<_>>();
        sources.sort_by_key(|source| match source.id.kind() {
            RuntimeSourceKind::Plugin if !source.project_plugin => 0,
            RuntimeSourceKind::Bundle => 1,
            _ => 2,
        });
        sources
            .into_iter()
            .filter(|source| {
                source.id.kind() == RuntimeSourceKind::Plugin
                    || (source.id.kind() == RuntimeSourceKind::Bundle
                        && exclude != Some(source.id.configured_id())
                        && self
                            .snapshot
                            .catalog
                            .bundles()
                            .bundles()
                            .iter()
                            .any(|bundle| {
                                bundle.identity().id == source.id.configured_id()
                                    && (bundle.agents().is_empty() && bundle.workflow().is_none())
                            }))
            })
            .filter_map(|source| source.hooks.clone())
            .collect()
    }
}
