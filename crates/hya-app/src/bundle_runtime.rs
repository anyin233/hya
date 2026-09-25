//! Prepare immutable bundle process/MCP resources before generation publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_bundle::{
    PreparedApi, PreparedInstallableBundle, PreparedPermissionMode, PreparedProcessExtension,
    PreparedProcessKind, PreparedSchema,
};
use hya_core::{CoreError, RuntimeSource, RuntimeSourceExport, RuntimeSourceId};
use hya_mcp::{McpServerConfig, PreparedMcpServer};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_plugin::{PluginContributionSet, PluginHost, SkillContribution};
use hya_tool::{NamedTool, Tool, ToolPermission};
use sha2::{Digest, Sha256};

use crate::bundle_config::BundleConfigLocation;
use crate::runtime_reconcile::{bundle_schema_claims, prepared_bundle_skill_exports};

#[derive(Clone)]
pub(crate) struct CachedBundleSource {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) source: RuntimeSource,
}

/// One bundle's configuration location plus, for bundles that start a
/// process or MCP server, the `config.yml` content digest read at refresh.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct BundleRuntimeConfig {
    location: BundleConfigLocation,
    /// `None` when the file is absent or the bundle spawns nothing.
    digest: Option<[u8; 32]>,
    watched: bool,
}

impl BundleRuntimeConfig {
    /// Capture `location` for `bundle`, reading the file only when the bundle
    /// starts a process or MCP server that the file can influence.
    pub(crate) fn capture(
        bundle: &PreparedInstallableBundle,
        process: Option<&PreparedProcessExtension>,
        location: BundleConfigLocation,
    ) -> Self {
        let watched = spawns_runtime(bundle, process);
        let digest = if watched {
            location.content_digest()
        } else {
            None
        };
        Self {
            location,
            digest,
            watched,
        }
    }

    pub(crate) fn location(&self) -> &BundleConfigLocation {
        &self.location
    }

    /// Whether config edits restart this bundle's runtime source.
    pub(crate) fn watched(&self) -> bool {
        self.watched
    }

    pub(crate) fn digest(&self) -> Option<[u8; 32]> {
        self.digest
    }
}

/// Whether preparing `bundle` starts an out-of-process provider: an explicit
/// `extensions.process`, a declared MCP server, or the implicit Bun process of
/// a JavaScript Plugin with tool or hook entrypoints.
fn spawns_runtime(
    bundle: &PreparedInstallableBundle,
    process: Option<&PreparedProcessExtension>,
) -> bool {
    process.is_some()
        || !bundle.mcp().is_empty()
        || (bundle.plugin_bundle().is_some()
            && (!bundle.tools().is_empty() || !bundle.hooks().is_empty()))
}

/// Runtime source identity: the prepared bundle, its process, schemas, and
/// API endpoints, and for spawning bundles the configuration location and content
/// digest, so a `config.yml` edit restarts the bundle's providers like a
/// changed bundle.
pub(crate) fn fingerprint(
    bundle: &PreparedInstallableBundle,
    process: Option<&PreparedProcessExtension>,
    schemas: &[PreparedSchema],
    apis: &[PreparedApi],
    permission_modes: &[PreparedPermissionMode],
    config: &BundleRuntimeConfig,
) -> Result<[u8; 32], CoreError> {
    let config = config.watched.then_some(config);
    // Without modes the encoding is unchanged, so bundles that declare none
    // keep their runtime identity (and the Workflow request hashes built on it).
    let bytes = if permission_modes.is_empty() {
        serde_json::to_vec(&(bundle, process, schemas, apis, config))
    } else {
        serde_json::to_vec(&(bundle, process, schemas, apis, config, permission_modes))
    }
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
        let bytes = resource
            .source_bytes()
            .map_err(|error| invalid("decode bundle resource", error))?;
        let path = guard.0.join(path);
        if let Some(previous) = paths.insert(resource.source_path.as_str(), bytes.clone()) {
            if previous != bytes {
                return Err(CoreError::Invalid(format!(
                    "conflicting bundle resource {}",
                    resource.source_path
                )));
            }
            if resource.binary_base64.is_some() {
                mark_executable(&path)?;
            }
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| invalid("create bundle resource parent", error))?;
        }
        std::fs::write(&path, bytes).map_err(|error| invalid("write bundle resource", error))?;
        if resource.binary_base64.is_some() {
            mark_executable(&path)?;
        }
    }
    Ok(guard)
}

