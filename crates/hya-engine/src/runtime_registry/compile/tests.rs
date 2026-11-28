#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedResource,
    SourceFile, prepare_package,
};
use hya_proto::{AgentName, ToolName};
use hya_tool::{
    Action, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, Rule, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Self-contained test fixtures and compact builders
// ---------------------------------------------------------------------------

static NEXT_TEMP_SKILL_DIR: AtomicU64 = AtomicU64::new(0);
const WITHOUT_CHANNEL_TOOLS: [&str; 4] = [
    "hya/hashline",
    "hya/coding-extras",
    "hya/multi-agent",
    "hya/network",
];

fn catalog_from(bundles: &[Bundle]) -> Arc<AgentCatalog> {
    Arc::new(AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(bundles).unwrap())).unwrap())
}

fn verified_catalog(catalogs: &[&hya_bundle::PreparedCatalog]) -> Arc<AgentCatalog> {
    Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(catalogs).unwrap(),
        ))
        .unwrap(),
    )
}

fn reg_with(tools: ToolRegistry, catalog: Arc<AgentCatalog>) -> RuntimeRegistry {
    let sources: Vec<_> = catalog
        .bundles()
        .bundles()
        .iter()
        .filter_map(|bundle| {
            let skills: Vec<_> = bundle
                .resources(hya_bundle::ExportKind::Skill)
                .iter()
                .map(|r| {
                    let p = crate::skill_catalog::parse_skill(&r.content).expect("valid skill");
                    let path =
                        PathBuf::from(format!("bundle:{}/{}", bundle.identity().id, r.source_path));
                    let dir = path.parent().unwrap().to_path_buf();
                    RuntimeSourceSkill::new(
                        r.stable_id.clone(),
                        r.local_id.clone(),
                        r.aliases.clone(),
                        r.digest.clone(),
                        r.content.clone(),
                        SkillCatalogEntry {
                            name: p.name,
                            description: p.description,
                            content: p.content,
                            allowed_tools: p.allowed_tools,
                            model: p.model,
                            path,
                            dir,
                            origin: hya_tool::SkillCatalogOrigin::Virtual,
                            disable_model_invocation: false,
                        },
                    )
                })
                .collect();
            (!skills.is_empty()).then(|| {
                RuntimeSource::new(
                    RuntimeSourceId::bundle(bundle.identity().id.clone()),
                    [0; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_skills(skills)
            })
        })
        .collect();
    let reg = RuntimeRegistry::new(tools, catalog).with_skill_discovery(
        crate::skill_catalog::discover,
        crate::skill_catalog::builtin_skills,
    );
    if !sources.is_empty() {
        reg.refresh(|c| c.replace_sources_of_kind(RuntimeSourceKind::Bundle, sources))
            .unwrap();
    }
    reg
}

fn reg_for(bundles: &[Bundle]) -> RuntimeRegistry {
    reg_with(hya_tool_loader::builtins(), catalog_from(bundles))
}

fn bind(reg: &RuntimeRegistry) -> TurnBinding {
    reg.bind_turn(Path::new("/tmp/hya-compile-tests")).unwrap()
}

fn compile_on(
    b: &TurnBinding,
    id: &str,
    plane: Option<AgentToolPlane>,
) -> Result<Arc<CompiledResourceView>, BundleError> {
    let pol = match plane {
        Some(p) => b.agent_resource_policy_on_plane(id, p)?,
        None => b.agent_resource_policy(id)?,
    };
    b.compile_agent_resources(&pol)
}

fn full(b: &TurnBinding, id: &str) -> Arc<CompiledResourceView> {
    compile_on(b, id, Some(AgentToolPlane::Full)).unwrap()
}

fn basic(b: &TurnBinding, id: &str) -> Arc<CompiledResourceView> {
    compile_on(b, id, None).unwrap()
}

struct StubTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl StubTool {
    fn noop(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: "noop".into(),
            input_schema: json!({ "type": "object" }),
        }
    }
    fn marked(name: impl Into<String>, marker: &str) -> Self {
        Self {
            name: name.into(),
            description: format!("fingerprint schema {marker}"),
            input_schema: json!({"type": "object", "properties": {"marker": {"const": marker}}}),
        }
    }
}

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            output_schema: None,
        }
    }
    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

struct ReferenceRecordingTool {
    name: String,
    seen: Mutex<Vec<String>>,
}

impl ReferenceRecordingTool {
    fn new(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            seen: Mutex::new(Vec::new()),
        })
    }
    fn references(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Tool for ReferenceRecordingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "records dispatch references".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if let Some(r) = input.get("reference").and_then(Value::as_str) {
            self.seen.lock().unwrap().push(r.to_string());
        }
        Ok(json!({ "output": "owner body" }))
    }
}

struct TempSkillDir(PathBuf);

impl TempSkillDir {
    fn new(label: &str, name: &str, content: &str) -> Self {
        let serial = NEXT_TEMP_SKILL_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "hya-compile-{label}-{}-{serial}",
            std::process::id()
        ));
        let dir = root.join(".hya/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), content).unwrap();
        Self(root)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempSkillDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn skill_md(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {name}\n---\n{body}\n")
}

fn res(
    bundle_id: &str,
    kind: &str,
    local: &str,
    path: &str,
    content: String,
    aliases: &[&str],
) -> PreparedResource {
    PreparedResource {
        local_id: local.into(),
        stable_id: format!("bundle:{bundle_id}/{kind}/{local}"),
        source_path: path.into(),
        digest: "test-only".into(),
        content,
        binary_base64: None,
        aliases: aliases.iter().map(|a| (*a).into()).collect(),
        overrides: None,
    }
}

fn skill_res(bundle_id: &str, local: &str, body: &str, aliases: &[&str]) -> PreparedResource {
    res(
        bundle_id,
        "skill",
        local,
        &format!("resources/skills/{local}.md"),
        skill_md(local, body),
        aliases,
    )
}

fn agent(id: &str, resource_view: ResourceView) -> PreparedAgent {
    PreparedAgent {
        private: false,
        id: AgentName::new(id),
        description: None,
        role: AgentRole::Main,
        color: None,
        prompt: Some("prompt".into()),
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        resource_view,
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
        advertise: Vec::new(),
        handoff: hya_bundle::HandoffPolicy::Model,
    }
}

fn bundle_with_agent(id: &str, ag: PreparedAgent, skills: Vec<PreparedResource>) -> Bundle {
    Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: id.into(),
            version: "0.0.0".into(),
            publisher: "hya-tests".into(),
        },
        namespace: None,
        digest: "test-only".into(),
        agents: vec![ag],
        tools: Vec::new(),
        skills,
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,
        workflow: None,
        channels: Vec::new(),
        schemas: Vec::new(),
        process: None,
        apis: Vec::new(),
        commands: Vec::new(),
        runtime: None,
        permission_modes: Vec::new(),
        cli: Vec::new(),
        overrides: None,
    }
}

fn one(bundle_id: &str, agent_id: &str, rv: ResourceView, skills: Vec<PreparedResource>) -> Bundle {
    bundle_with_agent(bundle_id, agent(agent_id, rv), skills)
}

fn view(allow: &[&str], deny: &[&str]) -> ResourceView {
    view_with(allow, deny, &[], None)
}

fn view_with(
    allow: &[&str],
    deny: &[&str],
    aliases: &[(&str, &str)],
    ns: Option<&str>,
) -> ResourceView {
    ResourceView {
        allow: allow.iter().map(|s| (*s).into()).collect(),
        deny: deny.iter().map(|s| (*s).into()).collect(),
        aliases: aliases
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        namespace: ns.map(str::to_string),
    }
}

fn export(
    local: &str,
    canonical: &str,
    aliases: &[&str],
    perm: ToolPermission,
) -> RuntimeSourceExport {
    RuntimeSourceExport::tool(
        local,
        canonical,
        aliases.iter().map(|a| (*a).into()).collect(),
        Arc::new(StubTool::noop(canonical)),
        perm,
    )
}

fn mcp_ping(aliases: &[&str], digest: [u8; 32]) -> RuntimeSource {
    RuntimeSource::new(
        RuntimeSourceId::mcp("fixture"),
        digest,
        Arc::new(()),
        vec![export(
            "ping",
            "mcp__fixture__ping",
            aliases,
            ToolPermission::Mcp,
        )],
    )
}

fn compile_bundle_agent(
    tools: ToolRegistry,
    ag: PreparedAgent,
) -> Result<Arc<CompiledResourceView>, BundleError> {
    let id = ag.id.as_str().to_string();
    let mut bundles = vec![bundle_with_agent(
        &format!("hya/coord-{id}"),
        ag,
        Vec::new(),
    )];
    if let Ok(multi_agent) = hya_bundle::first_party_bundle("hya/multi-agent") {
        bundles.extend(multi_agent.bundles().iter().cloned());
    }
    let reg = reg_with(tools, catalog_from(&bundles));
    compile_on(&bind(&reg), &id, None)
}

