use super::*;

/// One admitted turn's immutable runtime binding.
///
/// Keep every field behind an `Arc` (place data goes in [`BindingPlace`]):
/// bindings are moved by value through deep async state machines, and a
/// larger binding has overflowed the stack before.
#[derive(Clone)]
pub struct TurnBinding {
    pub(super) snapshot: Arc<RuntimeSnapshot>,
    pub(super) agent_model_preferences: AgentModelPreferenceSnapshot,
    pub(super) agent_model_configuration: AgentModelConfigurationSnapshot,
    pub(super) session_agent_models: AgentModelPreferenceSnapshot,
    pub(super) place: Arc<BindingPlace>,
}

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

/// Where a [`TurnBinding`] was bound: one `Arc` keeps the binding small.
pub(super) struct BindingPlace {
    pub(super) workdir: PathBuf,
    pub(super) scope: CatalogScope,
    pub(super) project_bundle_dirs: Arc<BTreeMap<String, PathBuf>>,
    pub(super) bundle_efforts: Arc<BTreeMap<String, BTreeMap<String, ReasoningEffort>>>,
}

#[cfg(test)]
impl BindingPlace {
    fn new(workdir: PathBuf, scope: CatalogScope) -> Arc<Self> {
        Arc::new(Self {
            workdir,
            scope,
            project_bundle_dirs: Arc::new(BTreeMap::new()),
            bundle_efforts: Arc::new(BTreeMap::new()),
        })
    }
}

/// Which Harness-owned resources a bound agent may see.
///
/// **Derived from [`AgentOrigin`], never from a manifest.** A bundle author
/// cannot widen their own plane; that is the whole point of the clamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentToolPlane {
    /// Built-in agents: the live tool snapshot, Harness skills, and Harness MCP.
    Full,
    /// Bundle agents: the internal public tool snapshot captured when the
    /// registry was built. No Harness skills, no Harness MCP, no plugin tools.
    InternalPublic,
}

impl AgentToolPlane {
    /// Plane for an agent of this origin.
    #[must_use]
    pub const fn for_origin(origin: &AgentOrigin<'_>) -> Self {
        match origin {
            AgentOrigin::Builtin => Self::Full,
            AgentOrigin::Bundle { .. } => Self::InternalPublic,
        }
    }

    /// Short label used in diagnostics and plane-violation errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::InternalPublic => "internal-public",
        }
    }
}

/// Catalog-derived policy retained only for one in-process agent execution.
/// It contains no agent identity and is never persisted or exposed on the wire.
#[derive(Clone, Debug)]
pub struct AgentResourcePolicy {
    /// Owning bundle id, or `None` for a built-in agent (which owns no bundle).
    bundle_id: Option<String>,
    plane: AgentToolPlane,
    resource_view: ResourceView,
    /// Whether the agent may spawn anyone (a non-empty effective spawn set);
    /// decides the `task`/`archive` coordination tools.
    spawn_rights: bool,
    selected_bundle_tool_ids: Arc<Vec<String>>,
    selected_bundle_skill_ids: Arc<Vec<String>>,
    canonical_hook_ids: Arc<[String]>,
}

impl AgentResourcePolicy {
    /// Bundle-local tool ids selected for this agent view.
    #[must_use]
    pub fn selected_bundle_tool_ids(&self) -> &[String] {
        self.selected_bundle_tool_ids.as_slice()
    }

    /// Bundle-local Skill ids selected for this agent view.
    #[must_use]
    pub fn selected_bundle_skill_ids(&self) -> &[String] {
        self.selected_bundle_skill_ids.as_slice()
    }

    /// Canonical hook ids activated for this agent view.
    #[must_use]
    pub fn canonical_hook_ids(&self) -> &[String] {
        self.canonical_hook_ids.as_ref()
    }

    /// Harness resource plane this agent is bound to.
    #[must_use]
    pub fn plane(&self) -> AgentToolPlane {
        self.plane
    }

    /// Owning bundle id, or `None` for a built-in agent.
    #[must_use]
    pub fn bundle_id(&self) -> Option<&str> {
        self.bundle_id.as_deref()
    }

    /// Namespace used for bundle-local qualified public names.
    ///
    /// Built-ins have no bundle namespace; harness candidates keep their
    /// `harness:<kind>/<name>` spelling regardless.
    fn namespace(&self) -> &str {
        self.resource_view
            .namespace
            .as_deref()
            .or(self.bundle_id.as_deref())
            .unwrap_or("harness")
    }

    /// Scope label used in resource-resolution error messages.
    fn diagnostic_scope(&self) -> &str {
        self.bundle_id.as_deref().unwrap_or("builtin")
    }
}

/// Immutable per-turn/child resource map compiled once from a retained
/// [`TurnBinding`] and bound agent policy. Schema visibility, skill exposure,
/// and dispatch share this map; there is no registry fallback.
pub(crate) struct CompiledResourceView {
    tools: BTreeMap<String, ResolvedTool>,
    schemas: Vec<ToolSchema>,
    skills: Arc<Vec<SkillCatalogEntry>>,
    canonical_hook_ids: Arc<[String]>,
    /// Whether the selected view includes the canonical harness skill facade
    /// tool (regardless of any public alias spelling for that tool).
    skill_facade_selected: bool,
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