fn mark_executable(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| invalid("mark bundle executable", error))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn invalid(context: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Invalid(format!("{context}: {error}"))
}

/// Expand `${BUNDLE_ROOT}`, `${CLAUDE_PLUGIN_ROOT}`, `${BUNDLE_CONFIG_DIR}`,
/// and `${BUNDLE_CONFIG_FILE}` in one declared argv or env value.
fn expand(value: &str, root: &Path, config: &BundleConfigLocation) -> String {
    value
        .replace("${BUNDLE_ROOT}", &root.to_string_lossy())
        .replace("${CLAUDE_PLUGIN_ROOT}", &root.to_string_lossy())
        .replace("${BUNDLE_CONFIG_DIR}", &config.dir().to_string_lossy())
        .replace("${BUNDLE_CONFIG_FILE}", &config.file().to_string_lossy())
}

fn argv(command: &[String], root: &Path, config: &BundleConfigLocation) -> Vec<String> {
    command
        .iter()
        .map(|arg| {
            let expanded = expand(arg, root, config);
            let path = Path::new(&expanded);
            if path.is_relative() && root.join(path).is_file() {
                root.join(path).to_string_lossy().into_owned()
            } else {
                expanded
            }
        })
        .collect()
}

/// Cleared environment for a bundle `extensions.process` provider: inherited
/// `PATH`, the materialized root, and the bundle's configuration location.
fn process_env(root: &Path, config: &BundleConfigLocation) -> BTreeMap<String, String> {
    let mut env = config.env();
    env.extend([
        (
            "HYA_BUNDLE_ROOT".into(),
            root.to_string_lossy().into_owned(),
        ),
        (
            "CLAUDE_PLUGIN_ROOT".into(),
            root.to_string_lossy().into_owned(),
        ),
    ]);
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".into(), path);
    }
    env
}

/// The declared parts of one bundle that its runtime source is prepared from.
pub(crate) struct BundleRuntimeParts<'a> {
    /// The explicit `extensions.process`, when declared.
    pub(crate) process: Option<&'a PreparedProcessExtension>,
    /// Declared URI-scheme extensions.
    pub(crate) schemas: &'a [PreparedSchema],
    /// Declared API endpoints (explicit process only).
    pub(crate) apis: &'a [PreparedApi],
    /// Declared session permission modes (explicit process only).
    pub(crate) permission_modes: &'a [PreparedPermissionMode],
    /// Read-only host services behind the process's capabilities.
    pub(crate) reads: Option<Arc<dyn hya_core::HostSessionReads>>,
}

/// Publishable endpoint declarations: parsed path templates plus the JSON
/// Schema documents read from the bundle's own declared extension files
/// (prepare already validated both; a failure here means a corrupt catalog).
fn source_apis(
    bundle: &PreparedInstallableBundle,
    apis: &[PreparedApi],
) -> Result<Vec<hya_core::SourceApi>, CoreError> {
    let id = &bundle.identity().id;
    let schema = |path: &Option<String>| -> Result<Option<serde_json::Value>, CoreError> {
        let Some(path) = path else {
            return Ok(None);
        };
        let resource = bundle
            .extensions()
            .iter()
            .find(|resource| &resource.source_path == path)
            .ok_or_else(|| {
                CoreError::Invalid(format!("bundle `{id}` API schema `{path}` is not packaged"))
            })?;
        serde_json::from_str(&resource.content)
            .map(Some)
            .map_err(|error| {
                CoreError::Invalid(format!("bundle `{id}` API schema `{path}`: {error}"))
            })
    };
    apis.iter()
        .map(|api| {
            Ok(hya_core::SourceApi {
                id: api.id.clone(),
                method: api.method,
                scope: api.scope,
                path: hya_core::ApiPathTemplate::parse(&api.path).map_err(|reason| {
                    CoreError::Invalid(format!(
                        "bundle `{id}` API `{}` path `{}` {reason}",
                        api.id, api.path
                    ))
                })?,
                description: api.description.clone(),
                request_schema: schema(&api.request_schema)?,
                response_schema: schema(&api.response_schema)?,
            })
        })
        .collect()
}

