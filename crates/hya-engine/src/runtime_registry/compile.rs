use super::*;

mod integrations;
mod resources;

pub(crate) use integrations::BoundAgentHook;
pub(crate) use resources::CompiledResourceView;
pub use resources::{AgentResourcePolicy, AgentToolPlane};

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

/// Where a [`TurnBinding`] was bound: one `Arc` keeps the binding small.
pub(super) struct BindingPlace {
    pub(super) workdir: PathBuf,
    pub(super) scope: CatalogScope,
    pub(super) project_bundle_dirs: Arc<BTreeMap<String, PathBuf>>,
    pub(super) bundle_efforts: Arc<BTreeMap<String, BTreeMap<String, ReasoningEffort>>>,
    pub(super) resource_selections: resources::ResourceSelections,
}

#[cfg(test)]
impl BindingPlace {
    fn new(workdir: PathBuf, scope: CatalogScope) -> Arc<Self> {
        Arc::new(Self {
            workdir,
            scope,
            project_bundle_dirs: Arc::new(BTreeMap::new()),
            bundle_efforts: Arc::new(BTreeMap::new()),
            resource_selections: Default::default(),
        })
    }
}

impl TurnBinding {
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

    /// Resolve a workflow node or verifier using workflow authorization.
    pub fn resolve_workflow_agent(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let definition = self
            .snapshot
            .catalog
            .resolve_workflow_agent(caller, requested)?;
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

    /// Handoff policy declared by `stable_id`; built-ins use their preset's.
    pub fn handoff_policy(&self, stable_id: &str) -> Result<HandoffPolicy, BundleError> {
        let definition = self.snapshot.catalog.require(stable_id)?;
        match definition.origin.bundle_id() {
            None => Ok(crate::builtin_agents::preset_agent(stable_id)
                .map(|agent| agent.handoff)
                .unwrap_or_default()),
            Some(id) => self
                .snapshot
                .catalog
                .bundles()
                .resolve_agent_entry(stable_id)
                .map(|(_, agent)| agent.handoff)
                .ok_or_else(|| BundleError::UnknownAgentId {
                    agent_id: format!("{id}/{stable_id}"),
                }),
        }
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

#[cfg(test)]
mod tests;