    /// Return a deterministic identity for the currently supported complete
    /// runtime view. Views with unidentifiable sources are intentionally
    /// unavailable until their semantic sections have a canonical encoding.
    #[must_use]
    /// Domain-separated fingerprint of agents, tools, sources, and permissions.
    ///
    /// v2 folds in the compiled-in built-in roster digest, which replaced the
    /// prepared-catalog digests the retired built-in bundles contributed.
    pub fn semantic_fingerprint_v1(&self, permission: &PermissionPlane) -> Option<[u8; 32]> {
        let catalog_identity = self.snapshot.catalog.semantic_identity_v1()?;
        let permission_identity = permission.semantic_identity_v1()?;
        let mut bytes = Vec::new();
        append_identity_bytes(&mut bytes, RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2).ok()?;

        append_identity_tag(&mut bytes, 1);
        append_identity_bytes(&mut bytes, &catalog_identity).ok()?;

        // The explicit empty section represents the none effective view.
        append_identity_tag(&mut bytes, 2);
        append_identity_count(&mut bytes, 0).ok()?;

        append_identity_tag(&mut bytes, 3);
        append_tool_view_identity(&mut bytes, &self.snapshot.basic_tools)?;

        append_identity_tag(&mut bytes, 4);
        append_tool_view_identity(&mut bytes, &self.snapshot.tools)?;

        append_identity_tag(&mut bytes, 5);
        let skills = self
            .snapshot
            .skills
            .get(&self.place.workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        append_skill_view_identity(&mut bytes, skills)?;

        append_identity_tag(&mut bytes, 6);
        append_runtime_source_view_identity(
            &mut bytes,
            &self.snapshot.sources,
            &self.snapshot.tools,
        )?;

        append_identity_tag(&mut bytes, 7);
        append_identity_bytes(&mut bytes, &permission_identity).ok()?;

        if !self.agent_model_configuration.builtin.is_empty()
            || self
                .agent_model_configuration
                .bundles
                .values()
                .any(|models| !models.is_empty())
        {
            append_identity_tag(&mut bytes, 8);
            append_model_configuration_identity(&mut bytes, &self.agent_model_configuration)
                .ok()?;
        }
        if !self.session_agent_models.is_empty() {
            append_identity_tag(&mut bytes, 9);
            append_model_map_identity(&mut bytes, &self.session_agent_models).ok()?;
        }

        Some(Sha256::digest(bytes).into())
    }

    #[must_use]
    /// Config generation of the retained snapshot.
    pub fn generation(&self) -> ConfigGeneration {
        self.snapshot.generation
    }

    /// Look up a remembered model by exact stable Agent id in this binding.
    ///
    /// The lookup reads the immutable preference map captured when this binding
    /// was created. Later registry publications do not affect the returned
    /// reference or any other lookup through this binding.
    ///
    /// # Arguments
    ///
    /// * `stable_id` - Exact catalog stable id of the Agent to look up.
    ///
    /// # Returns
    ///
    /// The remembered model for `stable_id`, or `None` when no preference was
    /// captured for that Agent.
    #[must_use]
    pub fn agent_model_preference(&self, stable_id: &str) -> Option<&ModelRef> {
        self.agent_model_preferences.get(stable_id)
    }

    /// Replace the temporary Agent model map captured by this binding.
    ///
    /// The returned binding owns an immutable copy; later calls on the
    /// registry or other bindings cannot change this map. An empty map keeps
    /// the historical no-override behavior.
    #[must_use]
    pub fn with_session_agent_models(mut self, models: BTreeMap<String, ModelRef>) -> Self {
        self.session_agent_models = Arc::new(models);
        self
    }

    /// Replace the captured user-file model configuration on this binding.
    ///
    /// This is useful after a successful configuration-file write when the
    /// caller already has a fresh binding and must not trigger another runtime
    /// bind or skill discovery. The captured Session override map is retained.
    #[must_use]
    pub fn with_agent_model_configuration(
        mut self,
        configuration: AgentModelConfiguration,
    ) -> Self {
        self.agent_model_configuration = Arc::new(configuration);
        self
    }

    /// Look up the user-file model for an Agent's original catalog origin.
    ///
    /// Authored direct/category policy is intentionally not considered here;
    /// this reports only the matching global built-in or owning-bundle file
    /// entry captured when the binding was made.
    #[must_use]
    pub fn configured_agent_model(&self, stable_id: &str) -> Option<&ModelRef> {
        let definition = self.snapshot.catalog.resolve(stable_id)?;
        match definition.origin {
            AgentOrigin::Builtin => self
                .agent_model_configuration
                .builtin
                .get(definition.stable_id),
            AgentOrigin::Bundle { bundle_id } => self
                .agent_model_configuration
                .bundles
                .get(bundle_id)
                .and_then(|models| models.get(definition.stable_id)),
        }
    }

    /// Look up a temporary Session-tree model override for an Agent.
    #[must_use]
    pub fn session_agent_model(&self, stable_id: &str) -> Option<&ModelRef> {
        self.session_agent_models.get(stable_id)
    }

    #[must_use]
    /// Working directory this turn was bound to.
    pub fn workdir(&self) -> &Path {
        &self.place.workdir
    }

    /// Catalog scope this binding was bound in.
    #[must_use]
    pub fn scope(&self) -> &CatalogScope {
        &self.place.scope
    }

    /// Scope bundle id to source directory for this binding's scope (empty
    /// without a scope overlay).
    #[must_use]
    pub fn project_bundle_dirs(&self) -> &BTreeMap<String, PathBuf> {
        &self.place.project_bundle_dirs
    }

    /// A project-owned Agent's configured effort, including explicit absence.
    pub fn project_agent_effort(&self, agent_id: &str) -> Option<Option<ReasoningEffort>> {
        let definition = self.agent_catalog().resolve(agent_id)?;
        let bundle = definition.origin.bundle_id()?;
        self.project_bundle_dirs().contains_key(bundle).then(|| {
            self.place
                .bundle_efforts
                .get(bundle)
                .and_then(|entries| entries.get(agent_id))
                .copied()
        })
    }

    #[must_use]
    /// Agent catalog (built-ins plus installed bundles) retained by this binding.
    pub fn agent_catalog(&self) -> &AgentCatalog {
        &self.snapshot.catalog
    }

    #[must_use]
    /// Installed-bundle catalog behind this binding's agent catalog.
    pub fn bundle_catalog(&self) -> &BundleCatalog {
        self.snapshot.catalog.bundles()
    }

    #[must_use]
    /// Published bare-name mask table: contested bare name → the canonical
    /// name of the winning tool. View compilation resolves every contested
    /// name to its winner; qualified canonical spellings always work.
    pub fn masks(&self) -> &BTreeMap<String, String> {
        &self.snapshot.masks
    }

    #[must_use]
    /// Published scheme table: registered external URI scheme → the binding of
    /// the winning source. Mirrors [`TurnBinding::masks`]. View compilation
    /// exposes a scheme for dispatch only when this view also contains the
    /// binding's owning tool.
    pub fn schemes(&self) -> &BTreeMap<String, SchemeBinding> {
        &self.snapshot.schemes
    }

    /// Every claimant of `scheme` ordered by ascending source id, with the
    /// active provider (the lexicographically greatest source id) last. Each
    /// entry is `(owner source label, canonical tool name)`. Unclaimed schemes
    /// yield an empty chain.
    #[must_use]
    pub fn scheme_chain(&self, scheme: &str) -> Vec<(String, String)> {
        self.snapshot
            .sources
            .values()
            .filter_map(|source| {
                let claim = source.schemas.iter().find(|claim| claim.scheme == scheme)?;
                Some((source.id.to_string(), claim.canonical_tool.clone()))
            })
            .collect()
    }

    #[must_use]
    /// Look up an agent by stable id, whatever its origin.
    pub fn resolve_agent(&self, stable_id: &str) -> Option<AgentDefinition<'_>> {
        self.snapshot
            .catalog
            .resolve(stable_id)
            .map(|definition| self.overlay_agent_model(definition))
    }
    /// Resolve whether `caller` may spawn `target`.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let definition = self.snapshot.catalog.resolve_spawn(caller, requested)?;
        Ok(self.overlay_agent_model(definition))
    }

    /// Agents the caller may spawn per can_spawn rules.
    pub fn spawnable_agents(&self, caller: &str) -> Result<Vec<AgentDefinition<'_>>, BundleError> {
        Ok(self
            .snapshot
            .catalog
            .spawnable(caller)?
            .into_iter()
            .map(|definition| self.overlay_agent_model(definition))
            .collect())
    }

    fn overlay_agent_model<'a>(&self, definition: AgentDefinition<'a>) -> AgentDefinition<'a> {
        let model = self
            .session_agent_model(definition.stable_id)
            .or_else(|| self.configured_agent_model(definition.stable_id));
        let Some(model) = model else {
            return definition;
        };
        let mut policy = definition.model_policy.into_owned();
        policy.model = Some(model.as_str().to_string());
        AgentDefinition {
            model_policy: std::borrow::Cow::Owned(policy),
            ..definition
        }
    }

    /// Compile the agent resource/tool policy for `stable_id`.
    pub fn agent_resource_policy(
        &self,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let definition = self.snapshot.catalog.require(stable_id)?;
        let plane = AgentToolPlane::for_origin(&definition.origin);
        let bundle_id = definition.origin.bundle_id().map(str::to_string);
        // Built-ins own no bundle: their view comes from the core-agents
        // preset (read-only agents deny the write tools), and they carry no hooks.
        let (resource_view, hook_refs) = match bundle_id.as_deref() {
            None => (
                crate::builtin_agents::preset_agent(stable_id)
                    .map(|agent| agent.resource_view.clone())
                    .unwrap_or_default(),
                Vec::new(),
            ),
            Some(id) => {
                let (_, agent) = self
                    .snapshot
                    .catalog
                    .bundles()
                    .resolve_agent_entry(stable_id)
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: format!("{id}/{stable_id}"),
                    })?;
                (agent.resource_view.clone(), agent.hook_refs.clone())
            }
        };
        // Spawn rights are the effective (installed) spawn set: a built-in with
        // the ordinary scope, or a bundle `can_spawn` naming an installed agent.
        let spawn_rights = self
            .snapshot
            .catalog
            .spawnable(stable_id)
            .is_ok_and(|agents| !agents.is_empty());
        let mut policy = AgentResourcePolicy {
            bundle_id,
            plane,
            resource_view,
            spawn_rights,
            selected_bundle_tool_ids: Arc::new(Vec::new()),
            selected_bundle_skill_ids: Arc::new(Vec::new()),
            canonical_hook_ids: Arc::from(hook_refs),
        };
        {
            let partitions = self.collect_resource_candidates(&policy)?;
            let selected = select_candidates_globally(&policy, &partitions)?;
            policy.selected_bundle_tool_ids = Arc::new(
                selected
                    .tool
                    .iter()
                    .filter(|id| {
                        partitions
                            .tool
                            .get(*id)
                            .is_some_and(ResourceCandidate::is_bundle_local)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            policy.selected_bundle_skill_ids = Arc::new(
                selected
                    .skill
                    .iter()
                    .filter(|id| {
                        partitions
                            .skill
                            .get(*id)
                            .is_some_and(ResourceCandidate::is_bundle_local)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        Ok(policy)
    }

    /// Test-only: compile the policy for `stable_id` on an explicit plane.
    ///
    /// Production **always** derives the plane from the agent's origin through
    /// [`AgentToolPlane::for_origin`]. The resource-view compiler itself is
    /// plane-agnostic, so its unit tests pin a plane directly rather than
    /// resurrect an author-facing knob. Not reachable outside `cfg(test)`.
    #[cfg(test)]
    pub(crate) fn agent_resource_policy_on_plane(
        &self,
        stable_id: &str,
        plane: AgentToolPlane,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let definition = self.snapshot.catalog.require(stable_id)?;
        let bundle_id = definition.origin.bundle_id().map(str::to_string);
        let (resource_view, hook_refs) = match bundle_id.as_deref() {
            None => (
                crate::builtin_agents::preset_agent(stable_id)
                    .map(|agent| agent.resource_view.clone())
                    .unwrap_or_default(),
                Vec::new(),
            ),
            Some(_) => {
                let (_, agent) = self
                    .snapshot
                    .catalog
                    .bundles()
                    .resolve_agent_entry(stable_id)
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: stable_id.to_string(),
                    })?;
                (agent.resource_view.clone(), agent.hook_refs.clone())
            }
        };
        Ok(AgentResourcePolicy {
            bundle_id,
            plane,
            resource_view,
            spawn_rights: self
                .snapshot
                .catalog
                .spawnable(stable_id)
                .is_ok_and(|agents| !agents.is_empty()),
            selected_bundle_tool_ids: Arc::new(Vec::new()),
            selected_bundle_skill_ids: Arc::new(Vec::new()),
            canonical_hook_ids: Arc::from(hook_refs),
        })
    }

    /// Report whether the selected agent's effective resource view needs a
    /// bundle sidecar in order to provide its executable capabilities.
    pub fn has_selected_bundle_sidecar_capability(
        &self,
        stable_id: &str,
    ) -> Result<bool, BundleError> {
        let policy = self.agent_resource_policy(stable_id)?;
        if policy.bundle_id.is_none() {
            // A built-in owns no bundle resources, so it never needs a sidecar.
            return Ok(false);
        }
        if policy
            .bundle_id
            .as_deref()
            .is_some_and(|id| self.bundle_hooks(id).is_some())
        {
            // Process extensions already own the complete declared executable surface.
            return Ok(false);
        }
        let partitions = self.collect_resource_candidates(&policy)?;
        let selected = select_candidates_globally(&policy, &partitions)?;
        let selected_bundle_tool = selected.tool.iter().any(|id| {
            partitions
                .tool
                .get(id)
                .is_some_and(ResourceCandidate::is_bundle_local)
        });
        Ok(selected_bundle_tool || !policy.canonical_hook_ids.is_empty())
    }

    fn collect_resource_candidates(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<CandidatePartitions, BundleError> {
        let bundles = self.snapshot.catalog.bundles();
        let mut tool_candidates = BTreeMap::new();
        let mut skill_candidates = BTreeMap::new();
        let mut mcp_candidates = BTreeMap::new();

        // Bundle-local resources exist only for a bundle agent, and only from
        // its OWN bundle. Resolving a `bundle:` reference against the whole
        // catalog would let one bundle borrow another bundle's tools.
        if let Some(bundle_id) = policy.bundle_id.as_deref() {
            collect_bundle_tool_candidates(bundles, bundle_id, &mut tool_candidates)?;
            for reference in &policy.resource_view.allow {
                if !reference.starts_with("bundle:")
                    || kind_from_qualified_reference(reference) != Some("tool")
                    || tool_candidates.contains_key(reference)
                {
                    continue;
                }
                let (owner, resource) =
                    bundles.resolve_resource_entry(bundle_id, ExportKind::Tool, reference)?;
                if owner != bundle_id {
                    return Err(BundleError::ResourceNotInPlane {
                        bundle_id: bundle_id.to_string(),
                        reference: reference.clone(),
                        plane: policy.plane.as_str().to_string(),
                    });
                }
                tool_candidates.insert(
                    resource.stable_id.clone(),
                    ResourceCandidate::BundleLocal {
                        short_name: resource.local_id.clone(),
                        qualified_name: resource.stable_id.clone(),
                        aliases: resource.aliases.clone(),
                    },
                );
            }
            collect_bundle_skill_candidates(
                bundles,
                &self.snapshot.sources,
                bundle_id,
                &mut skill_candidates,
            )?;
            collect_bundle_mcp_candidates(bundles, bundle_id, &mut mcp_candidates)?;
        }

        collect_harness_tool_candidates(
            policy.plane,
            &self.snapshot.basic_tools,
            &self.snapshot.tools,
            &self.snapshot.sources,
            &self.snapshot.masks,
            bundles,
            &mut tool_candidates,
        );
        collect_harness_skill_candidates(
            policy.plane,
            self.skills(),
            &self.snapshot.sources,
            bundles,
            &mut skill_candidates,
        );
        collect_harness_mcp_candidates(
            policy.plane,
            &self.snapshot.sources,
            &self.snapshot.masks,
            bundles,
            &mut mcp_candidates,
        );

        Ok(CandidatePartitions {
            tool: tool_candidates,
            skill: skill_candidates,
            mcp: mcp_candidates,
        })
    }

    pub(crate) fn compile_agent_resources(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        self.compile_agent_resources_with_sidecar_tools(policy, &[])
    }

    pub(crate) fn compile_agent_resources_with_sidecar_tools(
        &self,
        policy: &AgentResourcePolicy,
        sidecar_tools: &[ResolvedTool],
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        let view = &policy.resource_view;
        let bundle_id = policy.diagnostic_scope();
        let namespace = policy.namespace();

        let mut sidecar_tools_by_name = BTreeMap::new();
        for resolved in sidecar_tools {
            let canonical_id = resolved.tool.name().to_string();
            if sidecar_tools_by_name
                .insert(canonical_id.clone(), resolved.clone())
                .is_some()
            {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: canonical_id,
                });
            }
        }

        let partitions = self.collect_resource_candidates(policy)?;
        let selected = select_candidates_globally(policy, &partitions)?;

        let view_aliases = resolve_view_aliases(policy, &partitions, &selected, view)?;
        let tool_view_aliases = aliases_for_kind("tool", &view_aliases);
        let skill_view_aliases = aliases_for_kind("skill", &view_aliases);
        let mcp_view_aliases = aliases_for_kind("mcp", &view_aliases);

        let mut tool_public = assign_public_names(
            bundle_id,
            namespace,
            "tool",
            &partitions.tool,
            &selected.tool,
            &tool_view_aliases,
            Some(&partitions.mcp),
        )?;
        let mut skill_public = assign_public_names(
            bundle_id,
            namespace,
            "skill",
            &partitions.skill,
            &selected.skill,
            &skill_view_aliases,
            None,
        )?;
        let mut mcp_public = assign_public_names(
            bundle_id,
            namespace,
            "mcp",
            &partitions.mcp,
            &selected.mcp,
            &mcp_view_aliases,
            Some(&partitions.tool),
        )?;

        inject_effective_aliases(
            bundle_id,
            "tool",
            &partitions.tool,
            &selected.tool,
            &tool_view_aliases,
            &mut tool_public,
        )?;
        inject_effective_aliases(
            bundle_id,
            "skill",
            &partitions.skill,
            &selected.skill,
            &skill_view_aliases,
            &mut skill_public,
        )?;
        inject_effective_aliases(
            bundle_id,
            "mcp",
            &partitions.mcp,
            &selected.mcp,
            &mcp_view_aliases,
            &mut mcp_public,
        )?;

        // Tool and MCP share the provider tool/schema/dispatch namespace.
        for name in tool_public.keys() {
            if mcp_public.contains_key(name) {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: name.clone(),
                });
            }
        }

        let model_tool_names = model_schema_public_names(
            bundle_id,
            "tool",
            &partitions.tool,
            &tool_public,
            &tool_view_aliases,
        )?;
        let mut model_mcp_names = model_schema_public_names(
            bundle_id,
            "mcp",
            &partitions.mcp,
            &mcp_public,
            &mcp_view_aliases,
        )?;

        let skill_facade_selected = selected.tool.contains("harness:tool/skill");
        let has_harness_skill = selected.skill.iter().any(|id| {
            matches!(
                partitions.skill.get(id),
                Some(ResourceCandidate::HarnessSkill { .. })
            )
        });
        if has_harness_skill && !skill_facade_selected {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: "selected harness skills require the skill tool facade".to_string(),
            });
        }

        let mut tools = BTreeMap::new();
        let mut mcp_expansions = BTreeMap::new();
        for (public_name, canonical_id) in tool_public.iter().chain(mcp_public.iter()) {
            let (kind, candidates) = if partitions.tool.contains_key(canonical_id) {
                ("tool", &partitions.tool)
            } else {
                ("mcp", &partitions.mcp)
            };
            let candidate = candidates.get(canonical_id).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: canonical_id.clone(),
                }
            })?;
            match candidate {
                ResourceCandidate::BundleLocal { .. } if kind == "tool" => {
                    let process_tool = canonical_id
                        .strip_prefix("bundle:")
                        .and_then(|id| id.split_once("/tool/"))
                        .and_then(|(owner, local)| {
                            self.snapshot
                                .sources
                                .get(&RuntimeSourceId::bundle(owner))
                                .and_then(|source| {
                                    source.exports.iter().find(|export| {
                                        export.declared_id == local
                                            && export.permission != ToolPermission::Mcp
                                    })
                                })
                        })
                        .map(|export| ResolvedTool {
                            tool: Arc::new(NamedTool::new(
                                canonical_id.clone(),
                                export.tool.clone(),
                            )),
                            permission: export.permission,
                        });
                    if let Some(resolved) = sidecar_tools_by_name
                        .get(canonical_id)
                        .cloned()
                        .or(process_tool)
                    {
                        tools.insert(public_name.clone(), resolved);
                    } else {
                        return Err(BundleError::UnsupportedBundleFeature {
                            bundle_id: bundle_id.to_string(),
                            feature: "resources.tools".to_string(),
                        });
                    }
                }
                ResourceCandidate::BundleLocal { .. } => {
                    let (owner, server_id) = canonical_id
                        .strip_prefix("bundle:")
                        .and_then(|id| id.split_once("/mcp/"))
                        .ok_or_else(|| BundleError::UnknownResourceReference {
                            bundle_id: bundle_id.to_string(),
                            kind: "mcp".into(),
                            reference: canonical_id.clone(),
                        })?;
                    let source = self
                        .snapshot
                        .sources
                        .get(&RuntimeSourceId::bundle(owner))
                        .ok_or_else(|| BundleError::UnsupportedBundleFeature {
                            bundle_id: bundle_id.to_string(),
                            feature: "resources.mcp".into(),
                        })?;
                    let prefix = format!("mcp/{server_id}/");
                    let mut expanded_names = BTreeSet::new();
                    for export in source
                        .exports
                        .iter()
                        .filter(|export| export.permission == ToolPermission::Mcp)
                    {
                        let Some(local) = export.declared_id.strip_prefix(&prefix) else {
                            continue;
                        };
                        let name = format!("{public_name}__{local}");
                        if tools.contains_key(&name) {
                            return Err(BundleError::NamespaceCollision {
                                bundle_id: bundle_id.into(),
                                name,
                            });
                        }
                        tools.insert(
                            name.clone(),
                            ResolvedTool {
                                tool: export.tool.clone(),
                                permission: export.permission,
                            },
                        );
                        expanded_names.insert(name);
                    }
                    mcp_expansions.insert(public_name.clone(), expanded_names);
                }
                ResourceCandidate::HarnessTool { resolved, .. }
                | ResourceCandidate::HarnessMcp { resolved, .. } => {
                    tools.insert(public_name.clone(), resolved.clone());
                }
                ResourceCandidate::BundleLocalSkill { .. }
                | ResourceCandidate::HarnessSkill { .. } => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: kind.to_string(),
                        reference: canonical_id.clone(),
                    });
                }
            }
        }

        let mut skills = Vec::new();
        let mut ordered_skill_names = skill_public.keys().cloned().collect::<Vec<_>>();
        ordered_skill_names.sort();
        for public_name in ordered_skill_names {
            let canonical_id = skill_public.get(&public_name).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: "skill".to_string(),
                    reference: public_name.clone(),
                }
            })?;
            let candidate = partitions.skill.get(canonical_id).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: "skill".to_string(),
                    reference: canonical_id.clone(),
                }
            })?;
            let mut entry = match candidate {
                ResourceCandidate::BundleLocalSkill { entry, .. }
                | ResourceCandidate::HarnessSkill { entry, .. } => entry.clone(),
                ResourceCandidate::BundleLocal { .. }
                | ResourceCandidate::HarnessTool { .. }
                | ResourceCandidate::HarnessMcp { .. } => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: "skill".to_string(),
                        reference: canonical_id.clone(),
                    });
                }
            };
            entry.name = public_name;
            skills.push(entry);
        }

        for (server_name, expanded) in mcp_expansions {
            if model_mcp_names.remove(&server_name) {
                for name in expanded {
                    if !is_model_tool_name(&name) {
                        return Err(BundleError::InvalidManifest {
                            source_name: bundle_id.into(),
                            detail: format!(
                                "expanded MCP tool `{name}` needs a shorter provider-safe server alias"
                            ),
                        });
                    }
                    model_mcp_names.insert(name);
                }
            }
        }
        let schemas = model_tool_names
            .iter()
            .chain(model_mcp_names.iter())
            .map(|public_name| {
                let resolved = tools.get(public_name).ok_or_else(|| {
                    BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: "tool".to_string(),
                        reference: public_name.clone(),
                    }
                })?;
                let mut schema = resolved.tool.schema();
                schema.name = ToolName::new(public_name.clone());
                Ok(schema)
            })
            .collect::<Result<Vec<_>, BundleError>>()?;

        let mut tools = self.with_scheme_dispatch(tools);
        let mut schemas = schemas;
        // Coordination: with the channel family loaded, every agent can read
        // its mail. A view without its own `read` gets a mail-only `read`
        // (never file access); injected after scheme dispatch so no external
        // scheme can reach through it.
        if partitions.tool.contains_key(&harness_id(
            "tool",
            crate::coordination::CHANNEL_MARKER_TOOL,
        )) && !selected.tool.contains(&harness_id("tool", "read"))
            && !tools.contains_key("read")
            && let Some(ResourceCandidate::HarnessTool { resolved, .. }) =
                partitions.tool.get(&harness_id("tool", "read"))
        {
            let channel_read: Arc<dyn Tool> = Arc::new(crate::coordination::ChannelReadTool::new(
                Arc::clone(&resolved.tool),
            ));
            schemas.push(channel_read.schema());
            tools.insert(
                "read".to_string(),
                ResolvedTool {
                    tool: channel_read,
                    permission: resolved.permission,
                },
            );
        }

        let compiled = Arc::new(CompiledResourceView {
            tools,
            schemas,
            skills: Arc::new(skills),
            canonical_hook_ids: Arc::clone(&policy.canonical_hook_ids),
            skill_facade_selected,
        });
        debug_assert_eq!(compiled.canonical_hook_ids(), policy.canonical_hook_ids());
        Ok(compiled)
    }

    /// Install the view's external-scheme dispatch on its read and write tools.
    ///
    /// Only schemes whose owning tool this view actually resolves become
    /// dispatchable — the binding table is intersected with the view's own tool
    /// set, and there is deliberately no registry-wide fallback. A view that
    /// resolves no schemes keeps its tools byte-for-byte unwrapped.
    fn with_scheme_dispatch(
        &self,
        mut tools: BTreeMap<String, ResolvedTool>,
    ) -> BTreeMap<String, ResolvedTool> {
        if self.snapshot.schemes.is_empty() {
            return tools;
        }
        let mut handlers = BTreeMap::new();
        for (scheme, binding) in self.snapshot.schemes.iter() {
            if let Some(resolved) = tools
                .values()
                .find(|resolved| resolved.tool.name() == binding.canonical_tool())
            {
                handlers.insert(
                    scheme.clone(),
                    SchemeHandler::new(binding.clone(), Arc::clone(&resolved.tool)),
                );
            }
        }
        if handlers.is_empty() {
            return tools;
        }
        let dispatch = SchemeDispatch::new(handlers);
        let wrapped = tools
            .into_iter()
            .map(|(public_name, resolved)| {
                let tool: Arc<dyn Tool> = match resolved.tool.name() {
                    "read" => Arc::new(SchemeReadTool::new(
                        Arc::clone(&resolved.tool),
                        dispatch.clone(),
                    )),
                    "write" => Arc::new(SchemeWriteTool::new(
                        Arc::clone(&resolved.tool),
                        dispatch.clone(),
                    )),
                    _ => resolved.tool,
                };
                (
                    public_name,
                    ResolvedTool {
                        tool,
                        permission: resolved.permission,
                    },
                )
            })
            .collect();
        tools = wrapped;
        tools
    }

    #[must_use]
    /// Model-facing tool schemas from this snapshot or view.
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.snapshot.tools.schemas()
    }

    #[must_use]
    /// Resolve a tool name or alias in this compiled view.
    pub fn resolve_tool(&self, name: &str) -> Option<ResolvedTool> {
        self.snapshot.tools.resolve(name)
    }

    #[must_use]
    /// Skill entries visible to this resource view.
    pub fn skills(&self) -> &[SkillCatalogEntry] {
        self.snapshot
            .skills
            .get(&self.place.workdir)
            .map_or(&[], |skills| skills.as_slice())
    }

    #[must_use]
    /// Build a skill plane over this view's skill snapshot.
    pub fn skill_plane(&self) -> SkillPlane {
        let skills = self
            .snapshot
            .skills
            .get(&self.place.workdir)
            .cloned()
            .unwrap_or_default();
        SkillPlane::from_snapshot(skills)
    }
}

