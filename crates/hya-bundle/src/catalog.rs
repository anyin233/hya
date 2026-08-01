use std::collections::{BTreeMap, BTreeSet};

use crate::{BundleError, PreparedAgent, PreparedBundle, PreparedResource};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExportKind {
    Tool,
    Skill,
    Mcp,
    Hook,
    Extension,
}

impl ExportKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Skill => "skill",
            Self::Mcp => "mcp",
            Self::Hook => "hook",
            Self::Extension => "extension",
        }
    }
}

/// Pure immutable catalog built only from already-prepared Bundle data.
#[derive(Debug)]
pub struct BundleCatalog {
    bundles: Vec<PreparedBundle>,
    agents: BTreeMap<String, (usize, usize)>,
    resources: BTreeMap<(ExportKind, String), (usize, usize)>,
    local_resources: BTreeMap<(String, ExportKind, String), String>,
}

impl BundleCatalog {
    pub fn from_prepared(bundles: &[PreparedBundle]) -> Result<Self, BundleError> {
        if bundles.is_empty() {
            return Err(BundleError::EmptyPreparedCatalog);
        }
        let mut catalog = Self {
            bundles: bundles.to_vec(),
            agents: BTreeMap::new(),
            resources: BTreeMap::new(),
            local_resources: BTreeMap::new(),
        };
        let mut stable_agent_ids = BTreeSet::new();
        for (bundle_index, bundle) in catalog.bundles.iter().enumerate() {
            for (agent_index, agent) in bundle.agents.iter().enumerate() {
                if !stable_agent_ids.insert(agent.stable_id.as_str()) {
                    return Err(BundleError::DuplicateStableAgentId {
                        stable_id: agent.stable_id.as_str().to_string(),
                    });
                }
                for reference in [
                    agent.stable_id.as_str().to_string(),
                    format!("bundle:{}/agent/{}", bundle.identity.id, agent.local_id),
                ] {
                    if catalog
                        .agents
                        .insert(reference.clone(), (bundle_index, agent_index))
                        .is_some()
                    {
                        return Err(BundleError::NamespaceCollision {
                            bundle_id: bundle.identity.id.clone(),
                            name: reference,
                        });
                    }
                }
            }
            for kind in [
                ExportKind::Tool,
                ExportKind::Skill,
                ExportKind::Mcp,
                ExportKind::Hook,
                ExportKind::Extension,
            ] {
                for (resource_index, resource) in resources(bundle, kind).iter().enumerate() {
                    let qualified = resource.stable_id.clone();
                    if catalog
                        .resources
                        .insert((kind, qualified.clone()), (bundle_index, resource_index))
                        .is_some()
                    {
                        return Err(BundleError::NamespaceCollision {
                            bundle_id: bundle.identity.id.clone(),
                            name: qualified,
                        });
                    }
                    for name in std::iter::once(resource.local_id.as_str())
                        .chain(resource.aliases.iter().map(String::as_str))
                    {
                        let key = (bundle.identity.id.clone(), kind, name.to_string());
                        if catalog
                            .local_resources
                            .insert(key, resource.stable_id.clone())
                            .is_some()
                        {
                            return Err(BundleError::AliasCollision {
                                bundle_id: bundle.identity.id.clone(),
                                name: name.to_string(),
                            });
                        }
                    }
                }
            }
        }
        Ok(catalog)
    }

    #[must_use]
    pub fn bundles(&self) -> &[PreparedBundle] {
        &self.bundles
    }

    #[must_use]
    pub fn resolve_agent(&self, reference: &str) -> Option<&PreparedAgent> {
        self.resolve_agent_entry(reference).map(|(_, agent)| agent)
    }

    #[must_use]
    pub fn resolve_agent_entry(&self, reference: &str) -> Option<(&str, &PreparedAgent)> {
        let (bundle, agent) = *self.agents.get(reference)?;
        let bundle = self.bundles.get(bundle)?;
        Some((bundle.identity.id.as_str(), bundle.agents.get(agent)?))
    }

    #[must_use]
    pub fn bundle_resources(
        &self,
        bundle_id: &str,
        kind: ExportKind,
    ) -> Option<&[PreparedResource]> {
        self.bundles
            .iter()
            .find(|bundle| bundle.identity.id == bundle_id)
            .map(|bundle| resources(bundle, kind))
    }

    /// Resolve one ordinary agent-to-agent spawn against the caller's compiled
    /// reachability graph. Exact catalog lookup remains available separately for
    /// the Harness-owned system operations.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<&PreparedAgent, BundleError> {
        let caller_agent =
            self.resolve_agent(caller)
                .ok_or_else(|| BundleError::UnknownAgentId {
                    agent_id: caller.to_string(),
                })?;
        let requested_agent =
            self.resolve_agent(requested)
                .ok_or_else(|| BundleError::UnknownAgentId {
                    agent_id: requested.to_string(),
                })?;
        if !caller_agent
            .can_spawn
            .iter()
            .any(|allowed| allowed == &requested_agent.stable_id)
        {
            return Err(BundleError::AgentSpawnNotAllowed {
                caller: caller_agent.stable_id.as_str().to_string(),
                agent_id: requested_agent.stable_id.as_str().to_string(),
            });
        }
        Ok(requested_agent)
    }

    pub fn spawnable_agents(&self, caller: &str) -> Result<Vec<&PreparedAgent>, BundleError> {
        let caller_agent =
            self.resolve_agent(caller)
                .ok_or_else(|| BundleError::UnknownAgentId {
                    agent_id: caller.to_string(),
                })?;
        caller_agent
            .can_spawn
            .iter()
            .map(|stable_id| {
                self.resolve_agent(stable_id.as_str())
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: stable_id.as_str().to_string(),
                    })
            })
            .collect()
    }

    pub fn resolve_resource(
        &self,
        bundle_id: &str,
        kind: ExportKind,
        reference: &str,
    ) -> Result<&PreparedResource, BundleError> {
        let qualified = if reference.starts_with("bundle:") {
            reference
        } else {
            let Some(qualified) =
                self.local_resources
                    .get(&(bundle_id.to_string(), kind, reference.to_string()))
            else {
                return Err(unknown_resource(bundle_id, kind, reference));
            };
            qualified
        };
        let Some((bundle, resource)) = self.resources.get(&(kind, qualified.to_string())) else {
            return Err(unknown_resource(bundle_id, kind, reference));
        };
        self.bundles
            .get(*bundle)
            .and_then(|bundle| resources(bundle, kind).get(*resource))
            .ok_or_else(|| unknown_resource(bundle_id, kind, reference))
    }
}

fn resources(bundle: &PreparedBundle, kind: ExportKind) -> &[PreparedResource] {
    match kind {
        ExportKind::Tool => &bundle.tools,
        ExportKind::Skill => &bundle.skills,
        ExportKind::Mcp => &bundle.mcp,
        ExportKind::Hook => &bundle.hooks,
        ExportKind::Extension => &bundle.extensions,
    }
}

fn unknown_resource(bundle_id: &str, kind: ExportKind, reference: &str) -> BundleError {
    BundleError::UnknownResourceReference {
        bundle_id: bundle_id.to_string(),
        kind: kind.as_str().to_string(),
        reference: reference.to_string(),
    }
}
