use std::collections::{BTreeMap, BTreeSet};

use crate::{
    BundleError, BundleOrigin, PreparedAgent, PreparedBundle, PreparedCatalog, PreparedResource,
    prepare::validate_hook_local_id,
};

const SEMANTIC_IDENTITY_DOMAIN_V1: &[u8] = b"hya.bundle-catalog.semantic-identity/v1";

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
    semantic_identity_v1: Option<Vec<u8>>,
    verified_catalog_records: Option<Vec<Vec<u8>>>,
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
            semantic_identity_v1: None,
            verified_catalog_records: None,
        };
        let mut bundle_ids = BTreeSet::new();
        let mut stable_agent_ids = BTreeSet::new();
        for (bundle_index, bundle) in catalog.bundles.iter().enumerate() {
            if !bundle.mcp.is_empty() {
                return Err(BundleError::UnsupportedBundleFeature {
                    bundle_id: bundle.identity.id.clone(),
                    feature: "resources.mcp".to_string(),
                });
            }
            for hook in &bundle.hooks {
                validate_hook_local_id(&bundle.identity.id, &hook.local_id)?;
            }
            if !bundle_ids.insert(bundle.identity.id.as_str()) {
                return Err(BundleError::DuplicateBundleId {
                    bundle_id: bundle.identity.id.clone(),
                });
            }
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

    pub fn from_verified_catalogs(catalogs: &[&PreparedCatalog]) -> Result<Self, BundleError> {
        let bundles = catalogs
            .iter()
            .flat_map(|catalog| catalog.bundles.iter().cloned())
            .collect::<Vec<_>>();
        let mut catalog = Self::from_prepared(&bundles)?;
        let records = encode_verified_catalog_records(catalogs)?;
        catalog.semantic_identity_v1 = Some(encode_semantic_identity_v1(&records)?);
        catalog.verified_catalog_records = Some(records);
        Ok(catalog)
    }

    pub fn with_verified_catalogs(
        &self,
        catalogs: &[&PreparedCatalog],
    ) -> Result<Self, BundleError> {
        let Some(existing_records) = self.verified_catalog_records.as_ref() else {
            return Err(BundleError::PreparedEncode {
                detail: "verified catalog provenance unavailable".to_string(),
            });
        };
        let bundles = self
            .bundles
            .iter()
            .cloned()
            .chain(
                catalogs
                    .iter()
                    .flat_map(|catalog| catalog.bundles.iter().cloned()),
            )
            .collect::<Vec<_>>();
        let mut merged = Self::from_prepared(&bundles)?;
        let mut records = existing_records.clone();
        records.extend(encode_verified_catalog_records(catalogs)?);
        records.sort();
        merged.semantic_identity_v1 = Some(encode_semantic_identity_v1(&records)?);
        merged.verified_catalog_records = Some(records);
        Ok(merged)
    }

    #[must_use]
    pub fn semantic_identity_v1(&self) -> Option<&[u8]> {
        self.semantic_identity_v1.as_deref()
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
        self.resolve_resource_entry(bundle_id, kind, reference)
            .map(|(_, resource)| resource)
    }

    pub fn resolve_resource_entry(
        &self,
        bundle_id: &str,
        kind: ExportKind,
        reference: &str,
    ) -> Result<(&str, &PreparedResource), BundleError> {
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
        let owner = self
            .bundles
            .get(*bundle)
            .ok_or_else(|| unknown_resource(bundle_id, kind, reference))?;
        let resource = resources(owner, kind)
            .get(*resource)
            .ok_or_else(|| unknown_resource(bundle_id, kind, reference))?;
        Ok((owner.identity.id.as_str(), resource))
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

fn encode_verified_catalog_records(
    catalogs: &[&PreparedCatalog],
) -> Result<Vec<Vec<u8>>, BundleError> {
    let mut records = Vec::with_capacity(catalogs.len());
    for catalog in catalogs {
        let mut record = Vec::new();
        append_bytes(&mut record, catalog.digest.as_bytes())?;

        let mut bundles = catalog.bundles.iter().collect::<Vec<_>>();
        bundles.sort_by(|left, right| compare_bundle_identity(left, right));
        append_count(&mut record, bundles.len())?;
        for bundle in bundles {
            append_bytes(&mut record, bundle.identity.id.as_bytes())?;
            append_bytes(&mut record, bundle.identity.version.as_bytes())?;
            append_bytes(&mut record, bundle.identity.publisher.as_bytes())?;
            append_bytes(&mut record, bundle_origin_bytes(bundle.origin))?;
            append_flag(&mut record, bundle.immutable);
            append_bytes(&mut record, bundle.digest.as_bytes())?;
        }
        records.push(record);
    }
    records.sort();
    Ok(records)
}

fn encode_semantic_identity_v1(records: &[Vec<u8>]) -> Result<Vec<u8>, BundleError> {
    let mut identity = Vec::new();
    append_bytes(&mut identity, SEMANTIC_IDENTITY_DOMAIN_V1)?;
    append_count(&mut identity, records.len())?;
    for record in records {
        append_bytes(&mut identity, record)?;
    }
    Ok(identity)
}

fn compare_bundle_identity(left: &PreparedBundle, right: &PreparedBundle) -> std::cmp::Ordering {
    left.identity
        .id
        .as_bytes()
        .cmp(right.identity.id.as_bytes())
        .then_with(|| {
            left.identity
                .version
                .as_bytes()
                .cmp(right.identity.version.as_bytes())
        })
        .then_with(|| {
            left.identity
                .publisher
                .as_bytes()
                .cmp(right.identity.publisher.as_bytes())
        })
        .then_with(|| bundle_origin_bytes(left.origin).cmp(bundle_origin_bytes(right.origin)))
        .then_with(|| left.immutable.cmp(&right.immutable))
        .then_with(|| left.digest.as_bytes().cmp(right.digest.as_bytes()))
}

fn bundle_origin_bytes(origin: BundleOrigin) -> &'static [u8] {
    match origin {
        BundleOrigin::Builtin => b"builtin",
        BundleOrigin::Installed => b"installed",
    }
}

fn append_count(bytes: &mut Vec<u8>, count: usize) -> Result<(), BundleError> {
    let count = u64::try_from(count).map_err(|_| BundleError::PreparedEncode {
        detail: "semantic identity count exceeds u64".to_string(),
    })?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), BundleError> {
    let length = u64::try_from(value.len()).map_err(|_| BundleError::PreparedEncode {
        detail: "semantic identity field length exceeds u64".to_string(),
    })?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn append_flag(bytes: &mut Vec<u8>, value: bool) {
    bytes.push(u8::from(value));
}