impl CompiledResourceView {
    pub(crate) fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }

    pub(crate) fn resolve_tool(&self, name: &str) -> Option<ResolvedTool> {
        self.tools.get(name).cloned()
    }

    pub(crate) fn skills(&self) -> &[SkillCatalogEntry] {
        self.skills.as_slice()
    }

    pub(crate) fn canonical_hook_ids(&self) -> &[String] {
        self.canonical_hook_ids.as_ref()
    }

    /// Prompt skill exposure for the bound agent. When the selected view
    /// includes the harness skill facade (even if aliased), only the on-demand
    /// index is appended; otherwise selected bundle-local static skill bodies
    /// are inlined once with their dispatchable spelling list.
    pub(crate) fn skills_prompt_section(&self) -> Option<String> {
        let skills = self.skills();
        if skills.is_empty() {
            return None;
        }
        if self.skill_facade_selected {
            return skill_index_with_spellings(skills);
        }
        Some(inline_selected_skills_with_spellings(skills))
    }

    pub(crate) fn skill_plane(&self) -> SkillPlane {
        SkillPlane::from_snapshot(self.skills.clone())
    }
}

#[derive(Clone)]
enum ResourceCandidate {
    BundleLocal {
        short_name: String,
        qualified_name: String,
        aliases: Vec<String>,
    },
    BundleLocalSkill {
        short_name: String,
        qualified_name: String,
        entry: SkillCatalogEntry,
        aliases: Vec<String>,
    },
    HarnessTool {
        short_name: String,
        qualified_name: String,
        resolved: ResolvedTool,
        aliases: Vec<String>,
    },
    HarnessSkill {
        short_name: String,
        qualified_name: String,
        entry: SkillCatalogEntry,
        aliases: Vec<String>,
    },
    HarnessMcp {
        short_name: String,
        qualified_name: String,
        resolved: ResolvedTool,
        aliases: Vec<String>,
    },
}

