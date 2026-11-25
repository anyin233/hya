use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use hya_bundle::{PreparedResource, PreparedSchema};
use hya_engine::runtime_registry::{RuntimeSourceSkill, SourceSchema};
use hya_plugin::messages::{PluginContributionSet, SkillContribution};
use hya_tool::SkillCatalogEntry;
use sha2::{Digest, Sha256};

use crate::plugin_adapter::source::parse_skill_contribution;
use crate::runtime_reconcile::{PreparedSource, ReconcileError, SourceId, update_bytes};

/// Convert prepared bundle Skill resources through the shared contribution seam.
pub(crate) fn adapt_prepared_bundle_skills(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<Vec<SkillCatalogEntry>, ReconcileError> {
    Ok(
        prepared_bundle_skill_exports(bundle_id, resources, contributions)?
            .into_iter()
            .map(|skill| skill.entry().clone())
            .collect(),
    )
}

/// Prepare a static bundle source whose Skills are already validated and parsed.
pub(crate) fn prepared_static_bundle_source(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<PreparedSource, ReconcileError> {
    let skills = prepared_bundle_skill_exports(bundle_id, resources, contributions)?;
    let declaration_digest = digest_static_bundle_skills(bundle_id, &skills);
    Ok(PreparedSource::new(
        SourceId::bundle(bundle_id),
        declaration_digest,
        Arc::new(()),
        Vec::new(),
    )
    .with_skills(skills))
}

/// Resolve one prepared bundle's declared schema extensions into runtime
/// scheme claims.
///
/// Bundle tools are view-scoped (sidecar activation), so the claim names the
/// owning tool by its `bundle:{id}/tool/{local}` stable id — the spelling a
/// bundle agent's compiled view resolves the sidecar tool under — rather than
/// a registry export.
pub(crate) fn bundle_schema_claims(
    bundle_id: &str,
    schemas: &[PreparedSchema],
    tools: &[PreparedResource],
) -> Result<Vec<SourceSchema>, ReconcileError> {
    let mut claims = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let Some(tool) = tools.iter().find(|tool| tool.local_id == schema.tool) else {
            return Err(ReconcileError::InvalidPrepared(format!(
                "bundle `{bundle_id}` schema `{}` references unknown tool `{}`",
                schema.scheme, schema.tool
            )));
        };
        claims.push(SourceSchema {
            scheme: schema.scheme.clone(),
            canonical_tool: tool.stable_id.clone(),
            writable: schema.writable,
        });
    }
    Ok(claims)
}

/// Validate and parse every prepared bundle Skill exactly once at the contribution seam.
pub(crate) fn prepared_bundle_skill_exports(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<Vec<RuntimeSourceSkill>, ReconcileError> {
    contributions.validate(bundle_id).map_err(|error| {
        ReconcileError::InvalidPrepared(format!("Skill contribution validation failed: {error}"))
    })?;

    let mut prepared_by_id = BTreeMap::new();
    for resource in resources {
        let expected_stable_id = format!("bundle:{bundle_id}/skill/{}", resource.local_id);
        if resource.stable_id != expected_stable_id {
            return Err(ReconcileError::InvalidPrepared(format!(
                "prepared Skill `{}` has unexpected stable id `{}`",
                resource.local_id, resource.stable_id
            )));
        }
        if prepared_by_id
            .insert(resource.local_id.as_str(), resource)
            .is_some()
        {
            return Err(ReconcileError::InvalidPrepared(format!(
                "duplicate prepared Skill `{}`",
                resource.local_id
            )));
        }
    }
    if !contributions.skills.is_empty() && prepared_by_id.len() != contributions.skills.len() {
        return Err(ReconcileError::InvalidPrepared(format!(
            "prepared Skill count {} does not match contribution count {}",
            prepared_by_id.len(),
            contributions.skills.len()
        )));
    }

    let mut contribution_by_id = BTreeMap::new();
    for contribution in &contributions.skills {
        if contribution_by_id
            .insert(contribution.id.as_str(), contribution)
            .is_some()
        {
            return Err(ReconcileError::InvalidPrepared(format!(
                "duplicate Skill contribution `{}`",
                contribution.id
            )));
        }
    }

    let mut exports = Vec::with_capacity(resources.len());
    for (local_id, resource) in prepared_by_id {
        if !contributions.skills.is_empty() {
            let Some(contribution) = contribution_by_id.get(local_id) else {
                return Err(ReconcileError::InvalidPrepared(format!(
                    "prepared Skill `{local_id}` is missing from contributions"
                )));
            };
            if contribution.content != resource.content || contribution.digest != resource.digest {
                return Err(ReconcileError::InvalidPrepared(format!(
                    "Skill contribution `{local_id}` differs from prepared id/content/digest"
                )));
            }
        }
        let contribution = SkillContribution {
            id: local_id.to_string(),
            content: resource.content.clone(),
            digest: resource.digest.clone(),
        };
        let path = PathBuf::from(format!("bundle:{bundle_id}/{}", resource.source_path));
        let entry = parse_skill_contribution(&contribution, path)?;
        exports.push(RuntimeSourceSkill::new(
            resource.stable_id.clone(),
            resource.local_id.clone(),
            resource.aliases.clone(),
            resource.digest.clone(),
            resource.content.clone(),
            entry,
        ));
    }
    Ok(exports)
}

/// Build deterministic declaration identity bytes for a static bundle Skill source.
fn digest_static_bundle_skills(bundle_id: &str, skills: &[RuntimeSourceSkill]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:static-bundle-skill-declaration:v1\0");
    update_bytes(&mut digest, bundle_id.as_bytes());
    for skill in skills {
        update_bytes(&mut digest, skill.stable_id().as_bytes());
        update_bytes(&mut digest, skill.local_id().as_bytes());
        update_bytes(&mut digest, skill.digest().as_bytes());
        update_bytes(&mut digest, skill.content().as_bytes());
    }
    digest.finalize().into()
}