fn dispatch_ctx(workdir: &Path) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    ToolCtx::new(
        workdir.to_path_buf(),
        permission,
        tokio_util::sync::CancellationToken::new(),
    )
}

fn is_coord(name: &str) -> bool {
    crate::coordination::COORDINATION_TOOLS
        .contains(&name.strip_prefix("harness:tool/").unwrap_or(name))
}

fn has_mail_only_read(c: &CompiledResourceView) -> bool {
    c.resolve_tool("read")
        .is_some_and(|r| r.tool.schema().description.starts_with("Read team mail"))
}

fn schema_names(c: &CompiledResourceView) -> BTreeSet<String> {
    c.tool_schemas()
        .into_iter()
        .map(|s| s.name.as_str().into())
        .collect()
}

fn domain_tool_names(c: &CompiledResourceView) -> BTreeSet<String> {
    let mail = has_mail_only_read(c);
    c.tools
        .keys()
        .filter(|n| !is_coord(n) && (!mail || *n != "read"))
        .cloned()
        .collect()
}

fn domain_schema_names(c: &CompiledResourceView) -> BTreeSet<String> {
    let mail = has_mail_only_read(c);
    schema_names(c)
        .into_iter()
        .filter(|n| !is_coord(n) && (!mail || n != "read"))
        .collect()
}

fn fp_of(snapshot: Arc<RuntimeSnapshot>, workdir: &str, perm: &PermissionPlane) -> [u8; 32] {
    let binding = TurnBinding {
        snapshot,
        agent_model_preferences: Arc::new(BTreeMap::new()),
        agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
        session_agent_models: Arc::new(BTreeMap::new()),
        place: BindingPlace::new(PathBuf::from(workdir), CatalogScope::Global),
    };
    let fp = binding.semantic_fingerprint_v1(perm).unwrap();
    assert_ne!(fp, [0_u8; 32]);
    fp
}

// ---------------------------------------------------------------------------
// 1. Publication lock, pure skills, and TurnBinding pinning
// ---------------------------------------------------------------------------

#[test]
fn skill_discovery_is_unlocked_and_pure_mode_excludes_filesystem_skills() {
    let unlocked = RuntimeRegistry::new(hya_tool_loader::builtins(), catalog_from(&[]));
    let p = Path::new("/skill-discovery-lock-test");
    unlocked
        .bind_scoped_with_skills(&CatalogScope::Directory(p.to_path_buf()), p, || {
            assert!(unlocked.publication.try_lock().is_ok());
            Vec::new()
        })
        .unwrap();

    let dir = TempSkillDir::new(
        "pure-skills",
        "external-skill",
        &skill_md("external-skill", "body"),
    );
    let plain = reg_for(&[]);
    let pure = reg_for(&[]).with_pure_skills(true);
    let names = |b: &TurnBinding| -> Vec<String> {
        b.snapshot
            .skills
            .get(dir.path())
            .map(|s| s.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default()
    };
    assert!(
        names(&plain.bind_turn(dir.path()).unwrap())
            .iter()
            .any(|n| n == "external-skill")
    );
    let pure_names = names(&pure.bind_turn(dir.path()).unwrap());
    assert!(!pure_names.iter().any(|n| n == "external-skill") && !pure_names.is_empty());
}

#[test]
fn turn_binding_and_clones_pin_tools_and_masks_across_refresh() {
    // Part A: Retained TurnBinding does not observe later tool publications.
    let reg = reg_for(&[one(
        "hya/pin",
        "pin-agent",
        ResourceView::default(),
        Vec::new(),
    )]);
    let b = bind(&reg);
    let pol = b
        .agent_resource_policy_on_plane("pin-agent", AgentToolPlane::Full)
        .unwrap();
    let before = b.compile_agent_resources(&pol).unwrap();
    assert!(
        before.resolve_tool("read").is_some() && before.resolve_tool("dynamic_marker").is_none()
    );

    reg.refresh(|c| c.register_tool(Arc::new(StubTool::noop("dynamic_marker"))))
        .unwrap();
    let after = b.compile_agent_resources(&pol).unwrap();
    assert!(after.resolve_tool("dynamic_marker").is_none());
    assert_eq!(
        before.tools.keys().collect::<BTreeSet<_>>(),
        after.tools.keys().collect::<BTreeSet<_>>()
    );

    // Part B: Cloned bindings reuse masked winners after source removal; fresh bindings drop them.
    let mask_reg = reg_for(&[]);
    let sid = RuntimeSourceId::plugin("mask-src");
    mask_reg
        .refresh(|c| {
            c.upsert_sources(vec![RuntimeSource::new(
                sid.clone(),
                [14; 32],
                Arc::new(()),
                vec![export(
                    "fast",
                    "mask__fast",
                    &["bash"],
                    ToolPermission::Tool,
                )],
            )])
        })
        .unwrap();
    let bound = bind(&mask_reg);
    let retained = bound.clone();
    let main_pol = bound.agent_resource_policy("hya-main").unwrap();
    let masked_before = bound.compile_agent_resources(&main_pol).unwrap();
    let masked_bash = masked_before.resolve_tool("bash").unwrap().tool;
    assert!(
        masked_before
            .resolve_tool("harness:tool/mask__fast")
            .is_some()
    );

    mask_reg
        .refresh(|c| {
            c.remove_sources(&BTreeSet::from([sid]));
            Ok(())
        })
        .unwrap();
    let fresh = bind(&mask_reg);
    let fresh_view = basic(&fresh, "hya-main");
    assert!(fresh_view.resolve_tool("harness:tool/mask__fast").is_none());
    assert!(fresh_view.resolve_tool("harness:tool/bash").is_some());
    assert!(
        fresh
            .compile_agent_resources(&main_pol)
            .unwrap()
            .resolve_tool("harness:tool/mask__fast")
            .is_none()
    );

    let retained_view = basic(&retained, "hya-main");
    assert!(
        retained_view
            .resolve_tool("harness:tool/mask__fast")
            .is_some()
    );
    assert!(Arc::ptr_eq(
        &masked_bash,
        &retained_view.resolve_tool("bash").unwrap().tool
    ));
}

// ---------------------------------------------------------------------------
// 2. Bundle owner scoping: skills, process tools, and bundled MCP
// ---------------------------------------------------------------------------

#[test]
fn bundle_owner_scoping_for_plugin_skills_and_process_tools() {
    let prep = |id: &str, name: &str, ag: &str| {
        prepare_package(BundleSource::new(
            id,
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    format!(
                        "schema_version: 2\nkind: Bundle\nidentity: {{ id: acme/{id}, version: 1.0.0, publisher: acme }}\nresources:\n  skills: [{{ id: {name}, path: skills/guide.md }}]\n{ag}"
                    ),
                ),
                SourceFile::new("skills/guide.md", skill_md(name, "SKILL_BODY")),
            ],
        ))
        .unwrap()
    };
    let plugin = prep("shared", "shared-skill", "");
    let private = prep(
        "private",
        "private-skill",
        "agents: [{ id: private-agent, role: main }]\n",
    );
    let process = prepare_package(BundleSource::new(
        "private-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                "schema_version: 2\nkind: Bundle\nidentity: { id: acme/private-process, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [fixture] }\nresources:\n  tools: [{ id: echo, path: echo.json }]\n  hooks: [{ id: tool.execute.before, path: echo.json }]\nagents:\n  - { id: private-process-agent, role: main, hook_refs: [tool.execute.before], resource_view: { allow: [echo] } }\n",
            ),
            SourceFile::new("echo.json", "{}"),
        ],
    ))
    .unwrap();

    let reg = reg_with(
        hya_tool_loader::builtins(),
        verified_catalog(&[&plugin, &private, &process]),
    );
    let mut unknown = reg.active().sources[&RuntimeSourceId::bundle("acme/shared")].clone();
    unknown.id = RuntimeSourceId::bundle("acme/unknown");
    unknown.skills[0].stable_id = "bundle:acme/unknown/skill/unknown-skill".into();
    unknown.skills[0].local_id = "unknown-skill".into();
    unknown.skills[0].entry.name = "unknown-skill".into();
    reg.refresh(|c| {
        c.upsert_sources(vec![
            unknown,
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/private-process"),
                [2; 32],
                Arc::new(()),
                vec![export(
                    "echo",
                    "private-process__echo",
                    &[],
                    ToolPermission::Tool,
                )],
            )
            .with_hooks(Arc::new(PrivateProcessHook)),
        ])
    })
    .unwrap();

    let b = bind(&reg);
    let main_view = full(&b, "hya-main");
    let main_skills: BTreeSet<&str> = main_view.skills().iter().map(|s| s.name.as_str()).collect();
    assert!(
        main_skills.contains("shared-skill")
            && main_skills.contains("bundle:acme/shared/skill/shared-skill")
    );
    assert!(!main_skills.contains("private-skill") && !main_skills.contains("unknown-skill"));
    assert!(main_view.resolve_tool("private-process__echo").is_none());
    assert!(b.bundle_hooks_for_agent("hya-main").is_empty());

    let priv_view = basic(&b, "private-agent");
    assert!(priv_view.skills().iter().any(|s| s.name == "private-skill"));
    assert!(!priv_view.skills().iter().any(|s| s.name == "shared-skill"));

    let proc_view = basic(&b, "private-process-agent");
    assert!(proc_view.resolve_tool("echo").is_some());
    assert_eq!(b.bundle_hooks_for_agent("private-process-agent").len(), 1);
    assert!(
        !b.has_selected_bundle_sidecar_capability("private-process-agent")
            .unwrap()
    );
}