impl ResourceCandidate {
    fn short_name(&self) -> &str {
        match self {
            Self::BundleLocal { short_name, .. }
            | Self::BundleLocalSkill { short_name, .. }
            | Self::HarnessTool { short_name, .. }
            | Self::HarnessSkill { short_name, .. }
            | Self::HarnessMcp { short_name, .. } => short_name,
        }
    }

    fn qualified_name(&self) -> &str {
        match self {
            Self::BundleLocal { qualified_name, .. }
            | Self::BundleLocalSkill { qualified_name, .. }
            | Self::HarnessTool { qualified_name, .. }
            | Self::HarnessSkill { qualified_name, .. }
            | Self::HarnessMcp { qualified_name, .. } => qualified_name,
        }
    }

    fn is_bundle_local(&self) -> bool {
        matches!(
            self,
            Self::BundleLocal { .. } | Self::BundleLocalSkill { .. }
        )
    }

    fn aliases(&self) -> &[String] {
        match self {
            Self::BundleLocal { aliases, .. }
            | Self::BundleLocalSkill { aliases, .. }
            | Self::HarnessTool { aliases, .. }
            | Self::HarnessSkill { aliases, .. }
            | Self::HarnessMcp { aliases, .. } => aliases,
        }
    }
}

struct CandidatePartitions {
    tool: BTreeMap<String, ResourceCandidate>,
    skill: BTreeMap<String, ResourceCandidate>,
    mcp: BTreeMap<String, ResourceCandidate>,
}