pub(crate) async fn prepare_source(
    bundle: &PreparedInstallableBundle,
    parts: BundleRuntimeParts<'_>,
    config: &BundleRuntimeConfig,
) -> Result<CachedBundleSource, CoreError> {
    let BundleRuntimeParts {
        process,
        schemas,
        apis,
        permission_modes,
        reads,
    } = parts;
    let bundle_config = config.location();
    let id = &bundle.identity().id;
    let fingerprint = fingerprint(bundle, process, schemas, apis, permission_modes, config)?;
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
    let mut api_provider: Option<Arc<dyn hya_core::BundleApiProvider>> = None;
    let mut declaration = Sha256::new();
    declaration.update(fingerprint);
    if process.is_some() || !bundle.mcp().is_empty() {
        let root = materialize(bundle)?;
        if let Some(process) = process {
            let mut command = argv(&process.command, &root.0, bundle_config);
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
            let env = process_env(&root.0, bundle_config);
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
                PluginHost::connect_bundle_with_reads(
                    spec,
                    crate::host_info(),
                    root.0.clone(),
                    reads,
                )
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
            // A process that implicitly started (a JavaScript Plugin) has no
            // manifest endpoints, so this also rejects stray API declarations.
            let declared_apis = apis
                .iter()
                .map(|api| api.id.as_str())
                .collect::<BTreeSet<_>>();
            let actual_apis = host.declared_apis();
            let actual_apis = actual_apis
                .iter()
                .map(|api| api.name.as_str())
                .collect::<BTreeSet<_>>();
            if declared_apis != actual_apis {
                return Err(CoreError::Invalid(format!(
                    "bundle `{id}` process API endpoints differ from declared apis: expected {declared_apis:?}, got {actual_apis:?}"
                )));
            }
            if !apis.is_empty() {
                api_provider = Some(host.clone());
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
            config.command = argv(&config.command, &root.0, bundle_config);
            // Host defaults sit under the declared map (declared keys win);
            // `hya_mcp::prepare_bundle` adds inherited `PATH` below both and
            // pins `HYA_BUNDLE_ROOT` above them.
            let mut env = bundle_config.env();
            if let Some(declared) = config.env.take() {
                env.extend(declared.into_iter().map(|(key, value)| {
                    let value = expand(&value, &root.0, bundle_config);
                    (key, value)
                }));
            }
            config.env = Some(env);
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
        // Prepare accepts modes only with an explicit process that declares
        // `permission.approve`, and the process's hooks must equal the
        // declared hook resources, so these hooks answer the modes.
        source = source.with_permission_modes(
            permission_modes
                .iter()
                .map(|mode| hya_core::RuntimePermissionMode {
                    id: mode.id.clone(),
                    title: mode.title.clone(),
                    description: mode.description.clone(),
                })
                .collect(),
        );
    }
    if let Some(provider) = api_provider {
        source = source.with_apis(source_apis(bundle, apis)?, provider);
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

    fn test_config(bundle_id: &str) -> BundleConfigLocation {
        let root =
            std::env::temp_dir().join(format!("hya-bundle-config-{}", hya_proto::SessionId::new()));
        crate::bundle_config::bundle_config_location(
            &root.join("hya/config.yaml"),
            bundle_id,
            crate::bundle_config::BundleConfigScope::User,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn bundle_mcp_server_sees_path_root_and_config_location() {
        let report = std::env::temp_dir().join(format!(
            "hya-bundle-mcp-env-{}.txt",
            hya_proto::SessionId::new()
        ));
        let script = r#"import json,os,sys
report = "|".join([
    "path=" + str(bool(os.environ.get("PATH"))),
    "home=" + str("HOME" in os.environ),
    "root=" + str(bool(os.environ.get("HYA_BUNDLE_ROOT"))),
    "dir=" + os.environ.get("HYA_BUNDLE_CONFIG_DIR", ""),
    "file=" + os.environ.get("HYA_BUNDLE_CONFIG_FILE", ""),
    "arg=" + sys.argv[1],
    "explicit=" + os.environ.get("EXPLICIT", ""),
])
open(os.environ["REPORT"], "w").write(report)
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name": "probe", "description": "probe", "inputSchema": {"type": "object"}}]}
    else:
        result = {"resources": []}
    print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#;
        let declaration = serde_json::json!({
            "command": ["python3", "server.py", "${BUNDLE_CONFIG_FILE}"],
            "env": {"EXPLICIT": "${BUNDLE_CONFIG_DIR}", "REPORT": report.to_string_lossy()},
        })
        .to_string();
        let prepared = prepare_package(BundleSource::new(
            "mcp-env",
            vec![
                SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/mcp-env, version: 1.0.0, publisher: acme }\nresources:\n  mcp: [{ id: probe, path: mcp/probe.json }]\nextensions:\n  files: [{ id: server, path: server.py }]\n"),
                SourceFile::new("mcp/probe.json", declaration),
                SourceFile::new("server.py", script),
            ],
        ))
        .unwrap();
        let config = test_config("acme/mcp-env");
        let source = prepare_source(
            &prepared.bundles()[0],
            BundleRuntimeParts {
                process: prepared.bundle_process("acme/mcp-env"),
                schemas: &[],
                apis: &[],
                permission_modes: &[],
                reads: None,
            },
            &BundleRuntimeConfig::capture(
                &prepared.bundles()[0],
                prepared.bundle_process("acme/mcp-env"),
                config.clone(),
            ),
        )
        .await
        .unwrap();
        let observed = std::fs::read_to_string(&report).unwrap();
        drop(source);
        let _ = std::fs::remove_file(&report);
        let dir = config.dir().to_string_lossy();
        let file = config.file().to_string_lossy();
        assert_eq!(
            observed,
            format!(
                "path=True|home=False|root=True|dir={dir}|file={file}|arg={file}|explicit={dir}"
            )
        );
    }

    #[test]
    fn config_content_is_folded_into_spawning_bundle_fingerprints_only() {
        let spawning = prepare_package(BundleSource::new(
            "spawning",
            vec![
                SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/spawning, version: 1.0.0, publisher: acme }\nextensions:\n  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\n  files: [{ id: runtime, path: runtime.py }]\n"),
                SourceFile::new("runtime.py", "pass\n"),
            ],
        ))
        .unwrap();
        let skills_only = prepare_package(BundleSource::new(
            "skills-only",
            vec![
                SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/skills-only, version: 1.0.0, publisher: acme }\nresources:\n  skills: [{ id: guide, path: skills/guide/SKILL.md }]\n"),
                SourceFile::new("skills/guide/SKILL.md", "---\nname: guide\ndescription: guide\n---\nbody\n"),
            ],
        ))
        .unwrap();
        for (prepared, id, watched) in [
            (&spawning, "acme/spawning", true),
            (&skills_only, "acme/skills-only", false),
        ] {
            let bundle = &prepared.bundles()[0];
            let process = prepared.bundle_process(id);
            let location = test_config(id);
            let absent = BundleRuntimeConfig::capture(bundle, process, location.clone());
            assert_eq!(absent.watched(), watched);
            let before = fingerprint(bundle, process, &[], &[], &[], &absent).unwrap();
            std::fs::create_dir_all(location.dir()).unwrap();
            std::fs::write(location.file(), "key: value\n").unwrap();
            let present = BundleRuntimeConfig::capture(bundle, process, location.clone());
            let after = fingerprint(bundle, process, &[], &[], &[], &present).unwrap();
            assert_eq!(before != after, watched, "{id}");
            std::fs::write(location.file(), "key: other\n").unwrap();
            let edited = BundleRuntimeConfig::capture(bundle, process, location.clone());
            let edited = fingerprint(bundle, process, &[], &[], &[], &edited).unwrap();
            assert_eq!(after != edited, watched, "{id}");
            let _ = std::fs::remove_dir_all(location.dir());
        }
    }

    #[test]
    fn process_environment_carries_root_path_and_config_location() {
        let config = test_config("acme/process-env");
        let env = process_env(Path::new("/materialized"), &config);
        assert_eq!(
            env.get("HYA_BUNDLE_ROOT").map(String::as_str),
            Some("/materialized")
        );
        assert_eq!(
            env.get("CLAUDE_PLUGIN_ROOT").map(String::as_str),
            Some("/materialized")
        );
        assert_eq!(
            env.get("HYA_BUNDLE_CONFIG_FILE").map(String::as_str),
            Some(config.file().to_string_lossy().as_ref())
        );
        assert_eq!(
            env.get("HYA_BUNDLE_CONFIG_DIR").map(String::as_str),
            Some(config.dir().to_string_lossy().as_ref())
        );
        assert!(!env.contains_key("HOME"));
        assert_eq!(
            argv(
                &["${BUNDLE_CONFIG_FILE}".to_string()],
                Path::new("/materialized"),
                &config
            ),
            vec![config.file().to_string_lossy().into_owned()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn materialized_native_bundle_executable_can_run() {
        let prepared = prepare_package(BundleSource::new(
            "native-executable",
            vec![
                SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/native-run, version: 1.0.0, publisher: acme }\nextensions:\n  rust: [{ id: provider, path: bin/provider }]\n  process: { kind: rust, command: ['${BUNDLE_ROOT}/bin/provider'] }\n"),
                SourceFile::new("bin/provider", b"#!/bin/sh\nprintf native-ok\n"),
            ],
        ))
        .unwrap();
        let root = materialize(&prepared.bundles()[0]).unwrap();
        let output = std::process::Command::new(root.0.join("bin/provider"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"native-ok");
    }

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
            BundleRuntimeParts {
                process: prepared.bundle_process("acme/undeclared"),
                schemas: &[],
                apis: &[],
                permission_modes: &[],
                reads: None,
            },
            &BundleRuntimeConfig::capture(
                &prepared.bundles()[0],
                prepared.bundle_process("acme/undeclared"),
                test_config("acme/undeclared"),
            ),
        )
        .await;
        assert!(
            matches!(result, Err(CoreError::Invalid(ref detail)) if detail.contains("workspace adapters")),
            "bundle initialization must reject contributions absent from its schema"
        );
    }

    /// A bundle process whose initialize reply declares `apis` (a Python
    /// literal) for the manifest-declared `apis:` block.
    async fn prepare_api_bundle(
        manifest_apis: &str,
        process_apis: &str,
    ) -> Result<CachedBundleSource, CoreError> {
        let script = format!(
            r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 result={{'protocol_version':1,'plugin':{{'id':'apis','version':'1.0.0','kind':'bun'}},'hooks':[],'tools':[],'apis':{process_apis}}} if r.get('method') == 'initialize' else {{}}
 if 'id' in r: print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#
        );
        let prepared = prepare_package(BundleSource::new(
            "apis",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    format!("kind: Plugin\nidentity: {{ id: acme/apis, version: 1.0.0, publisher: acme }}\nextensions:\n  process: {{ kind: bun, command: [python3, '${{BUNDLE_ROOT}}/runtime.py'] }}\n  files: [{{ id: runtime, path: runtime.py }}, {{ id: usage-schema, path: usage.json }}]\n{manifest_apis}"),
                ),
                SourceFile::new("runtime.py", script),
                SourceFile::new("usage.json", r#"{"type":"object"}"#),
            ],
        ))
        .unwrap();
        prepare_source(
            &prepared.bundles()[0],
            BundleRuntimeParts {
                process: prepared.bundle_process("acme/apis"),
                schemas: &[],
                apis: prepared.bundle_apis("acme/apis"),
                permission_modes: &[],
                reads: None,
            },
            &BundleRuntimeConfig::capture(
                &prepared.bundles()[0],
                prepared.bundle_process("acme/apis"),
                test_config("acme/apis"),
            ),
        )
        .await
    }

    #[tokio::test]
    async fn bundle_process_apis_must_equal_the_declared_apis() {
        let declared = "apis: [{ id: usage, method: GET, scope: session, path: /usage, description: Token usage, response_schema: usage.json }]\n";
        let published = prepare_api_bundle(declared, "[{'name':'usage'}]")
            .await
            .unwrap();
        let Some(apis) = published.source.apis() else {
            panic!("matching API declarations publish their endpoints");
        };
        let apis = apis.apis();
        assert_eq!(apis.len(), 1);
        assert_eq!(apis[0].path.as_str(), "/usage");
        assert_eq!(
            apis[0].response_schema,
            Some(serde_json::json!({"type": "object"})),
            "the declared schema file is published with the endpoint"
        );
        for process_apis in [
            "[]",
            "[{'name':'usage'},{'name':'extra'}]",
            "[{'name':'other'}]",
        ] {
            let result = prepare_api_bundle(declared, process_apis).await;
            assert!(
                matches!(result, Err(CoreError::Invalid(ref detail)) if detail.contains("API endpoints differ")),
                "{process_apis}: the process must declare exactly the manifest apis"
            );
        }
        let result = prepare_api_bundle("", "[{'name':'usage'}]").await;
        assert!(
            matches!(result, Err(CoreError::Invalid(ref detail)) if detail.contains("API endpoints differ")),
            "a process may not serve undeclared endpoints"
        );
    }

    /// A process-backed bundle's `permission_modes:` publish on its runtime
    /// source together with the process hooks that approve them; a bundle
    /// without modes keeps the pre-modes runtime fingerprint.
    #[tokio::test]
    async fn bundle_permission_modes_publish_on_the_runtime_source() {
        let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 result={'protocol_version':1,'plugin':{'id':'approver','version':'1.0.0','kind':'bun'},'hooks':[{'name':'permission.approve'}],'tools':[]} if r.get('method') == 'initialize' else {'outcome':'defer'}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
        let prepared = prepare_package(BundleSource::new(
            "approver",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Plugin\nidentity: { id: acme/approver, version: 1.0.0, publisher: acme }\nextensions:\n  process: { kind: bun, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\n  files: [{ id: runtime, path: runtime.py }]\nresources:\n  hooks: [{ id: permission.approve, path: hooks/approve.json }]\npermission_modes:\n  - { id: careful, title: Careful, description: Approves reads }\n",
                ),
                SourceFile::new("runtime.py", script),
                SourceFile::new("hooks/approve.json", "{}"),
            ],
        ))
        .unwrap();
        let bundle = &prepared.bundles()[0];
        let process = prepared.bundle_process("acme/approver");
        let modes = prepared.bundle_permission_modes("acme/approver");
        assert_eq!(modes.len(), 1);
        let config = BundleRuntimeConfig::capture(bundle, process, test_config("acme/approver"));
        let cached = prepare_source(
            bundle,
            BundleRuntimeParts {
                process,
                schemas: &[],
                apis: &[],
                permission_modes: modes,
                reads: None,
            },
            &config,
        )
        .await
        .unwrap();
        assert_eq!(
            cached.source.permission_modes(),
            [hya_core::RuntimePermissionMode {
                id: "careful".to_string(),
                title: "Careful".to_string(),
                description: "Approves reads".to_string(),
            }]
        );
        assert_ne!(
            fingerprint(bundle, process, &[], &[], modes, &config).unwrap(),
            fingerprint(bundle, process, &[], &[], &[], &config).unwrap(),
            "declared modes are part of the runtime fingerprint"
        );
        let legacy = {
            let config = config.watched.then_some(&config);
            Sha256::digest(
                serde_json::to_vec(&(
                    bundle,
                    process,
                    &[] as &[PreparedSchema],
                    &[] as &[PreparedApi],
                    config,
                ))
                .unwrap(),
            )
        };
        assert_eq!(
            fingerprint(bundle, process, &[], &[], &[], &config).unwrap(),
            <[u8; 32]>::from(legacy),
            "a bundle without modes keeps its previous fingerprint"
        );
    }
}
