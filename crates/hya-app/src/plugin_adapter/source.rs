use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_engine::runtime_registry::RuntimeSourceSkill;
use hya_plugin::PreparedPlugin;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::SkillContribution;
use hya_tool::{SkillCatalogEntry, ToolPermission};
use sha2::{Digest, Sha256};

use super::PluginHostAdapter;
use crate::runtime_reconcile::{
    PreparedExport, PreparedFailure, PreparedResult, PreparedSource, ReconcileError, SourceId,
    update_bytes,
};

pub(crate) async fn prepare(id: SourceId, spec: PluginSpec) -> PreparedResult {
    let (host, failures) =
        hya_plugin::PluginHost::connect_all_observed(vec![spec], crate::runtime::host_info()).await;
    let prepared = match failures.into_values().next() {
        Some(error) => Err(format!("PLUGIN_START_FAILED: {error}")),
        None => prepared_plugin_host(host).map_err(|error| error.to_string()),
    };
    match prepared {
        Ok(source) => source.into(),
        Err(error) => PreparedFailure::new(id, error).into(),
    }
}

/// Parse one shared Skill contribution into runtime metadata at the declaration seam.
pub(crate) fn parse_skill_contribution(
    contribution: &SkillContribution,
    path: PathBuf,
) -> Result<SkillCatalogEntry, ReconcileError> {
    let parsed =
        hya_engine::skill_catalog::parse_skill(&contribution.content).ok_or_else(|| {
            ReconcileError::InvalidPrepared(format!(
                "Skill contribution `{}` has invalid SKILL.md frontmatter",
                contribution.id
            ))
        })?;
    if parsed.name != contribution.id {
        return Err(ReconcileError::InvalidPrepared(format!(
            "parsed Skill name `{}` does not match contribution id `{}`",
            parsed.name, contribution.id
        )));
    }
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    Ok(SkillCatalogEntry {
        name: parsed.name,
        description: parsed.description,
        content: parsed.content,
        allowed_tools: parsed.allowed_tools,
        model: parsed.model,
        path,
        dir,
        origin: hya_tool::SkillCatalogOrigin::Virtual,
        disable_model_invocation: parsed.disable_model_invocation,
    })
}

/// Prepare one dynamic plugin source through the same Skill parser and source view.
fn prepared_plugin_source(
    plugin: PreparedPlugin,
    owner: Arc<PluginHostAdapter>,
) -> Result<PreparedSource, ReconcileError> {
    let id = SourceId::plugin(plugin.id());
    let contributions = plugin.contributions();
    contributions.validate(plugin.id()).map_err(|error| {
        ReconcileError::InvalidPrepared(format!("plugin contribution validation failed: {error}"))
    })?;
    let skills = contributions
        .skills
        .iter()
        .map(|contribution| {
            let path = PathBuf::from(format!(
                "plugin:{}/skills/{}/SKILL.md",
                plugin.id(),
                contribution.id
            ));
            let entry = parse_skill_contribution(contribution, path)?;
            Ok(RuntimeSourceSkill::new(
                format!("plugin:{}/skill/{}", plugin.id(), contribution.id),
                contribution.id.clone(),
                Vec::new(),
                contribution.digest.clone(),
                contribution.content.clone(),
                entry,
            ))
        })
        .collect::<Result<Vec<_>, ReconcileError>>()?;
    let mut digest = Sha256::new();
    digest.update(b"hya:plugin-declaration:v1\0");
    update_bytes(&mut digest, plugin.canonical_declaration());
    let declaration_digest = digest.finalize().into();
    // Contributed tools are composed into their qualified provider-facing
    // name by the host: `{plugin_id}__{declared local name}`. The inner tool
    // keeps the declared name for its `tool/call` wire requests; the
    // `NamedTool` wrapper exposes only the qualified spelling to the
    // registry, schemas, and permission plane.
    let exports = plugin
        .tools()
        .into_iter()
        .map(|tool| {
            let local_name = tool.name().to_string();
            let qualified = format!("{}__{local_name}", plugin.id());
            let exposed = owner.named_tool(qualified, tool);
            PreparedExport::tool(local_name, Vec::new(), exposed, ToolPermission::Tool)
        })
        .collect();
    Ok(PreparedSource::new(id, declaration_digest, owner, exports).with_skills(skills))
}

/// The host is retained only by its published source, alongside its tools.
pub(crate) fn prepared_plugin_host(
    host: hya_plugin::PluginHost,
) -> Result<PreparedSource, ReconcileError> {
    let plugin = host.prepared_plugins().into_iter().next().ok_or_else(|| {
        ReconcileError::InvalidPrepared("plugin host has no prepared plugin".into())
    })?;
    let has_hooks = !plugin.contributions().hooks.is_empty();
    let owner = Arc::new(PluginHostAdapter::new(Arc::new(host)));
    let mut source = prepared_plugin_source(plugin, owner.clone())?;
    if has_hooks {
        source.hooks = Some(owner);
    }
    Ok(source)
}