struct SelectedIds {
    tool: BTreeSet<String>,
    skill: BTreeSet<String>,
    mcp: BTreeSet<String>,
}

fn harness_id(kind: &str, short: &str) -> String {
    format!("harness:{kind}/{short}")
}

fn namespace_qualified(namespace: &str, kind: &str, local_id: &str) -> String {
    format!("bundle:{namespace}/{kind}/{local_id}")
}

fn collect_bundle_tool_candidates(
    catalog: &BundleCatalog,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Tool) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    for resource in resources {
        out.insert(
            resource.stable_id.clone(),
            ResourceCandidate::BundleLocal {
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn is_harness_source(source: &RuntimeSource, bundles: &BundleCatalog) -> bool {
    source.id.kind() != RuntimeSourceKind::Bundle
        || bundles.bundles().iter().any(|bundle| {
            bundle.identity().id == source.id.configured_id()
                && (bundle.agents().is_empty() && bundle.workflow().is_none())
        })
}

fn collect_harness_tool_candidates(
    plane: AgentToolPlane,
    basic_tools: &ToolRegistrySnapshot,
    full_tools: &ToolRegistrySnapshot,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: &BTreeMap<String, String>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // `basic_tools` is the snapshot captured when the registry was built, before
    // any MCP or plugin publication. That is what makes it the internal public
    // plane: later contributions cannot reach it.
    let selected = match plane {
        AgentToolPlane::InternalPublic => basic_tools,
        AgentToolPlane::Full => full_tools,
    };
    let excluded_names = sources
        .values()
        .flat_map(|source| {
            source
                .exports
                .iter()
                .filter(|export| {
                    export.permission == ToolPermission::Mcp || !is_harness_source(source, bundles)
                })
                .map(|export| export.canonical_name.as_str())
        })
        .collect::<BTreeSet<_>>();
    let mut entries = selected.canonical_tools();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let pool: BTreeSet<String> = entries.iter().map(|(name, _)| name.clone()).collect();
    for (name, resolved) in entries {
        // MCP is an independent resource kind; do not re-home it under tool.
        if excluded_names.contains(name.as_str()) {
            continue;
        }
        // A masked tool is excluded from the view: the mask winner provides
        // the bare name instead. Masks only bite when the winner is visible
        // in this plane's pool, so a plane without the winner keeps the
        // built-in rather than losing the bare name entirely.
        if let Some(winner) = masks.get(name.as_str())
            && winner.as_str() != name.as_str()
            && pool.contains(winner)
        {
            continue;
        }
        // A mask winner is advertised under the bare name it won — wrapped so
        // the schema name is the bare spelling — and that spelling appears
        // exactly once (the qualified spelling is not also advertised).
        let promoted_bare = winner_bare_name(masks, name.as_str());
        let (short_name, resolved) = match promoted_bare {
            Some(bare) => (
                bare.clone(),
                ResolvedTool {
                    tool: Arc::new(NamedTool::new(bare.clone(), resolved.tool)),
                    permission: resolved.permission,
                },
            ),
            None => (name.clone(), resolved),
        };
        let qualified = harness_id("tool", &name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessTool {
                short_name,
                qualified_name: qualified,
                resolved,
                aliases: selected.aliases_for_canonical(&name),
            },
        );
    }
}

/// The bare name `canonical` won through masking, when that differs from the
/// canonical spelling itself. Deterministic: the lexicographically smallest
/// contested name wins the primary spelling when one tool won several.
fn winner_bare_name(masks: &BTreeMap<String, String>, canonical: &str) -> Option<String> {
    masks
        .iter()
        .find(|(bare, winner)| winner.as_str() == canonical && bare.as_str() != canonical)
        .map(|(bare, _)| bare.clone())
}

fn collect_bundle_skill_candidates(
    catalog: &BundleCatalog,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Skill) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    if resources.is_empty() {
        return Ok(());
    }
    let source_id = RuntimeSourceId::bundle(bundle_id);
    let Some(source) = sources.get(&source_id) else {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "prepared Skill contributions were not published".to_string(),
        });
    };
    let prepared = resources
        .iter()
        .map(|resource| (resource.stable_id.as_str(), resource))
        .collect::<BTreeMap<_, _>>();
    if prepared.len() != source.skills.len() {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "published Skill contributions do not match prepared resources".to_string(),
        });
    }
    for skill in &source.skills {
        let Some(resource) = prepared.get(skill.stable_id.as_str()) else {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("published Skill `{}` is not prepared", skill.stable_id),
            });
        };
        if resource.local_id != skill.local_id
            || resource.digest != skill.digest
            || resource.aliases != skill.aliases
        {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "published Skill `{}` differs from prepared bytes",
                    skill.stable_id
                ),
            });
        }
        out.insert(
            skill.stable_id.clone(),
            ResourceCandidate::BundleLocalSkill {
                short_name: skill.local_id.clone(),
                qualified_name: skill.stable_id.clone(),
                entry: skill.entry.clone(),
                aliases: skill.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_skill_candidates(
    plane: AgentToolPlane,
    harness_skills: &[SkillCatalogEntry],
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // Project and user skills are discovered from the working directory. A
    // bundle agent must not see them. Agent-bearing bundle Skills remain
    // owner-scoped; only catalog-confirmed agentless Plugins join the Full plane.
    if plane != AgentToolPlane::Full {
        return;
    }
    for entry in harness_skills {
        let qualified = harness_id("skill", &entry.name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessSkill {
                short_name: entry.name.clone(),
                qualified_name: qualified,
                entry: entry.clone(),
                aliases: Vec::new(),
            },
        );
    }
    for source in sources.values() {
        let agentless_bundle_plugin = source.id.kind() == RuntimeSourceKind::Bundle
            && bundles
                .bundles()
                .iter()
                .find(|bundle| bundle.identity().id == source.id.configured_id())
                .is_some_and(|bundle| bundle.agents().is_empty() && bundle.workflow().is_none());
        if source.id.kind() != RuntimeSourceKind::Plugin && !agentless_bundle_plugin {
            continue;
        }
        for skill in &source.skills {
            if skill.stable_id.starts_with("bundle:") && !agentless_bundle_plugin {
                continue;
            }
            out.insert(
                skill.stable_id.clone(),
                ResourceCandidate::HarnessSkill {
                    short_name: skill.local_id.clone(),
                    qualified_name: skill.stable_id.clone(),
                    entry: skill.entry.clone(),
                    aliases: skill.aliases.clone(),
                },
            );
        }
    }
}

fn collect_bundle_mcp_candidates(
    catalog: &BundleCatalog,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Mcp) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    for resource in resources {
        out.insert(
            resource.stable_id.clone(),
            ResourceCandidate::BundleLocal {
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_mcp_candidates(
    plane: AgentToolPlane,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: &BTreeMap<String, String>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // MCP servers are configured at the Harness level. A bundle agent gets only
    // the MCP declarations its own bundle ships.
    if plane != AgentToolPlane::Full {
        return;
    }
    let mut exports = sources
        .values()
        .filter(|source| is_harness_source(source, bundles))
        .flat_map(|source| source.exports.iter())
        .filter(|export| export.permission == ToolPermission::Mcp)
        .collect::<Vec<_>>();
    exports.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    for export in exports {
        // Contested aliases this export lost are dropped; its qualified
        // canonical spelling remains the escape hatch.
        let mut aliases = export
            .aliases
            .iter()
            .filter(|alias| {
                masks
                    .get(alias.as_str())
                    .is_none_or(|winner| winner.as_str() == export.canonical_name.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        // A mask winner is advertised under the bare name it won, exactly once.
        let (short_name, resolved) = match winner_bare_name(masks, &export.canonical_name) {
            Some(bare) => {
                aliases.retain(|alias| alias != &bare);
                (
                    bare.clone(),
                    ResolvedTool {
                        tool: Arc::new(NamedTool::new(bare.clone(), export.tool.clone())),
                        permission: export.permission,
                    },
                )
            }
            None => (
                export.canonical_name.clone(),
                ResolvedTool {
                    tool: export.tool.clone(),
                    permission: export.permission,
                },
            ),
        };
        let qualified = harness_id("mcp", &export.canonical_name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessMcp {
                short_name,
                qualified_name: qualified,
                resolved,
                aliases,
            },
        );
    }
}

fn select_candidates_globally(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
) -> Result<SelectedIds, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    let view = &policy.resource_view;
    let mut selected = SelectedIds {
        tool: partitions.tool.keys().cloned().collect(),
        skill: partitions.skill.keys().cloned().collect(),
        mcp: partitions.mcp.keys().cloned().collect(),
    };
    if !view.allow.is_empty() {
        let mut allowed = SelectedIds {
            tool: BTreeSet::new(),
            skill: BTreeSet::new(),
            mcp: BTreeSet::new(),
        };
        for reference in &view.allow {
            let hit = resolve_global_reference(policy, reference, partitions)?;
            match hit.kind {
                "tool" => {
                    allowed.tool.insert(hit.canonical);
                }
                "skill" => {
                    allowed.skill.insert(hit.canonical);
                }
                "mcp" => {
                    allowed.mcp.insert(hit.canonical);
                }
                other => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: other.to_string(),
                        reference: reference.clone(),
                    });
                }
            }
        }
        selected.tool = selected.tool.intersection(&allowed.tool).cloned().collect();
        selected.skill = selected
            .skill
            .intersection(&allowed.skill)
            .cloned()
            .collect();
        selected.mcp = selected.mcp.intersection(&allowed.mcp).cloned().collect();
    }
    // Coordination tools are harness-owned: `allow` never narrows them away.
    let coordination = coordination_tool_ids(policy, partitions, &selected);
    selected.tool.extend(coordination);
    for reference in &view.deny {
        let hit = resolve_global_reference(policy, reference, partitions)?;
        match hit.kind {
            "tool" => {
                if let Some(name) = crate::coordination::UNDENIABLE_TOOLS
                    .iter()
                    .find(|name| hit.canonical == harness_id("tool", name))
                {
                    return Err(BundleError::InvalidManifest {
                        source_name: bundle_id.to_string(),
                        detail: format!(
                            "resource_view.deny `{reference}` removes the coordination tool \
                             `{name}`; a subagent without it can never finish (root agents \
                             never see it anyway)"
                        ),
                    });
                }
                selected.tool.remove(&hit.canonical);
            }
            "skill" => {
                selected.skill.remove(&hit.canonical);
            }
            "mcp" => {
                selected.mcp.remove(&hit.canonical);
            }
            other => {
                return Err(BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: other.to_string(),
                    reference: reference.clone(),
                });
            }
        }
    }
    // `task`/`archive` exist only for agents that can spawn someone, however
    // the view reached them (default view or an explicit allow).
    if !policy.spawn_rights {
        for name in crate::coordination::SPAWN_TOOLS {
            selected.tool.remove(&harness_id("tool", name));
        }
    }
    Ok(selected)
}

/// Harness coordination tool ids to inject into `policy`'s view (see
/// [`crate::coordination`]): each one present in the candidate pool, minus the
/// spawn tools without spawn rights, minus any whose bare name the view already
/// gives to one of its own selected resources or aliases (the bundle's own
/// resource keeps the name; nothing collides).
fn coordination_tool_ids(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
    selected: &SelectedIds,
) -> Vec<String> {
    crate::coordination::COORDINATION_TOOLS
        .iter()
        .filter(|name| policy.spawn_rights || !crate::coordination::SPAWN_TOOLS.contains(name))
        .filter_map(|name| {
            let id = harness_id("tool", name);
            if !partitions.tool.contains_key(&id) || selected.tool.contains(&id) {
                return None;
            }
            let claimed_by_view = policy.resource_view.aliases.contains_key(*name)
                || [
                    (&partitions.tool, &selected.tool),
                    (&partitions.mcp, &selected.mcp),
                ]
                .into_iter()
                .flat_map(|(candidates, chosen)| {
                    chosen.iter().filter_map(|chosen| candidates.get(chosen))
                })
                .any(|candidate| {
                    candidate.short_name() == *name
                        || candidate.aliases().iter().any(|alias| alias == name)
                });
            (!claimed_by_view).then_some(id)
        })
        .collect()
}

struct ResolvedReference {
    kind: &'static str,
    canonical: String,
}

fn resolve_global_reference(
    policy: &AgentResourcePolicy,
    reference: &str,
    partitions: &CandidatePartitions,
) -> Result<ResolvedReference, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    if reference.starts_with("harness:") {
        let kind = kind_from_qualified_reference(reference).ok_or_else(|| {
            BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: "resource".to_string(),
                reference: reference.to_string(),
            }
        })?;
        if !matches!(kind, "tool" | "skill" | "mcp") {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: reference.to_string(),
            });
        }
        // The clamped plane admits no Harness skills and no Harness MCP. Say so
        // plainly instead of reporting the reference as unknown.
        if policy.plane == AgentToolPlane::InternalPublic && matches!(kind, "skill" | "mcp") {
            return Err(BundleError::ResourceNotInPlane {
                bundle_id: bundle_id.to_string(),
                reference: reference.to_string(),
                plane: policy.plane.as_str().to_string(),
            });
        }
        let candidates = partition_for(kind, partitions);
        if candidates.contains_key(reference) {
            return Ok(ResolvedReference {
                kind,
                canonical: reference.to_string(),
            });
        }
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: kind.to_string(),
            reference: reference.to_string(),
        });
    }
    if reference.starts_with("bundle:") {
        let kind = kind_from_qualified_reference(reference).ok_or_else(|| {
            BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: "resource".to_string(),
                reference: reference.to_string(),
            }
        })?;
        if !matches!(kind, "tool" | "skill" | "mcp") {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: reference.to_string(),
            });
        }
        let candidates = partition_for(kind, partitions);
        if candidates.contains_key(reference) {
            return Ok(ResolvedReference {
                kind,
                canonical: reference.to_string(),
            });
        }
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: kind.to_string(),
            reference: reference.to_string(),
        });
    }

    let mut matches = Vec::new();
    for (kind, candidates) in [
        ("tool", &partitions.tool),
        ("skill", &partitions.skill),
        ("mcp", &partitions.mcp),
    ] {
        for (id, candidate) in candidates {
            if candidate.short_name() == reference {
                matches.push((kind, id.clone()));
            }
        }
    }
    match matches.as_slice() {
        [] => Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "resource".to_string(),
            reference: reference.to_string(),
        }),
        [(kind, canonical)] => Ok(ResolvedReference {
            kind,
            canonical: canonical.clone(),
        }),
        _ => Err(BundleError::NamespaceCollision {
            bundle_id: bundle_id.to_string(),
            name: reference.to_string(),
        }),
    }
}

