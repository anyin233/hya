//! Prepare immutable bundle process/MCP resources before generation publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_bundle::{
    PreparedInstallableBundle, PreparedProcessExtension, PreparedProcessKind, PreparedSchema,
};
use hya_core::{CoreError, RuntimeSource, RuntimeSourceExport, RuntimeSourceId};
use hya_mcp::{McpServerConfig, PreparedMcpServer};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_plugin::{PluginContributionSet, PluginHost, SkillContribution};
use hya_tool::{NamedTool, Tool, ToolPermission};
use sha2::{Digest, Sha256};

use crate::runtime_reconcile::{bundle_schema_claims, prepared_bundle_skill_exports};

#[derive(Clone)]
pub(crate) struct CachedBundleSource {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) source: RuntimeSource,
}

pub(crate) fn fingerprint(
    bundle: &PreparedInstallableBundle,
    process: Option<&PreparedProcessExtension>,
    schemas: &[PreparedSchema],
) -> Result<[u8; 32], CoreError> {
    let bytes = serde_json::to_vec(&(bundle, process, schemas))
        .map_err(|error| CoreError::Invalid(format!("encode bundle runtime identity: {error}")))?;
    Ok(Sha256::digest(bytes).into())
}

struct MaterializedRoot(PathBuf);
impl Drop for MaterializedRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct BundleOwner {
    // Drop clients before removing their restart files.
    _host: Option<Arc<PluginHost>>,
    _mcp: Vec<Arc<PreparedMcpServer>>,
    _root: Option<MaterializedRoot>,
}

fn materialize(bundle: &PreparedInstallableBundle) -> Result<MaterializedRoot, CoreError> {
    let root = std::env::temp_dir().join(format!(
        "hya-bundle-runtime-{}",
        hya_proto::SessionId::new()
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&root)
        .map_err(|error| invalid("create private bundle directory", error))?;
    let guard = MaterializedRoot(root);
    let mut paths = BTreeMap::new();
    for resource in bundle
        .tools()
        .iter()
        .chain(bundle.skills())
        .chain(bundle.mcp())
        .chain(bundle.hooks())
        .chain(bundle.extensions())
    {
        let path = Path::new(&resource.source_path);
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
            || path.as_os_str().is_empty()
        {
            return Err(CoreError::Invalid(format!(
                "invalid bundle resource path {}",
                resource.source_path
            )));
        }
        if let Some(previous) =
            paths.insert(resource.source_path.as_str(), resource.content.as_str())
        {
            if previous != resource.content {
                return Err(CoreError::Invalid(format!(
                    "conflicting bundle resource {}",
                    resource.source_path
                )));
            }
            continue;
        }
        let path = guard.0.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| invalid("create bundle resource parent", error))?;
        }
        std::fs::write(path, &resource.content)
            .map_err(|error| invalid("write bundle resource", error))?;
    }
    Ok(guard)
}

fn invalid(context: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Invalid(format!("{context}: {error}"))
}

fn expand(value: &str, root: &Path) -> String {
    value
        .replace("${BUNDLE_ROOT}", &root.to_string_lossy())
        .replace("${CLAUDE_PLUGIN_ROOT}", &root.to_string_lossy())
}

fn argv(command: &[String], root: &Path) -> Vec<String> {
    command
        .iter()
        .map(|arg| {
            let expanded = expand(arg, root);
            let path = Path::new(&expanded);
            if path.is_relative() && root.join(path).is_file() {
                root.join(path).to_string_lossy().into_owned()
            } else {
                expanded
            }
        })
        .collect()
}