#[test]
fn bundled_mcp_partitioning_and_owner_server_expansion() {
    let shared_mcp = prepare_package(BundleSource::new(
        "bundled-mcp",
        vec![SourceFile::new(
            "bundle.yaml",
            "schema_version: 2\nkind: Bundle\nidentity: { id: acme/bundled-mcp, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let private_mcp = prepare_package(BundleSource::new(
        "private-mcp",
        vec![
            SourceFile::new(
                "bundle.yaml",
                "schema_version: 2\nkind: Bundle\nidentity: { id: acme/private-mcp, version: 1.0.0, publisher: acme }\nresources:\n  mcp: [{ id: echo, path: mcp.json }]\nagents:\n  - { id: private-mcp-agent, role: main, resource_view: { allow: [echo] } }\n",
            ),
            SourceFile::new("mcp.json", r#"{"command":["fixture"]}"#),
        ],
    ))
    .unwrap();
    let reg = reg_with(
        hya_tool_loader::builtins(),
        verified_catalog(&[&shared_mcp, &private_mcp]),
    );
    reg.refresh(|c| {
        c.upsert_sources(vec![
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/bundled-mcp"),
                [1; 32],
                Arc::new(()),
                vec![export(
                    "mcp/echo/ping",
                    "bundled-mcp__mcp__echo__ping",
                    &[],
                    ToolPermission::Mcp,
                )],
            ),
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/private-mcp"),
                [3; 32],
                Arc::new(()),
                vec![export(
                    "mcp/echo/ping",
                    "private-mcp__mcp__echo__ping",
                    &[],
                    ToolPermission::Mcp,
                )],
            ),
        ])
    })
    .unwrap();

    let b = bind(&reg);
    let main_pol = b.agent_resource_policy("hya-main").unwrap();
    let cand = b.collect_resource_candidates(&main_pol).unwrap();
    assert!(
        cand.mcp
            .contains_key("harness:mcp/bundled-mcp__mcp__echo__ping")
    );
    assert!(
        !cand
            .tool
            .contains_key("harness:tool/bundled-mcp__mcp__echo__ping")
    );
    assert!(
        b.compile_agent_resources(&main_pol)
            .unwrap()
            .resolve_tool("private-mcp__mcp__echo__ping")
            .is_none()
    );

    let owner = basic(&b, "private-mcp-agent");
    assert_eq!(
        owner.resolve_tool("echo__ping").unwrap().permission,
        ToolPermission::Mcp
    );
    assert_eq!(
        domain_schema_names(&owner),
        BTreeSet::from(["echo__ping".into()])
    );
}

// ---------------------------------------------------------------------------
// 3. Alias masking (built-in override, dissolution, cross-source precedence)
// ---------------------------------------------------------------------------

#[test]
fn alias_masking_builtin_override_dissolution_and_cross_source_precedence() {
    let reg = reg_for(&[one(
        "hya/mask",
        "mask-agent",
        ResourceView::default(),
        Vec::new(),
    )]);
    let plugin = |id: &str, d: [u8; 32], loc: &str, can: &str, alias: &str| {
        RuntimeSource::new(
            RuntimeSourceId::plugin(id),
            d,
            Arc::new(()),
            vec![export(loc, can, &[alias], ToolPermission::Tool)],
        )
    };
    reg.refresh(|c| {
        c.upsert_sources(vec![
            plugin("mask-src", [14; 32], "fast", "mask__fast", "bash"),
            plugin("alpha", [16; 32], "lookup", "alpha__lookup", "lookup"),
            plugin("beta", [17; 32], "lookup", "beta__lookup", "lookup"),
        ])
    })
    .expect("masks must publish");

    let b = bind(&reg);
    assert_eq!(
        b.masks().get("bash").map(String::as_str),
        Some("mask__fast")
    );
    assert_eq!(
        b.masks().get("lookup").map(String::as_str),
        Some("beta__lookup")
    );

    let compiled = full(&b, "mask-agent");
    let names: BTreeSet<&str> = compiled.tools.keys().map(String::as_str).collect();
    assert!(names.contains("bash") && !names.contains("harness:tool/bash"));
    assert!(names.contains("harness:tool/mask__fast"));
    assert!(names.contains("lookup") && names.contains("harness:tool/beta__lookup"));
    assert!(names.contains("alpha__lookup") && compiled.resolve_tool("alpha__lookup").is_some());

    let schemas = compiled.tool_schemas();
    for bare in ["bash", "lookup"] {
        let m: Vec<_> = schemas.iter().filter(|s| s.name.as_str() == bare).collect();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].input_schema, json!({ "type": "object" }));
    }
    assert!(!schemas.iter().any(|s| s.name.as_str() == "beta__lookup"));

    reg.refresh(|c| {
        c.remove_sources(&BTreeSet::from([RuntimeSourceId::plugin("mask-src")]));
        Ok(())
    })
    .unwrap();
    let b2 = bind(&reg);
    assert!(!b2.masks().contains_key("bash"));
    assert!(
        full(&b2, "mask-agent")
            .tools
            .contains_key("harness:tool/bash")
    );
}

// ---------------------------------------------------------------------------
// 4. Dispatch identity and runtime semantic fingerprints
// ---------------------------------------------------------------------------

#[test]
fn dispatch_identity_and_base_semantic_fingerprint_invariants() {
    let source_id = |id: &str, digest: [u8; 32], res: Value| {
        let reg = reg_for(&[one(
            "hya/source-id",
            "sid",
            ResourceView::default(),
            Vec::new(),
        )]);
        reg.refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::plugin(id),
                    digest,
                    Arc::new(()),
                    vec![export(
                        "probe",
                        "plugin__fixture__probe",
                        &[],
                        ToolPermission::Tool,
                    )],
                )
                .with_resources(BTreeMap::from([("probe".into(), res)])),
            ])
        })
        .unwrap();
        let did = reg
            .active()
            .tools
            .dispatch_identity_v1("plugin__fixture__probe")
            .unwrap();
        assert_ne!(did, [0_u8; 32]);
        did
    };
    let base_did = source_id("fixture", [1; 32], json!({"mode": "one"}));
    assert_eq!(
        source_id("fixture", [1; 32], json!({"mode": "one"})),
        base_did
    );
    for (lbl, id, dig, res) in [
        ("id", "fixture-other", [1; 32], json!({"mode": "one"})),
        ("digest", "fixture", [2; 32], json!({"mode": "one"})),
        ("resource", "fixture", [1; 32], json!({"mode": "two"})),
    ] {
        assert_ne!(source_id(id, dig, res), base_did, "{lbl}");
    }

    let next_gen = ConfigGeneration::INITIAL.checked_next().unwrap();
    let base_fp = |cat: &str,
                   sch: &str,
                   rev: bool,
                   did: [u8; 32],
                   mode: Mode,
                   generation: ConfigGeneration| {
        let pkg = prepare_package(BundleSource::new(
            "rf",
            vec![SourceFile::new(
                "bundle.yaml",
                format!("schema_version: 2\nkind: Bundle\nidentity: {{ id: hya/rf, version: 1.0.0, publisher: hya-tests }}\nagents: [{{ id: fp, description: \"manifest {cat}\", role: main }}]\n"),
            )],
        ))
        .unwrap();
        let tr = hya_tool_loader::builtins();
        let mut custom: Vec<(Arc<dyn Tool>, [u8; 32])> = vec![
            (Arc::new(StubTool::marked("custom_one", sch)), did),
            (Arc::new(StubTool::marked("custom_two", "stable")), [2; 32]),
        ];
        if rev {
            custom.reverse();
        }
        for (t, d) in custom {
            tr.register_with_permission_and_aliases_and_dispatch_identity(
                t,
                ToolPermission::Tool,
                &[],
                d,
            )
            .unwrap();
        }
        let pol = InvocationPolicy::compile(
            PermissionModel::Default,
            vec![InvocationRule::new(
                PermissionTarget::Tool,
                "^custom_one$",
                Mode::Allow,
            )],
        )
        .unwrap();
        let (perm, _asks) = PermissionPlane::new_with_policy(
            PermissionRules::new(vec![Rule::new(Action::Tool, "custom_one", mode)]),
            pol,
        );
        let tools = tr.snapshot();
        fp_of(
            Arc::new(RuntimeSnapshot {
                generation,
                catalog: verified_catalog(&[&pkg]),
                basic_tools: tools.clone(),
                tools,
                skills: BTreeMap::new(),
                sources: BTreeMap::new(),
                masks: Arc::new(BTreeMap::new()),
                schemes: Arc::new(BTreeMap::new()),
                native_bundle_apis: Arc::new(std::sync::RwLock::new(BTreeMap::new())),
            }),
            "/tmp/rf",
            &perm,
        )
    };

    let base = base_fp(
        "one",
        "one",
        false,
        [1; 32],
        Mode::Allow,
        ConfigGeneration::INITIAL,
    );
    assert_eq!(
        base_fp("one", "one", true, [1; 32], Mode::Allow, next_gen),
        base
    );
    for (lbl, cat, sch, did, mode) in [
        ("catalog", "two", "one", [1; 32], Mode::Allow),
        ("schema", "one", "two", [1; 32], Mode::Allow),
        ("dispatch", "one", "one", [3; 32], Mode::Allow),
        ("permission", "one", "one", [1; 32], Mode::Deny),
    ] {
        assert_ne!(
            base_fp(cat, sch, false, did, mode, ConfigGeneration::INITIAL),
            base,
            "{lbl}"
        );
    }
}