/// Extract the resource kind from a qualified harness or bundle reference.
/// Bundle IDs may themselves contain kind-like path segments, so parse
/// structurally from the rightmost `/{kind}/{local}` pair.
fn kind_from_qualified_reference(reference: &str) -> Option<&'static str> {
    if let Some(rest) = reference.strip_prefix("harness:") {
        let kind = rest.split_once('/')?.0;
        return normalize_kind(kind);
    }
    if let Some(rest) = reference.strip_prefix("bundle:") {
        // bundle:{bundle_id}/{kind}/{local_id} — split from the right twice.
        let mut parts = rest.rsplitn(3, '/');
        let local = parts.next()?;
        let kind = parts.next()?;
        let bundle_id = parts.next()?;
        if local.is_empty() || kind.is_empty() || bundle_id.is_empty() {
            return None;
        }
        return normalize_kind(kind);
    }
    None
}

fn normalize_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "tool" => "tool",
        "skill" => "skill",
        "mcp" => "mcp",
        "hook" => "hook",
        "agent" => "agent",
        _ => return None,
    })
}

fn partition_for<'a>(
    kind: &str,
    partitions: &'a CandidatePartitions,
) -> &'a BTreeMap<String, ResourceCandidate> {
    match kind {
        "skill" => &partitions.skill,
        "mcp" => &partitions.mcp,
        _ => &partitions.tool,
    }
}

