use super::*;

impl TurnBinding {
    pub(in crate::runtime_registry::compile) fn collect_resource_candidates(
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
}

#[derive(Clone)]
pub(in crate::runtime_registry::compile) enum ResourceCandidate {
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
    pub(in crate::runtime_registry::compile) fn short_name(&self) -> &str {
        match self {
            Self::BundleLocal { short_name, .. }
            | Self::BundleLocalSkill { short_name, .. }
            | Self::HarnessTool { short_name, .. }
            | Self::HarnessSkill { short_name, .. }
            | Self::HarnessMcp { short_name, .. } => short_name,
        }
    }

    pub(in crate::runtime_registry::compile) fn qualified_name(&self) -> &str {
        match self {
            Self::BundleLocal { qualified_name, .. }
            | Self::BundleLocalSkill { qualified_name, .. }
            | Self::HarnessTool { qualified_name, .. }
            | Self::HarnessSkill { qualified_name, .. }
            | Self::HarnessMcp { qualified_name, .. } => qualified_name,
        }
    }

    pub(in crate::runtime_registry::compile) fn is_bundle_local(&self) -> bool {
        matches!(
            self,
            Self::BundleLocal { .. } | Self::BundleLocalSkill { .. }
        )
    }

    pub(in crate::runtime_registry::compile) fn aliases(&self) -> &[String] {
        match self {
            Self::BundleLocal { aliases, .. }
            | Self::BundleLocalSkill { aliases, .. }
            | Self::HarnessTool { aliases, .. }
            | Self::HarnessSkill { aliases, .. }
            | Self::HarnessMcp { aliases, .. } => aliases,
        }
    }
}

pub(in crate::runtime_registry::compile) struct CandidatePartitions {
    pub(in crate::runtime_registry::compile) tool: BTreeMap<String, ResourceCandidate>,
    pub(in crate::runtime_registry::compile) skill: BTreeMap<String, ResourceCandidate>,
    pub(in crate::runtime_registry::compile) mcp: BTreeMap<String, ResourceCandidate>,
}

pub(in crate::runtime_registry::compile) struct SelectedIds {
    pub(in crate::runtime_registry::compile) tool: BTreeSet<String>,
    pub(in crate::runtime_registry::compile) skill: BTreeSet<String>,
    pub(in crate::runtime_registry::compile) mcp: BTreeSet<String>,
}

pub(in crate::runtime_registry::compile) fn harness_id(kind: &str, short: &str) -> String {
    format!("harness:{kind}/{short}")
}

pub(in crate::runtime_registry::compile) fn namespace_qualified(
    namespace: &str,
    kind: &str,
    local_id: &str,
) -> String {
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

pub(in crate::runtime_registry::compile) fn select_candidates_globally(
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

pub(in crate::runtime_registry::compile) struct ResolvedReference {
    pub(in crate::runtime_registry::compile) kind: &'static str,
    pub(in crate::runtime_registry::compile) canonical: String,
}

pub(in crate::runtime_registry::compile) fn resolve_global_reference(
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
pub(in crate::runtime_registry::compile) fn kind_from_qualified_reference(
    reference: &str,
) -> Option<&'static str> {
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