#[test]
fn semantic_fingerprint_tracks_selected_workdir_skills() {
    let mk =
        |name: &str, desc: &str, content: &str, tools: &[&str], model: Option<&str>, path: &str| {
            SkillCatalogEntry {
                name: name.into(),
                description: desc.into(),
                content: content.into(),
                allowed_tools: tools.iter().map(|t| (*t).into()).collect(),
                model: model.map(str::to_string),
                path: PathBuf::from(path),
                dir: PathBuf::from(path)
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf),
                origin: hya_tool::SkillCatalogOrigin::Filesystem,
                disable_model_invocation: false,
            }
        };
    let selected = || {
        vec![
            mk(
                "alpha",
                "Alpha",
                "alpha body",
                &["custom_one"],
                Some("model-a"),
                "/tmp/rf-sk/alpha/SKILL.md",
            ),
            mk(
                "beta",
                "Beta",
                "beta body",
                &[],
                None,
                "/tmp/rf-sk/beta/SKILL.md",
            ),
        ]
    };
    let pkg = prepare_package(BundleSource::new(
        "rf-sk",
        vec![SourceFile::new("bundle.yaml", "schema_version: 2\nkind: Bundle\nidentity: { id: hya/rf-sk, version: 1.0.0, publisher: hya-tests }\nagents: [{ id: fp-sk, role: main }]\n")],
    ))
    .unwrap();
    let fp_for = |sel: Vec<SkillCatalogEntry>, unrelated: Vec<SkillCatalogEntry>| {
        let tools = hya_tool_loader::builtins().snapshot();
        let mut skills = BTreeMap::from([(PathBuf::from("/tmp/rf-sk"), Arc::new(sel))]);
        if !unrelated.is_empty() {
            skills.insert(PathBuf::from("/tmp/rf-other"), Arc::new(unrelated));
        }
        let (perm, _) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::default(),
        );
        fp_of(
            Arc::new(RuntimeSnapshot {
                generation: ConfigGeneration::INITIAL,
                catalog: verified_catalog(&[&pkg]),
                basic_tools: tools.clone(),
                tools,
                skills,
                sources: BTreeMap::new(),
                masks: Arc::new(BTreeMap::new()),
                schemes: Arc::new(BTreeMap::new()),
                native_bundle_apis: Arc::new(std::sync::RwLock::new(BTreeMap::new())),
            }),
            "/tmp/rf-sk",
            &perm,
        )
    };

    let base = fp_for(selected(), Vec::new());
    assert_eq!(fp_for(selected(), Vec::new()), base);
    assert_eq!(
        fp_for(
            selected(),
            vec![mk("u", "u", "u", &[], None, "/tmp/rf-other/u/SKILL.md")]
        ),
        base
    );

    type SkillMutator = fn(&mut Vec<SkillCatalogEntry>);
    let muts: [(&str, SkillMutator); 7] = [
        ("content", |s| s[0].content = "changed".into()),
        ("name", |s| s[0].name = "alpha-renamed".into()),
        ("path_and_dir", |s| {
            s[0].path = PathBuf::from("/tmp/rf-sk/moved/SKILL.md");
            s[0].dir = PathBuf::from("/tmp/rf-sk/moved");
        }),
        ("description", |s| s[0].description = "changed".into()),
        ("allowed_tools", |s| {
            s[0].allowed_tools = vec!["custom_two".into()]
        }),
        ("model", |s| s[0].model = Some("model-b".into())),
        ("order", |s| s.reverse()),
    ];
    for (lbl, f) in muts {
        let mut entries = selected();
        f(&mut entries);
        assert_ne!(fp_for(entries, Vec::new()), base, "skill `{lbl}`");
    }
}

#[test]
fn semantic_fingerprint_tracks_plugin_and_mcp_sources() {
    let nested = |marker: &str, rev: bool| {
        let mut inner = serde_json::Map::new();
        let pairs = [
            ("marker", Value::String(marker.into())),
            ("enabled", Value::Bool(true)),
        ];
        let mut outer = serde_json::Map::new();
        if rev {
            pairs.into_iter().rev().for_each(|(k, v)| {
                inner.insert(k.into(), v);
            });
            outer.insert("nested".into(), Value::Object(inner));
            outer.insert("version".into(), Value::from(1));
        } else {
            pairs.into_iter().for_each(|(k, v)| {
                inner.insert(k.into(), v);
            });
            outer.insert("version".into(), Value::from(1));
            outer.insert("nested".into(), Value::Object(inner));
        }
        Value::Object(outer)
    };
    let pkg = prepare_package(BundleSource::new(
        "rf-src",
        vec![SourceFile::new("bundle.yaml", "schema_version: 2\nkind: Bundle\nidentity: { id: hya/rf-src, version: 1.0.0, publisher: hya-tests }\nagents: [{ id: fp-src, role: main }]\n")],
    ))
    .unwrap();
    #[derive(Clone, Copy)]
    struct C {
        kind: RuntimeSourceKind,
        pid: &'static str,
        pdig: [u8; 32],
        pm: &'static str,
        mdig: [u8; 32],
        mm: &'static str,
        mcp: bool,
        rev: bool,
    }
    let fp_for = |c: C| {
        let reg = reg_with(hya_tool_loader::builtins(), verified_catalog(&[&pkg]));
        let probe = match c.kind {
            RuntimeSourceKind::Mcp => format!("mcp__{}__probe", c.pid),
            _ => format!("plugin__{}__probe", c.pid),
        };
        let mut sources = vec![
            RuntimeSource::new(
                RuntimeSourceId::new(c.kind, c.pid),
                c.pdig,
                Arc::new(()),
                vec![export(
                    "probe",
                    &probe,
                    &["plugin_probe"],
                    ToolPermission::Tool,
                )],
            )
            .with_resources(BTreeMap::from([("config".into(), nested(c.pm, c.rev))])),
        ];
        if c.mcp {
            sources.push(
                RuntimeSource::new(
                    RuntimeSourceId::mcp("fixture-mcp"),
                    c.mdig,
                    Arc::new(()),
                    Vec::new(),
                )
                .with_resources(BTreeMap::from([("config".into(), nested(c.mm, c.rev))])),
            );
        }
        if c.rev {
            sources.reverse();
        }
        reg.refresh(|cand| cand.upsert_sources(sources)).unwrap();
        let (perm, _) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::default(),
        );
        fp_of(reg.active(), "/tmp/rf-src", &perm)
    };
    let base_c = C {
        kind: RuntimeSourceKind::Plugin,
        pid: "fixture-plugin",
        pdig: [1; 32],
        pm: "plugin-one",
        mdig: [2; 32],
        mm: "mcp-one",
        mcp: true,
        rev: false,
    };
    let base = fp_for(base_c);
    assert_eq!(
        fp_for(C {
            rev: true,
            ..base_c
        }),
        base
    );
    for (lbl, c) in [
        (
            "kind",
            C {
                kind: RuntimeSourceKind::Mcp,
                ..base_c
            },
        ),
        (
            "plugin_id",
            C {
                pid: "fixture-plugin-other",
                ..base_c
            },
        ),
        (
            "plugin_digest",
            C {
                pdig: [3; 32],
                ..base_c
            },
        ),
        (
            "plugin_res",
            C {
                pm: "plugin-two",
                ..base_c
            },
        ),
        (
            "mcp_digest",
            C {
                mdig: [3; 32],
                ..base_c
            },
        ),
        (
            "mcp_res",
            C {
                mm: "mcp-two",
                ..base_c
            },
        ),
        (
            "without_mcp",
            C {
                mcp: false,
                ..base_c
            },
        ),
    ] {
        assert_ne!(fp_for(c), base, "`{lbl}`");
    }
}