/// Resolve every resource-view alias target once against all partitions.
fn resolve_view_aliases(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
    selected: &SelectedIds,
    view: &ResourceView,
) -> Result<Vec<(String, &'static str, String)>, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    let mut out = Vec::new();
    for (alias, target) in &view.aliases {
        let hit = resolve_global_reference(policy, target, partitions)?;
        let selected_set = match hit.kind {
            "tool" => &selected.tool,
            "skill" => &selected.skill,
            "mcp" => &selected.mcp,
            other => {
                return Err(BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: other.to_string(),
                    reference: target.clone(),
                });
            }
        };
        if !selected_set.contains(&hit.canonical) {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: hit.kind.to_string(),
                reference: target.clone(),
            });
        }
        out.push((alias.clone(), hit.kind, hit.canonical));
    }
    Ok(out)
}

fn aliases_for_kind(
    kind: &str,
    aliases: &[(String, &'static str, String)],
) -> BTreeMap<String, String> {
    aliases
        .iter()
        .filter(|(_, alias_kind, _)| *alias_kind == kind)
        .map(|(alias, _, canonical)| (alias.clone(), canonical.clone()))
        .collect()
}

fn reserve_stable_names(
    namespace: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    sibling_dispatch: Option<&BTreeMap<String, ResourceCandidate>>,
) -> BTreeSet<String> {
    let mut reserved = BTreeSet::new();
    for candidate in candidates.values() {
        reserved.insert(candidate.short_name().to_string());
        reserved.insert(candidate.qualified_name().to_string());
        if candidate.is_bundle_local() {
            reserved.insert(namespace_qualified(namespace, kind, candidate.short_name()));
        }
    }
    if let Some(sibling) = sibling_dispatch {
        for candidate in sibling.values() {
            reserved.insert(candidate.short_name().to_string());
            reserved.insert(candidate.qualified_name().to_string());
        }
    }
    reserved
}

/// Select provider-safe canonical short names and explicit view aliases for schemas.
/// Qualified identities and candidate aliases remain dispatch-only spellings.
fn model_schema_public_names(
    bundle_id: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    public: &BTreeMap<String, String>,
    view_aliases: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>, BundleError> {
    let mut names = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for (public_name, canonical_id) in public {
        let candidate =
            candidates
                .get(canonical_id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: canonical_id.clone(),
                })?;
        let explicit_alias = view_aliases.get(public_name) == Some(canonical_id);
        if public_name != candidate.short_name() && !explicit_alias {
            continue;
        }
        if !is_model_tool_name(public_name) {
            if explicit_alias {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "model-facing {kind} name `{public_name}` must be 1-64 ASCII letters, digits, `_`, or `-`"
                    ),
                });
            }
            continue;
        }
        names.insert(public_name.clone());
        covered.insert(canonical_id.clone());
    }
    for canonical_id in public.values() {
        if covered.contains(canonical_id) {
            continue;
        }
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!(
                "selected {kind} `{canonical_id}` has no provider-safe schema name; add an explicit alias using 1-64 ASCII letters, digits, `_`, or `-`"
            ),
        });
    }
    Ok(names)
}

