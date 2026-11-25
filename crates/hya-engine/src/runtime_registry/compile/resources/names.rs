use super::*;

/// Resolve every resource-view alias target once against all partitions.
pub(super) fn resolve_view_aliases(
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

pub(super) fn aliases_for_kind(
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
pub(super) fn model_schema_public_names(
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
        // The kernel loop handles calls of these names itself.
        if [crate::kernel::tools::REWRITE, crate::kernel::tools::TODO]
            .contains(&public_name.as_str())
        {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "{kind} name `{public_name}` is reserved for the kernel's own tool"
                ),
            });
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
pub(super) fn is_model_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(super) fn assign_public_names(
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
pub(super) fn inject_effective_aliases(
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