// ---------------------------------------------------------------------------
// 5. Alias and canonical reference error contracts
// ---------------------------------------------------------------------------

#[test]
fn alias_error_contracts_reject_missing_colliding_and_invalid_names() {
    let check = |label: &str,
                 bundle: &str,
                 plane: Option<AgentToolPlane>,
                 rv: ResourceView,
                 dyn_tool: bool,
                 mcp: bool| {
        let reg = reg_for(&[one(bundle, "target", rv, Vec::new())]);
        if dyn_tool {
            reg.refresh(|c| c.register_tool(Arc::new(StubTool::noop("dynamic_marker"))))
                .unwrap();
        }
        if mcp {
            reg.refresh(|c| c.upsert_sources(vec![mcp_ping(&[], [1; 32])]))
                .unwrap();
        }
        compile_on(&bind(&reg), "target", plane)
            .err()
            .unwrap_or_else(|| panic!("{label}: expected compile error"))
    };
    let full_p = Some(AgentToolPlane::Full);

    assert!(matches!(
        check(
            "filtered-plane",
            "hya/alias-miss",
            None,
            view_with(&[], &[], &[("marker", "harness:tool/dynamic_marker")], None),
            true,
            false
        ),
        BundleError::UnknownResourceReference { .. }
    ));
    assert!(matches!(
        check(
            "deny-target",
            "hya/alias-denied",
            full_p,
            view_with(
                &[],
                &["harness:tool/read"],
                &[("reader", "harness:tool/read")],
                None
            ),
            false,
            false
        ),
        BundleError::UnknownResourceReference { .. }
    ));
    for (lbl, bid, allow, alias, target, exp_name, exp_bid) in [
        (
            "qualified-override",
            "hya/a-qual",
            &[][..],
            "harness:tool/read",
            "harness:tool/write",
            "harness:tool/read",
            None,
        ),
        (
            "short-collide",
            "hya/alias-collide",
            &[][..],
            "read",
            "harness:tool/write",
            "read",
            Some("hya/alias-collide"),
        ),
        (
            "same-target-short",
            "hya/a-self",
            &[][..],
            "read",
            "harness:tool/read",
            "read",
            None,
        ),
        (
            "candidate-alias-same",
            "hya/a-cand",
            &[][..],
            "patch",
            "harness:tool/apply_patch",
            "patch",
            None,
        ),
        (
            "filtered-stable",
            "hya/a-filt",
            &["harness:tool/read"][..],
            "write",
            "harness:tool/read",
            "write",
            None,
        ),
    ] {
        let err = check(
            lbl,
            bid,
            full_p,
            view_with(allow, &[], &[(alias, target)], None),
            false,
            false,
        );
        assert!(
            matches!(&err, BundleError::AliasCollision { bundle_id, name } if name == exp_name && exp_bid.is_none_or(|b| bundle_id == b)),
            "{lbl}: got {err:?}"
        );
    }
    let bad_err = check(
        "invalid-provider",
        "hya/a-bad",
        full_p,
        view_with(
            &["harness:tool/read"],
            &[],
            &[("bad:name", "harness:tool/read")],
            None,
        ),
        false,
        false,
    );
    assert!(
        matches!(&bad_err, BundleError::InvalidManifest { detail, .. } if detail.contains("model-facing tool name `bad:name`"))
    );

    let mcp_err = check(
        "tool-mcp-collide",
        "hya/a-mcp",
        full_p,
        view_with(
            &["harness:tool/read", "harness:mcp/mcp__fixture__ping"],
            &[],
            &[("mcp__fixture__ping", "harness:tool/read")],
            None,
        ),
        false,
        true,
    );
    assert!(matches!(
        &mcp_err,
        BundleError::NamespaceCollision { name, .. } | BundleError::AliasCollision { name, .. } if name == "mcp__fixture__ping"
    ));
}