/// Return whether a name satisfies the common provider Tool-name contract.
fn is_model_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn assign_public_names(
    bundle_id: &str,
    namespace: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    selected: &BTreeSet<String>,
    view_aliases: &BTreeMap<String, String>,
    sibling_dispatch: Option<&BTreeMap<String, ResourceCandidate>>,
) -> Result<BTreeMap<String, String>, BundleError> {
    let mut short_owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        short_owners
            .entry(candidate.short_name().to_string())
            .or_default()
            .push(id.clone());
    }

    let mut reserved = reserve_stable_names(namespace, kind, candidates, sibling_dispatch);

    let mut ordinary_short: BTreeMap<String, String> = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        let short = candidate.short_name();
        let owners = short_owners.get(short).map_or(&[][..], Vec::as_slice);
        let gets_short = if owners.len() == 1 {
            true
        } else {
            let local = owners
                .iter()
                .filter_map(|owner| candidates.get(owner))
                .filter(|candidate| candidate.is_bundle_local())
                .count();
            let harness = owners.len() - local;
            if local > 1 || harness > 1 {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: short.to_string(),
                });
            }
            candidate.is_bundle_local()
        };
        if gets_short {
            ordinary_short.insert(id.clone(), short.to_string());
        }
    }

    let mut public = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        if let Some(short) = ordinary_short.get(id)
            && public.insert(short.clone(), id.clone()).is_some()
        {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: short.clone(),
            });
        }
        let qualified_public = if candidate.is_bundle_local() {
            namespace_qualified(namespace, kind, candidate.short_name())
        } else {
            candidate.qualified_name().to_string()
        };
        if public
            .insert(qualified_public.clone(), id.clone())
            .is_some_and(|existing| existing != *id)
        {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: qualified_public,
            });
        }
    }

    for (alias, canonical) in view_aliases {
        if !selected.contains(canonical) {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: canonical.clone(),
            });
        }
        if reserved.contains(alias) {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        // Kind-local: explicit aliases collide with preexisting candidate
        // aliases (registry/source/prepared), even for the same target.
        if candidates
            .values()
            .any(|candidate| candidate.aliases().iter().any(|existing| existing == alias))
        {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        if public.get(alias).is_some_and(|id| id != canonical) {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        if let Some(short) = ordinary_short.get(canonical)
            && public.get(short).is_some_and(|id| id == canonical)
        {
            public.remove(short);
        }
        public.insert(alias.clone(), canonical.clone());
        reserved.insert(alias.clone());
    }

    Ok(public)
}

/// Project catalog/snapshot/source aliases into the compiled public map for
/// selected candidates. These are additive public spellings, not allow/deny
/// identities. When a canonical identity has one or more explicit resource-view
/// aliases, all of its candidate effective aliases are suppressed so prior
/// short/registry aliases cannot bypass the mapping.
fn inject_effective_aliases(
    bundle_id: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    selected: &BTreeSet<String>,
    view_aliases: &BTreeMap<String, String>,
    public: &mut BTreeMap<String, String>,
) -> Result<(), BundleError> {
    let explicitly_aliased = view_aliases.values().cloned().collect::<BTreeSet<_>>();
    for id in selected {
        if explicitly_aliased.contains(id) {
            continue;
        }
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        for alias in candidate.aliases() {
            if let Some(existing) = public.get(alias) {
                if existing != id {
                    return Err(BundleError::AliasCollision {
                        bundle_id: bundle_id.to_string(),
                        name: alias.clone(),
                    });
                }
                continue;
            }
            public.insert(alias.clone(), id.clone());
        }
    }
    Ok(())
}

use super::prompt::*;

#[cfg(test)]
mod tests;
