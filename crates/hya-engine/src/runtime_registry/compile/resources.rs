use super::*;

mod candidates;
mod names;

use super::super::prompt::*;
use candidates::*;
use names::*;

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
/// It is never persisted or exposed on the wire.
#[derive(Clone, Debug)]
pub struct AgentResourcePolicy {
    agent_id: String,
    /// Owning bundle id, or `None` for a built-in agent (which owns no bundle).
    bundle_id: Option<String>,
    plane: AgentToolPlane,
    // Keep policies compact inside turn and resident activation enums.
    pub(super) resource_view: Arc<ResourceView>,
    advertise: BTreeSet<String>,
    /// Whether the agent may spawn anyone (a non-empty effective spawn set);
    /// decides the `task`/`archive` coordination tools.
    spawn_rights: bool,
    selected_bundle_tool_ids: Arc<Vec<String>>,
    selected_bundle_skill_ids: Arc<Vec<String>>,
    pub(super) canonical_hook_ids: Arc<[String]>,
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

#[derive(Default)]
pub(in crate::runtime_registry) struct ResourceSelections {
    agents: Mutex<BTreeMap<String, Arc<SelectedResources>>>,
}

struct SelectedResources {
    policy: AgentResourcePolicy,
    partitions: CandidatePartitions,
    ids: SelectedIds,
}

/// Immutable per-turn/child resource map compiled once from a retained
/// [`TurnBinding`] and bound agent policy. Schema visibility, skill exposure,
/// and dispatch share this map; there is no registry fallback.
pub(crate) struct CompiledResourceView {
    pub(super) tools: BTreeMap<String, ResolvedTool>,
    schemas: Vec<ToolSchema>,
    advertise: BTreeSet<String>,
    skills: Arc<Vec<SkillCatalogEntry>>,
    canonical_hook_ids: Arc<[String]>,
    /// Whether the selected view includes the canonical harness skill facade
    /// tool (regardless of any public alias spelling for that tool).
    skill_facade_selected: bool,
}

impl TurnBinding {
    /// Compile the agent resource/tool policy for `stable_id`.
    pub fn agent_resource_policy(
        &self,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let mut selections = self
            .place
            .resource_selections
            .agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(selection) = selections.get(stable_id) {
            return Ok(selection.policy.clone());
        }
        let definition = self.snapshot.catalog.require(stable_id)?;
        let plane = AgentToolPlane::for_origin(&definition.origin);
        let bundle_id = definition.origin.bundle_id().map(str::to_string);
        // Built-ins own no bundle: their view comes from the core-agents
        // preset (read-only agents deny the write tools), and they carry no hooks.
        let (resource_view, hook_refs, advertise) = match bundle_id.as_deref() {
            None => self
                .snapshot
                .catalog
                .preset_agent(stable_id)
                .map(|agent| {
                    (
                        agent.resource_view.clone(),
                        Vec::new(),
                        agent.advertise.clone(),
                    )
                })
                .unwrap_or_default(),
            Some(id) => {
                let (_, agent) = self
                    .snapshot
                    .catalog
                    .bundles()
                    .resolve_agent_entry(stable_id)
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: format!("{id}/{stable_id}"),
                    })?;
                (
                    agent.resource_view.clone(),
                    agent.hook_refs.clone(),
                    agent.advertise.clone(),
                )
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
            agent_id: stable_id.to_string(),
            bundle_id,
            plane,
            resource_view: Arc::new(resource_view),
            advertise: advertise.into_iter().collect(),
            spawn_rights,
            selected_bundle_tool_ids: Arc::new(Vec::new()),
            selected_bundle_skill_ids: Arc::new(Vec::new()),
            canonical_hook_ids: Arc::from(hook_refs),
        };
        let (partitions, selected) = self.select_resources(&policy)?;
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
        selections.insert(
            stable_id.to_string(),
            Arc::new(SelectedResources {
                policy: policy.clone(),
                partitions,
                ids: selected,
            }),
        );
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
        let (resource_view, hook_refs, advertise) = match bundle_id.as_deref() {
            None => self
                .snapshot
                .catalog
                .preset_agent(stable_id)
                .map(|agent| {
                    (
                        agent.resource_view.clone(),
                        Vec::new(),
                        agent.advertise.clone(),
                    )
                })
                .unwrap_or_default(),
            Some(_) => {
                let (_, agent) = self
                    .snapshot
                    .catalog
                    .bundles()
                    .resolve_agent_entry(stable_id)
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: stable_id.to_string(),
                    })?;
                (
                    agent.resource_view.clone(),
                    agent.hook_refs.clone(),
                    agent.advertise.clone(),
                )
            }
        };
        Ok(AgentResourcePolicy {
            agent_id: stable_id.to_string(),
            bundle_id,
            plane,
            resource_view: Arc::new(resource_view),
            advertise: advertise.into_iter().collect(),
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
        if policy.bundle_id.as_deref().is_some_and(|id| {
            self.snapshot.catalog.bundles().bundles().iter().any(|b| {
                b.identity().id == id
                    && b.runtime()
                        .is_some_and(|rt| rt.kind == hya_bundle::PreparedRuntimeKind::Native)
            })
        }) {
            // Native bundles load in-process via ABI v2 rather than a JS sidecar.
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
        Ok(
            !policy.selected_bundle_tool_ids().is_empty()
                || !policy.canonical_hook_ids().is_empty(),
        )
    }

    fn select_resources(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<(CandidatePartitions, SelectedIds), BundleError> {
        let partitions = self.collect_resource_candidates(policy)?;
        let selected = select_candidates_globally(policy, &partitions)?;
        Ok((partitions, selected))
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

        let cached = self
            .place
            .resource_selections
            .agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&policy.agent_id)
            .filter(|selection| Arc::ptr_eq(&selection.policy.resource_view, &policy.resource_view))
            .cloned();
        // A caller can supply a policy captured by a different binding.
        let fresh;
        let (partitions, selected) = match cached.as_deref() {
            Some(selection) => (&selection.partitions, &selection.ids),
            None => {
                fresh = self.select_resources(policy)?;
                (&fresh.0, &fresh.1)
            }
        };
        let view_aliases = resolve_view_aliases(policy, partitions, selected, view)?;
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

        for name in &policy.advertise {
            if !tools.contains_key(name) {
                return Err(BundleError::InvalidManifest {
                    source_name: policy.diagnostic_scope().to_string(),
                    detail: format!(
                        "agent `{}` advertise: tool `{name}` is not in its compiled view",
                        policy.agent_id
                    ),
                });
            }
        }

        let compiled = Arc::new(CompiledResourceView {
            tools,
            schemas,
            advertise: policy.advertise.clone(),
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
}

impl CompiledResourceView {
    pub(crate) fn advertises(&self, name: &str) -> bool {
        self.advertise.is_empty() || self.advertise.contains(name)
    }

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

    /// Skills from directory discovery, including builtins that discovery did
    /// not shadow. Captured once as part of the session's initial context.
    /// A `disable-model-invocation: true` skill is left out: only
    /// `/skill:<name>` runs it.
    pub(crate) fn directory_skills_prompt_section(&self) -> Option<String> {
        skill_index_with_spellings(
            "Skills found at session start; use the skill tool to load a current skill:",
            self.skills().iter().filter(|entry| {
                entry.origin != hya_tool::SkillCatalogOrigin::Virtual
                    && !entry.disable_model_invocation
            }),
        )
    }

    /// Live skill guidance from selected bundle and plugin resources. With the
    /// skill facade these are listed, otherwise bundle-local bodies are inlined.
    /// A `disable-model-invocation: true` skill is neither listed nor inlined.
    pub(crate) fn skills_prompt_section(&self) -> Option<String> {
        let skills = self
            .skills()
            .iter()
            .filter(|entry| !entry.disable_model_invocation)
            .cloned()
            .collect::<Vec<_>>();
        if skills.is_empty() {
            return None;
        }
        if self.skill_facade_selected {
            return skill_index_with_spellings(
                "These skills are available on demand; read the named SKILL.md when relevant:",
                skills
                    .iter()
                    .filter(|entry| entry.origin == hya_tool::SkillCatalogOrigin::Virtual),
            );
        }
        Some(inline_selected_skills_with_spellings(&skills))
    }

    pub(crate) fn skill_plane(&self) -> SkillPlane {
        SkillPlane::from_snapshot(self.skills.clone())
    }
}