#[test]
fn canonical_reference_rejects_wrong_kind_and_cross_kind_ambiguity() {
    let reg = reg_for(&[one(
        "hya/global-ref",
        "wrong-kind",
        view(&["harness:skill/read"], &[]),
        Vec::new(),
    )]);
    let err = compile_on(&bind(&reg), "wrong-kind", Some(AgentToolPlane::Full))
        .err()
        .unwrap();
    assert!(
        matches!(&err, BundleError::UnknownResourceReference { reference, .. } if reference == "harness:skill/read")
    );

    for (label, rv) in [
        ("allow-short", view(&["shared"], &[])),
        (
            "alias-target",
            view_with(&[], &[], &[("marker", "shared")], None),
        ),
    ] {
        let reg = reg_for(&[one(
            "hya/ambig",
            "ambig",
            rv,
            vec![skill_res("hya/ambig", "shared", "LOCAL", &[])],
        )]);
        reg.refresh(|c| c.register_tool(Arc::new(StubTool::noop("shared"))))
            .unwrap();
        let err = compile_on(&bind(&reg), "ambig", Some(AgentToolPlane::Full))
            .err()
            .unwrap();
        assert!(
            matches!(&err, BundleError::NamespaceCollision { name, .. } if name == "shared"),
            "{label}: {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Positive aliasing, namespaces, builtin aliases, MCP & cross-kind sharing
// ---------------------------------------------------------------------------

#[test]
fn explicit_aliases_namespace_and_candidate_alias_suppression() {
    let reg = reg_for(&[
        one(
            "hya/qualified-map",
            "q-agent",
            view_with(
                &[
                    "harness:tool/read",
                    "harness:tool/write",
                    "bundle:hya/qualified-map/skill/probe",
                ],
                &[],
                &[
                    ("reader", "harness:tool/read"),
                    ("book", "harness:tool/read"),
                ],
                Some("custom.ns"),
            ),
            vec![skill_res("hya/qualified-map", "probe", "LOCAL_PROBE", &[])],
        ),
        one(
            "hya/view-alias-suppress",
            "suppress-agent",
            view_with(&[], &[], &[("applier", "harness:tool/apply_patch")], None),
            Vec::new(),
        ),
    ]);
    let b = bind(&reg);

    let compiled = full(&b, "q-agent");
    assert_eq!(
        domain_schema_names(&compiled),
        BTreeSet::from(["book".into(), "reader".into(), "write".into()])
    );
    for ok in [
        "reader",
        "book",
        "write",
        "harness:tool/read",
        "harness:tool/write",
    ] {
        assert!(compiled.resolve_tool(ok).is_some(), "missing {ok}");
    }
    assert!(compiled.resolve_tool("read").is_none());
    let skills: BTreeSet<&str> = compiled.skills().iter().map(|s| s.name.as_str()).collect();
    assert!(skills.contains("probe") && skills.contains("bundle:custom.ns/skill/probe"));

    // Explicit view alias suppresses preexisting candidate aliases (e.g. `patch`).
    let suppressed = full(&b, "suppress-agent");
    assert!(suppressed.resolve_tool("applier").is_some());
    assert!(
        suppressed
            .resolve_tool("harness:tool/apply_patch")
            .is_some()
    );
    assert!(
        suppressed.resolve_tool("apply_patch").is_none()
            && suppressed.resolve_tool("patch").is_none()
    );
    let names = schema_names(&suppressed);
    assert!(names.contains("applier"));
    assert!(
        !names.contains("apply_patch")
            && !names.contains("patch")
            && !names.contains("harness:tool/apply_patch")
    );
    assert!(
        names
            .iter()
            .all(|n| suppressed.resolve_tool(n).is_some() && !n.contains([':', '/']))
    );
}

#[test]
fn both_planes_keep_builtin_aliases_out_of_model_schemas() {
    let mut expected: BTreeSet<String> = hya_tool_loader::builtins()
        .schemas()
        .into_iter()
        .map(|s| s.name.as_str().into())
        .collect();
    assert_eq!(expected.len(), 23);
    expected.insert(crate::kernel::tools::SKILL.to_string());
    expected.insert(crate::kernel::tools::BASH.to_string());
    for name in crate::coordination::SPAWN_TOOLS {
        expected.remove(name);
    }
    for (id, plane) in [
        ("full-alias", AgentToolPlane::Full),
        ("internal-public-alias", AgentToolPlane::InternalPublic),
    ] {
        let reg = reg_for(&[one(
            "hya/builtin-alias",
            id,
            ResourceView::default(),
            Vec::new(),
        )]);
        let compiled = compile_on(&bind(&reg), id, Some(plane)).unwrap();
        assert!(compiled.resolve_tool("apply_patch").is_some());
        for alias in ["fetch", "search", "patch", "plan", "shell", "question"] {
            assert!(compiled.resolve_tool(alias).is_some(), "{id}: `{alias}`");
        }
        let names = schema_names(&compiled);
        assert_eq!(names, expected, "{id}");
        assert!(!names.contains("patch") && !names.contains("shell"));
        assert!(names.iter().all(|n| !n.contains([':', '/'])));
    }
}

#[test]
fn harness_mcp_plane_visibility_aliases_filtering_and_pinning() {
    let reg = reg_for(&[
        one(
            "hya/mcp-full",
            "full-mcp",
            ResourceView::default(),
            Vec::new(),
        ),
        one(
            "hya/mcp-basic",
            "basic-mcp",
            ResourceView::default(),
            Vec::new(),
        ),
        one(
            "hya/mcp-allow",
            "allow-mcp",
            view(&["harness:mcp/mcp__fixture__ping"], &[]),
            Vec::new(),
        ),
        one(
            "hya/mcp-deny",
            "deny-mcp",
            view(&[], &["harness:mcp/mcp__fixture__ping"]),
            Vec::new(),
        ),
    ]);
    reg.refresh(|c| c.upsert_sources(vec![mcp_ping(&["pingy"], [7; 32])]))
        .unwrap();
    let b = bind(&reg);

    let f = full(&b, "full-mcp");
    assert!(f.resolve_tool("mcp__fixture__ping").is_some() && f.resolve_tool("pingy").is_some());
    assert!(f.resolve_tool("harness:mcp/mcp__fixture__ping").is_some());
    assert!(f.resolve_tool("harness:tool/mcp__fixture__ping").is_none());
    let f_schemas = schema_names(&f);
    assert!(f_schemas.contains("mcp__fixture__ping"));
    assert!(!f_schemas.contains("pingy") && !f_schemas.contains("harness:mcp/mcp__fixture__ping"));
    assert!(f_schemas.iter().all(|n| f.resolve_tool(n).is_some()));

    let bas = basic(&b, "basic-mcp");
    assert!(
        bas.resolve_tool("mcp__fixture__ping").is_none()
            && bas.resolve_tool("harness:mcp/mcp__fixture__ping").is_none()
    );

    let allow = full(&b, "allow-mcp");
    assert!(
        allow.resolve_tool("mcp__fixture__ping").is_some()
            && !domain_tool_names(&allow).contains("read")
    );

    let deny = full(&b, "deny-mcp");
    assert!(
        deny.resolve_tool("mcp__fixture__ping").is_none() && deny.resolve_tool("read").is_some()
    );

    // Pinning: removing MCP source keeps old TurnBinding intact while fresh binding drops it.
    reg.refresh(|c| {
        c.remove_sources(&BTreeSet::from([RuntimeSourceId::mcp("fixture")]));
        Ok(())
    })
    .unwrap();
    assert!(
        full(&b, "full-mcp")
            .resolve_tool("mcp__fixture__ping")
            .is_some()
    );
    assert!(
        full(&bind(&reg), "full-mcp")
            .resolve_tool("mcp__fixture__ping")
            .is_none()
    );
}

#[test]
fn tool_or_mcp_and_skill_may_share_public_spellings_independently() {
    let reg = reg_for(&[
        one(
            "hya/tool-skill-ok",
            "tool-skill",
            view(
                &["harness:tool/read", "bundle:hya/tool-skill-ok/skill/read"],
                &[],
            ),
            vec![skill_res(
                "hya/tool-skill-ok",
                "read",
                "SKILL_READ_BODY",
                &[],
            )],
        ),
        one(
            "hya/mcp-skill-ok",
            "mcp-skill",
            view(
                &[
                    "harness:mcp/mcp__fixture__ping",
                    "bundle:hya/mcp-skill-ok/skill/pingy",
                ],
                &[],
            ),
            vec![skill_res(
                "hya/mcp-skill-ok",
                "pingy",
                "SKILL_PINGY_BODY",
                &[],
            )],
        ),
    ]);
    reg.refresh(|c| c.upsert_sources(vec![mcp_ping(&["pingy"], [5; 32])]))
        .unwrap();
    let b = bind(&reg);

    for (id, shared, tool_name, body) in [
        ("tool-skill", "read", "read", "SKILL_READ_BODY"),
        (
            "mcp-skill",
            "pingy",
            "mcp__fixture__ping",
            "SKILL_PINGY_BODY",
        ),
    ] {
        let c = full(&b, id);
        assert_eq!(
            c.resolve_tool(shared).unwrap().tool.name(),
            tool_name,
            "{id}"
        );
        assert!(
            c.skills()
                .iter()
                .any(|s| s.name == shared && s.content.contains(body)),
            "{id}"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. Skill precedence, aliases, nested bundle IDs, inlining, and facade rules
// ---------------------------------------------------------------------------

#[test]
fn local_skill_precedence_filtering_aliases_and_nested_bundle_ids() {
    let wd = TempSkillDir::new("skill-collision", "shared", &skill_md("shared", "HARNESS"));
    let def_reg = reg_for(&[one(
        "hya/skill-collision",
        "skill-agent",
        ResourceView::default(),
        vec![skill_res("hya/skill-collision", "shared", "LOCAL", &[])],
    )]);
    let def = full(&def_reg.bind_turn(wd.path()).unwrap(), "skill-agent");
    assert!(
        def.skills()
            .iter()
            .any(|s| s.name == "harness:skill/shared")
    );
    assert!(
        def.skills()
            .iter()
            .any(|s| s.name == "shared" && s.content.contains("LOCAL"))
    );

    let filt_reg = reg_for(&[one(
        "hya/skill-collision",
        "skill-agent",
        view(&["harness:skill/shared", "harness:tool/skill"], &[]),
        vec![skill_res("hya/skill-collision", "shared", "LOCAL", &[])],
    )]);
    let filt = full(&filt_reg.bind_turn(wd.path()).unwrap(), "skill-agent");
    assert_eq!(
        filt.skills()
            .iter()
            .map(|s| s.name.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["shared", "harness:skill/shared"])
    );
    assert!(
        filt.skills()
            .iter()
            .any(|s| s.name == "shared" && s.content.contains("HARNESS"))
    );

    // PreparedResource aliases & bundle IDs with kind segments (`tool/skill/mcp`) parse structurally.
    let nested_id = "hya/tool/skill/mcp-nest";
    let nested_stable = format!("bundle:{nested_id}/skill/docs");
    let nested_reg = reg_for(&[one(
        nested_id,
        "nested-agent",
        view(&[&nested_stable], &[]),
        vec![skill_res(
            nested_id,
            "docs",
            "NESTED_BUNDLE_BODY",
            &["handbook"],
        )],
    )]);
    let nested = basic(&bind(&nested_reg), "nested-agent");
    for name in ["docs", "handbook", nested_stable.as_str()] {
        assert!(
            nested
                .skills()
                .iter()
                .any(|s| s.name == name && s.content.contains("NESTED_BUNDLE_BODY")),
            "missing `{name}`"
        );
    }
}

#[test]
fn bundle_local_skill_inlining_and_prompt_spelling_dedup() {
    let stable = "bundle:hya/none-inline/skill/bundle-skill";
    let reg = reg_for(&[one(
        "hya/none-inline",
        "none-agent",
        view(&[stable], &[]),
        vec![skill_res(
            "hya/none-inline",
            "bundle-skill",
            "INLINE_BODY_MARKER",
            &["bundle-skill-alias"],
        )],
    )]);
    reg.refresh(|c| c.register_tool(Arc::new(StubTool::noop("dynamic_marker"))))
        .unwrap();
    let b = bind(&reg);
    let pol = b.agent_resource_policy("none-agent").unwrap();
    assert_eq!(pol.selected_bundle_skill_ids(), &[stable.to_string()]);

    let c = b.compile_agent_resources(&pol).unwrap();
    assert!(domain_tool_names(&c).is_empty() && has_mail_only_read(&c));
    for absent in ["skill", "dynamic_marker", "bash"] {
        assert!(c.resolve_tool(absent).is_none(), "`{absent}` leaked");
    }
    let section = c.skills_prompt_section().expect("inlined skill section");
    assert!(
        section.contains("bundle-skill")
            && (section.contains("bundle-skill-alias") || section.contains(stable))
    );
    assert_eq!(section.matches("INLINE_BODY_MARKER").count(), 1);
    assert_eq!(
        c.skills()
            .iter()
            .map(|s| &s.path)
            .collect::<BTreeSet<_>>()
            .len(),
        1
    );
    assert!(
        c.skills().iter().any(|s| s.name == "bundle-skill")
            && c.skills().iter().any(|s| s.name == stable)
    );
}

#[test]
fn skill_facade_aliasing_and_missing_facade_rejection() {
    let stable = "bundle:hya/facade-alias/skill/bundle-skill";
    let reg = reg_for(&[one(
        "hya/facade-alias",
        "facade-alias",
        view_with(
            &["harness:tool/skill", stable],
            &[],
            &[("load_skill", "harness:tool/skill")],
            None,
        ),
        vec![skill_res(
            "hya/facade-alias",
            "bundle-skill",
            "SHOULD_NOT_INLINE",
            &[],
        )],
    )]);
    let c = full(&bind(&reg), "facade-alias");
    assert!(c.resolve_tool("load_skill").is_some() && c.resolve_tool("skill").is_none());
    let section = c.skills_prompt_section().expect("on-demand index");
    assert!(!section.contains("SHOULD_NOT_INLINE") && section.contains("bundle-skill"));

    let wd = TempSkillDir::new(
        "no-facade",
        "workdir-only",
        &skill_md("workdir-only", "MUST_NOT_INLINE"),
    );
    let nf_reg = reg_for(&[one(
        "hya/no-facade",
        "no-facade",
        view(&[], &["harness:tool/skill"]),
        Vec::new(),
    )]);
    match compile_on(
        &nf_reg.bind_turn(wd.path()).unwrap(),
        "no-facade",
        Some(AgentToolPlane::Full),
    ) {
        Err(BundleError::InvalidManifest { detail, .. }) => {
            assert!(detail.contains("skill") || detail.contains("facade"))
        }
        Err(BundleError::UnknownResourceReference { .. }) => {}
        Ok(_) => panic!("selected harness skill without facade must fail"),
        Err(other) => panic!("expected typed facade rejection, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 8. Bundle sidecar collisions and disjoint bundle policies
// ---------------------------------------------------------------------------

#[test]
fn bundle_sidecar_collision_requires_provider_safe_names_for_both_targets() {
    let bid = "hya/sidecar-map";
    let mut bundle = one(bid, "sidecar-agent", ResourceView::default(), Vec::new());
    bundle.tools.push(res(
        bid,
        "tool",
        "echo",
        "tools/echo.js",
        "export default {}".into(),
        &[],
    ));
    let tools = hya_tool_loader::builtins();
    tools
        .register_with_permission(Arc::new(StubTool::noop("echo")), ToolPermission::Tool)
        .unwrap();
    let b = bind(&reg_with(tools, catalog_from(&[bundle])));
    let mut pol = b
        .agent_resource_policy_on_plane("sidecar-agent", AgentToolPlane::Full)
        .unwrap();
    let bundle_echo = format!("bundle:{bid}/tool/echo");
    let sidecar = ResolvedTool {
        tool: Arc::new(StubTool::noop(&bundle_echo)),
        permission: ToolPermission::Tool,
    };

    match b.compile_agent_resources_with_sidecar_tools(&pol, std::slice::from_ref(&sidecar)) {
        Err(BundleError::InvalidManifest { detail, .. }) => {
            assert!(
                detail.contains("harness:tool/echo")
                    && detail.contains("no provider-safe schema name")
            );
        }
        Ok(_) => panic!("selected harness Tool must not vanish"),
        Err(other) => panic!("unexpected {other:?}"),
    }

    Arc::make_mut(&mut pol.resource_view)
        .aliases
        .insert("harness_echo".into(), "harness:tool/echo".into());
    let c = b
        .compile_agent_resources_with_sidecar_tools(&pol, &[sidecar])
        .unwrap();
    let names = schema_names(&c);
    assert!(names.contains("echo") && names.contains("harness_echo"));
    assert!(!names.contains(&bundle_echo) && !names.contains("harness:tool/echo"));
    assert_eq!(c.resolve_tool("echo").unwrap().tool.name(), bundle_echo);
    assert_eq!(
        c.resolve_tool(&bundle_echo).unwrap().tool.name(),
        bundle_echo
    );
    assert_eq!(c.resolve_tool("harness_echo").unwrap().tool.name(), "echo");
    assert_eq!(
        c.resolve_tool("harness:tool/echo").unwrap().tool.name(),
        "echo"
    );
}

#[test]
fn captured_agent_resource_policy_retains_disjoint_bundle_tool_and_hook_ids() {
    let mk = |bid: &str, aid: &str, tool: &str, hook: &str| {
        let tid = format!("bundle:{bid}/tool/{tool}");
        let hid = format!("bundle:{bid}/hook/{hook}");
        let mut ag = agent(aid, view(&[&tid], &[]));
        ag.hook_refs = vec![hid.clone()];
        let mut b = bundle_with_agent(bid, ag, Vec::new());
        b.tools = vec![res(
            bid,
            "tool",
            tool,
            &format!("extensions/{tool}.js"),
            "export default {}".into(),
            &[],
        )];
        b.hooks = vec![res(
            bid,
            "hook",
            hook,
            &format!("extensions/{hook}.js"),
            "export default {}".into(),
            &[],
        )];
        (b, tid, hid)
    };
    let (ab, at, ah) = mk("hya/disjoint-alpha", "alpha-agent", "alpha", "event");
    let (bb, bt, bh) = mk(
        "hya/disjoint-beta",
        "beta-agent",
        "beta",
        "tool.execute.before",
    );

    let b = bind(&reg_for(&[ab, bb]));
    let ap = b.agent_resource_policy("alpha-agent").unwrap();
    let bp = b.agent_resource_policy("beta-agent").unwrap();
    assert_eq!(ap.selected_bundle_tool_ids(), std::slice::from_ref(&at));
    assert_eq!(bp.selected_bundle_tool_ids(), std::slice::from_ref(&bt));
    assert_eq!(ap.canonical_hook_ids(), std::slice::from_ref(&ah));
    assert_eq!(bp.canonical_hook_ids(), std::slice::from_ref(&bh));

    let sc = |name: &str| ResolvedTool {
        tool: Arc::new(StubTool::noop(name)),
        permission: ToolPermission::Tool,
    };
    let ac = b
        .compile_agent_resources_with_sidecar_tools(&ap, &[sc(&at)])
        .unwrap();
    let bc = b
        .compile_agent_resources_with_sidecar_tools(&bp, &[sc(&bt)])
        .unwrap();
    assert_eq!(ac.canonical_hook_ids(), &[ah]);
    assert_eq!(bc.canonical_hook_ids(), &[bh]);

    let (an, bn) = (domain_tool_names(&ac), domain_tool_names(&bc));
    assert!(an.contains("alpha") && an.contains(at.as_str()));
    assert!(bn.contains("beta") && bn.contains(bt.as_str()));
    assert!(an.is_disjoint(&bn));
}

// ---------------------------------------------------------------------------
// 9. Handle scheme claims and dispatch
// ---------------------------------------------------------------------------

#[test]
fn plugin_scheme_claims_dispatch_within_view_and_reject_writes_or_missing_owner() {
    let reg = reg_for(&[one(
        "hya/scheme",
        "scheme-agent",
        ResourceView::default(),
        Vec::new(),
    )]);
    let owner = ReferenceRecordingTool::new("scheme__query");
    reg.refresh(|c| {
        c.upsert_sources(vec![
            RuntimeSource::new(
                RuntimeSourceId::plugin("scheme-src"),
                [0; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "scheme__query",
                    "scheme__query",
                    Vec::new(),
                    owner.clone(),
                    ToolPermission::ReadOnly,
                )],
            )
            .with_schemas(vec![SourceSchema {
                scheme: "db".into(),
                canonical_tool: "scheme__query".into(),
                writable: false,
            }]),
        ])
    })
    .unwrap();

    let wd = Path::new("/tmp/hya-scheme-dispatch");
    let b = reg.bind_turn(wd).unwrap();
    assert_eq!(
        b.schemes().get("db").map(SchemeBinding::canonical_tool),
        Some("scheme__query")
    );
    assert_eq!(
        b.scheme_chain("db"),
        vec![("plugin:scheme-src".into(), "scheme__query".into())]
    );
    assert_eq!(reg.effective_schemes().schemes.len(), 1);
    let ctx = dispatch_ctx(wd);

    let f = full(&b, "scheme-agent");
    let out = futures::executor::block_on(
        f.resolve_tool("read")
            .unwrap()
            .tool
            .execute(&ctx, json!({"path": "db://x/y?head=2"})),
    )
    .unwrap();
    assert_eq!(
        out.get("output").and_then(Value::as_str),
        Some("owner body")
    );
    assert_eq!(owner.references(), vec!["db://x/y?head=2".to_string()]);

    let w_err = futures::executor::block_on(
        f.resolve_tool("write")
            .unwrap()
            .tool
            .execute(&ctx, json!({"path": "db://x/y", "content": "row"})),
    )
    .unwrap_err();
    assert!(w_err.to_string().contains("db:// is read-only"), "{w_err}");
    assert_eq!(owner.references().len(), 1);

    let r = compile_on(&b, "scheme-agent", Some(AgentToolPlane::InternalPublic)).unwrap();
    assert!(r.resolve_tool("scheme__query").is_none());
    let m_err = futures::executor::block_on(
        r.resolve_tool("read")
            .unwrap()
            .tool
            .execute(&ctx, json!({"path": "db://x/y"})),
    )
    .unwrap_err();
    assert!(
        m_err.to_string().contains("unknown handle scheme"),
        "{m_err}"
    );
    assert_eq!(owner.references().len(), 1);
}

#[test]
fn bundle_agent_view_with_sidecar_owner_reads_registered_scheme() {
    let mut bundle = one(
        "hya/vecdb",
        "vecdb-lead",
        view(&["query", "harness:tool/read"], &[]),
        Vec::new(),
    );
    bundle.tools.push(res(
        "hya/vecdb",
        "tool",
        "query",
        "extensions/runtime.js",
        "export default {}".into(),
        &[],
    ));
    let reg = reg_for(&[bundle]);
    reg.refresh(|c| {
        c.replace_sources_of_kind(
            RuntimeSourceKind::Bundle,
            vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("hya/vecdb"),
                    [7; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_schemas(vec![SourceSchema {
                    scheme: "db".into(),
                    canonical_tool: "bundle:hya/vecdb/tool/query".into(),
                    writable: false,
                }]),
            ],
        )
    })
    .unwrap();

    let wd = Path::new("/tmp/hya-bundle-scheme-dispatch");
    let b = reg.bind_turn(wd).unwrap();
    let owner = ReferenceRecordingTool::new("bundle:hya/vecdb/tool/query");
    let c = b
        .compile_agent_resources_with_sidecar_tools(
            &b.agent_resource_policy("vecdb-lead").unwrap(),
            &[ResolvedTool {
                tool: owner.clone(),
                permission: ToolPermission::Tool,
            }],
        )
        .unwrap();
    let out = futures::executor::block_on(
        c.resolve_tool("read")
            .unwrap()
            .tool
            .execute(&dispatch_ctx(wd), json!({"path": "db://rows/42"})),
    )
    .unwrap();
    assert_eq!(
        out.get("output").and_then(Value::as_str),
        Some("owner body")
    );
    assert_eq!(owner.references(), vec!["db://rows/42".to_string()]);
}

// ---------------------------------------------------------------------------
// 10. Coordination tools injection (harness-owned, injected at startup)
// ---------------------------------------------------------------------------

#[test]
fn coordination_injection_matrix_for_views_spawn_rights_and_tool_families() {
    let narrow = schema_names(
        &compile_bundle_agent(
            hya_tool_loader::builtins(),
            agent("narrow-scout", view(&["harness:tool/grep"], &[])),
        )
        .unwrap(),
    );
    for ok in ["grep", "report", "wait", "send", "list_channel", "read"] {
        assert!(narrow.contains(ok), "missing `{ok}`");
    }
    for absent in ["task", "archive", "bash", "glob", "edit", "write"] {
        assert!(!narrow.contains(absent), "`{absent}` leaked");
    }

    let lead = schema_names(
        &compile_bundle_agent(
            hya_tool_loader::builtins(),
            PreparedAgent {
                can_spawn: vec![AgentName::new("hya-scout")],
                ..agent("narrow-lead", view(&["harness:tool/grep"], &[]))
            },
        )
        .unwrap(),
    );
    assert!(lead.contains("task") && lead.contains("archive"));

    let leaf = schema_names(
        &compile_bundle_agent(
            hya_tool_loader::builtins(),
            agent("leaf-worker", ResourceView::default()),
        )
        .unwrap(),
    );
    assert!(
        leaf.contains("bash")
            && leaf.contains("wait")
            && !leaf.contains("task")
            && !leaf.contains("archive")
    );

    let quiet = schema_names(
        &compile_bundle_agent(
            hya_tool_loader::from_tool_families(&WITHOUT_CHANNEL_TOOLS),
            agent("quiet-scout", view(&["harness:tool/grep"], &[])),
        )
        .unwrap(),
    );
    assert_eq!(quiet, BTreeSet::from(["grep".into(), "wait".into()]));

    let explicit = compile_bundle_agent(
        hya_tool_loader::builtins(),
        agent(
            "explicit-scout",
            view(
                &[
                    "harness:tool/grep",
                    "harness:tool/report",
                    "harness:tool/wait",
                    "list_channel",
                ],
                &[],
            ),
        ),
    )
    .unwrap();
    let schemas = explicit.tool_schemas();
    for name in ["report", "wait", "list_channel"] {
        assert_eq!(
            schemas.iter().filter(|s| s.name.as_str() == name).count(),
            1,
            "`{name}` duplicated"
        );
    }
}

#[test]
fn coordination_read_injection_and_deny_rules() {
    let read_desc = |c: &CompiledResourceView| {
        c.tool_schemas()
            .into_iter()
            .find(|s| s.name.as_str() == "read")
            .unwrap()
            .description
    };
    let mail_only = compile_bundle_agent(
        hya_tool_loader::builtins(),
        agent("mail-reader", view(&["harness:tool/grep"], &[])),
    )
    .unwrap();
    assert!(read_desc(&mail_only).contains("channel://"));
    let err = futures::executor::block_on(mail_only.resolve_tool("read").unwrap().tool.execute(
        &dispatch_ctx(Path::new("/tmp/hya-coord")),
        json!({"path": "Cargo.toml"}),
    ))
    .unwrap_err();
    assert!(
        matches!(&err, ToolError::Input(m) if m.contains("channel://")),
        "{err:?}"
    );

    let full_read = compile_bundle_agent(
        hya_tool_loader::builtins(),
        agent("file-reader", view(&["harness:tool/read"], &[])),
    )
    .unwrap();
    assert!(read_desc(&full_read).starts_with("Read a file"));

    let denied_read = compile_bundle_agent(
        hya_tool_loader::builtins(),
        agent("no-files", view(&[], &["harness:tool/read"])),
    )
    .unwrap();
    assert!(read_desc(&denied_read).contains("channel://"));

    let reduced = schema_names(
        &compile_bundle_agent(
            hya_tool_loader::builtins(),
            PreparedAgent {
                can_spawn: vec![AgentName::new("hya-scout")],
                ..agent(
                    "no-task-lead",
                    view(&["harness:tool/grep"], &["harness:tool/task", "send"]),
                )
            },
        )
        .unwrap(),
    );
    assert!(
        !reduced.contains("task")
            && !reduced.contains("send")
            && reduced.contains("archive")
            && reduced.contains("report")
    );

    let report_err = compile_bundle_agent(
        hya_tool_loader::builtins(),
        agent("stuck-scout", view(&[], &["harness:tool/report"])),
    )
    .err()
    .unwrap();
    assert!(
        matches!(&report_err, BundleError::InvalidManifest { detail, .. } if detail.contains("report"))
    );
}

#[test]
fn builtin_agent_views_enforce_read_only_and_coordination_sets() {
    let plan_bundle = hya_bundle::first_party_bundle("hya/plan").unwrap();
    let multi_agent_bundle = hya_bundle::first_party_bundle("hya/multi-agent").unwrap();
    let b = bind(&reg_with(
        hya_tool_loader::builtins(),
        verified_catalog(&[plan_bundle, multi_agent_bundle]),
    ));
    let names = |id: &str| schema_names(&basic(&b, id));
    let writes = ["write", "edit", "apply_patch", "bash"];

    let plan = names("hya-plan");
    assert!(
        writes.iter().all(|d| !plan.contains(*d))
            && plan.contains("grep")
            && plan.contains("task")
            && plan.contains("plan_exit")
    );

    let scout = names("hya-scout");
    assert!(
        writes
            .into_iter()
            .chain(["task", "archive"])
            .all(|d| !scout.contains(d))
    );
    assert!(scout.contains("grep") && scout.contains("read"));

    let main = names("hya-main");
    for k in writes.into_iter().chain([
        "task",
        "archive",
        "wait",
        "report",
        "send",
        "list_channel",
        "read",
    ]) {
        assert!(main.contains(k), "hya-main lacks `{k}`");
    }
}

struct PrivateProcessHook;

impl crate::NativeHook for PrivateProcessHook {
    fn id(&self) -> &str {
        "private-process"
    }

    fn hook_points(&self) -> &[crate::HookPoint] {
        &[crate::HookPoint::ChatParams]
    }
}