pub(crate) async fn prepare_source(
    bundle: &PreparedInstallableBundle,
    process: Option<&PreparedProcessExtension>,
    schemas: &[PreparedSchema],
) -> Result<CachedBundleSource, CoreError> {
    let id = &bundle.identity().id;
    let fingerprint = fingerprint(bundle, process, schemas)?;
    // A shared JavaScript Plugin has no agent activation to own a Bun sidecar.
    // Promote its explicit executable entrypoints to a generation-owned process.
    let implicit_process = if process.is_none() && bundle.plugin_bundle().is_some() {
        let entrypoints = bundle
            .tools()
            .iter()
            .chain(bundle.hooks())
            .map(|resource| resource.source_path.as_str())
            .collect::<BTreeSet<_>>();
        if entrypoints.is_empty() {
            None
        } else {
            let mut command = crate::plugins::bundle_sidecar_command().ok_or_else(|| {
                CoreError::Invalid("Bun is required for JavaScript Plugin resources".into())
            })?;
            command.extend(["--plugin-id".into(), bundle.namespace().into()]);
            for path in entrypoints {
                command.extend([
                    "--bundle-extension".into(),
                    format!("${{BUNDLE_ROOT}}/{path}"),
                ]);
            }
            Some(PreparedProcessExtension {
                kind: PreparedProcessKind::Bun,
                command,
            })
        }
    } else {
        None
    };
    let process = process.or(implicit_process.as_ref());

    let contributions = PluginContributionSet {
        skills: bundle
            .skills()
            .iter()
            .map(|resource| SkillContribution {
                id: resource.local_id.clone(),
                content: resource.content.clone(),
                digest: resource.digest.clone(),
            })
            .collect(),
        ..PluginContributionSet::default()
    };
    let skills = prepared_bundle_skill_exports(id, bundle.skills(), &contributions)
        .map_err(|error| invalid("prepare bundle Skills", error))?;
    let mut claims = bundle_schema_claims(id, schemas, bundle.tools())
        .map_err(|error| invalid("prepare bundle schema claims", error))?;
    let mut exports = Vec::new();
    let mut owner = BundleOwner {
        _host: None,
        _mcp: Vec::new(),
        _root: None,
    };
    let mut resources = BTreeMap::new();
    let mut hooks: Option<Arc<dyn hya_core::hooks::HookDispatcher>> = None;
    let mut declaration = Sha256::new();
    declaration.update(fingerprint);
    if process.is_some() || !bundle.mcp().is_empty() {
        let root = materialize(bundle)?;
        if let Some(process) = process {
            let mut command = argv(&process.command, &root.0);
            let kind = match process.kind {
                PreparedProcessKind::Rust => PluginKindWire::Rust,
                PreparedProcessKind::Bun => PluginKindWire::Bun,
                PreparedProcessKind::Claude => {
                    let bun = crate::plugins::find_bun()
                        .ok_or_else(|| CoreError::Invalid("Claude bundle requires Bun".into()))?;
                    let mut runner = vec![
                        bun.to_string_lossy().into_owned(),
                        "run".into(),
                        crate::plugins::claude_adapter_dir()
                            .join("src/main.ts")
                            .to_string_lossy()
                            .into_owned(),
                        "--plugin-id".into(),
                        bundle.namespace().into(),
                    ];
                    runner.append(&mut command);
                    command = runner;
                    PluginKindWire::Claude
                }
            };
            let mut env = BTreeMap::from([
                (
                    "HYA_BUNDLE_ROOT".into(),
                    root.0.to_string_lossy().into_owned(),
                ),
                (
                    "CLAUDE_PLUGIN_ROOT".into(),
                    root.0.to_string_lossy().into_owned(),
                ),
            ]);
            if let Ok(path) = std::env::var("PATH") {
                env.insert("PATH".into(), path);
            }
            let spec = PluginSpec {
                id: bundle.namespace().into(),
                kind,
                command,
                timeout_ms: None,
                env,
                posture_overrides: BTreeMap::new(),
                plugin_dir: None,
            };
            let host = Arc::new(
                PluginHost::connect_bundle(spec, crate::host_info(), root.0.clone())
                    .await
                    .map_err(|error| invalid("start bundle process", error))?,
            );
            let tools = host.tools();
            let declared = bundle
                .tools()
                .iter()
                .map(|tool| tool.local_id.as_str())
                .collect::<BTreeSet<_>>();
            let actual = tools
                .iter()
                .map(|tool| tool.name())
                .collect::<BTreeSet<_>>();
            if declared != actual {
                return Err(CoreError::Invalid(format!(
                    "bundle `{id}` process tools differ from declared resources: expected {declared:?}, got {actual:?}"
                )));
            }
            let declared_hooks = bundle
                .hooks()
                .iter()
                .map(|hook| hook.local_id.as_str())
                .collect::<BTreeSet<_>>();
            let actual_hooks = host
                .contributions()
                .iter()
                .flat_map(|(_, set)| set.hooks.iter().map(|hook| hook.name.as_str()))
                .collect::<BTreeSet<_>>();
            if declared_hooks != actual_hooks {
                return Err(CoreError::Invalid(format!(
                    "bundle `{id}` process hooks differ from declared resources"
                )));
            }
            for (_, set) in host.contributions() {
                if !set.workspace_adapters.is_empty() {
                    return Err(CoreError::Invalid(format!(
                        "bundle `{id}` process workspace adapters have no bundle resource contract"
                    )));
                }
                if !set.skills.is_empty() {
                    prepared_bundle_skill_exports(id, bundle.skills(), set)
                        .map_err(|error| invalid("bundle process Skill declarations", error))?;
                }
            }
            for tool in tools {
                let local = tool.name().to_string();
                let metadata = bundle
                    .tools()
                    .iter()
                    .find(|resource| resource.local_id == local)
                    .ok_or_else(|| {
                        CoreError::Invalid("bundle tool declaration disappeared".into())
                    })?;
                let canonical = format!("{}__{local}", bundle.namespace());
                let named: Arc<dyn Tool> = Arc::new(NamedTool::new(canonical.clone(), tool));
                exports.push(RuntimeSourceExport::tool(
                    local,
                    canonical.clone(),
                    metadata.aliases.clone(),
                    named,
                    ToolPermission::Tool,
                ));
                for claim in &mut claims {
                    if bundle.plugin_bundle().is_some()
                        && claim.canonical_tool == metadata.stable_id
                    {
                        claim.canonical_tool = canonical.clone();
                    }
                }
            }
            for plugin in host.prepared_plugins() {
                declaration.update(plugin.canonical_declaration());
            }
            hooks = Some(host.clone());
            owner._host = Some(host);
        }
        for resource in bundle.mcp() {
            let mut config: McpServerConfig = serde_json::from_str(&resource.content)
                .map_err(|error| invalid("decode bundle MCP", error))?;
            if config.enabled == Some(false) {
                continue;
            }
            config.command = argv(&config.command, &root.0);
            if let Some(env) = &mut config.env {
                for value in env.values_mut() {
                    *value = expand(value, &root.0);
                }
            }
            let server_name = format!("{}_{}", bundle.namespace(), resource.local_id);
            let server = Arc::new(
                hya_mcp::prepare_bundle(server_name, config, root.0.clone())
                    .await
                    .map_err(|error| invalid("start bundle MCP", error))?,
            );
            for tool in server.tools() {
                declaration.update(
                    serde_json::to_vec(&tool.schema())
                        .map_err(|error| invalid("encode MCP schema", error))?,
                );
                let prefix = format!("mcp__{}__", server.name());
                let local = tool
                    .name()
                    .strip_prefix(&prefix)
                    .ok_or_else(|| {
                        CoreError::Invalid(format!(
                            "bundle MCP tool `{}` has an invalid server prefix",
                            tool.name()
                        ))
                    })?
                    .to_string();
                let canonical = format!(
                    "{}__mcp__{}__{local}",
                    bundle.namespace(),
                    resource.local_id
                );
                let named: Arc<dyn Tool> = Arc::new(NamedTool::new(canonical.clone(), tool));
                exports.push(RuntimeSourceExport::tool(
                    format!("mcp/{}/{local}", resource.local_id),
                    canonical,
                    Vec::new(),
                    named,
                    ToolPermission::Mcp,
                ));
            }
            resources.extend(server.resources());
            owner._mcp.push(server);
        }
        owner._root = Some(root);
    }
    let mut source = RuntimeSource::new(
        RuntimeSourceId::bundle(id),
        declaration.finalize().into(),
        Arc::new(owner),
        exports,
    )
    .with_skills(skills)
    .with_resources(resources)
    .with_schemas(claims);
    if let Some(hooks) = hooks {
        source = source.with_hooks(hooks);
    }
    Ok(CachedBundleSource {
        fingerprint,
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hya_bundle::{BundleSource, SourceFile, prepare_package};

    #[tokio::test]
    async fn bundle_process_rejects_unrepresentable_workspace_adapter_contribution() {
        let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 result={'protocol_version':1,'plugin':{'id':'undeclared','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[],'workspaceAdapters':[{'type':'remote','name':'Remote','description':'unrepresented adapter'}]} if r.get('method') == 'initialize' else {}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
        let prepared = prepare_package(BundleSource::new(
            "undeclared-contribution",
            vec![
                SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/undeclared, version: 1.0.0, publisher: acme }\nextensions:\n  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\n  files: [{ id: runtime, path: runtime.py }]\n"),
                SourceFile::new("runtime.py", script),
            ],
        )).unwrap();
        let result = prepare_source(
            &prepared.bundles()[0],
            prepared.bundle_process("acme/undeclared"),
            &[],
        )
        .await;
        assert!(
            matches!(result, Err(CoreError::Invalid(ref detail)) if detail.contains("workspace adapters")),
            "bundle initialization must reject contributions absent from its schema"
        );
    }
}
