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

#[path = "../../../tests/runtime_registry_moved/support.rs"]
mod support;
use support::*;

#[test]
fn skill_discovery_runs_outside_publication_lock() {
    let registry = RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(TestCatalog::from_prepared(&[]).unwrap()),
    );
    let directory = Path::new("/skill-discovery-lock-test");
    registry
        .bind_scoped_with_skills(
            &CatalogScope::Directory(directory.to_path_buf()),
            directory,
            || {
                assert!(registry.publication.try_lock().is_ok());
                Vec::new()
            },
        )
        .unwrap();
}

#[test]
fn full_view_exposes_only_catalog_confirmed_plugin_bundle_skills() {
    let prepare = |kind: &str, id: &str, name: &str, agent: &str| {
        prepare_package(BundleSource::new(
                id,
                vec![
                    SourceFile::new("bundle.yaml", format!(
                        "kind: {kind}\nidentity: {{ id: acme/{id}, version: 1.0.0, publisher: acme }}\nresources:\n  skills:\n    - id: {name}\n      path: skills/guide.md\n{agent}"
                    )),
                    SourceFile::new("skills/guide.md", skill_md(name, "SKILL_BODY")),
                ],
            )).unwrap()
    };
    let plugin = prepare("Bundle", "shared", "shared-skill", "");
    let agent = prepare(
        "Bundle",
        "private",
        "private-skill",
        "agents: [{ id: private-agent, role: main }]\n",
    );
    let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&plugin, &agent]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    // An unrecognized bundle source cannot opt into shared Skill visibility.
    let mut unknown = registry.active().sources[&RuntimeSourceId::bundle("acme/shared")].clone();
    unknown.id = RuntimeSourceId::bundle("acme/unknown");
    unknown.skills[0].stable_id = "bundle:acme/unknown/skill/unknown-skill".into();
    unknown.skills[0].local_id = "unknown-skill".into();
    unknown.skills[0].entry.name = "unknown-skill".into();
    registry
        .refresh(|candidate| candidate.upsert_sources(vec![unknown]))
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-plugin-skill-view"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("hya-main", AgentToolPlane::Full)
        .unwrap();
    let resources = binding.compile_agent_resources(&policy).unwrap();
    assert!(
        resources
            .skills()
            .iter()
            .any(|skill| skill.name == "shared-skill")
    );
    assert!(
        resources
            .skills()
            .iter()
            .any(|skill| skill.name == "bundle:acme/shared/skill/shared-skill")
    );
    assert!(
        !resources
            .skills()
            .iter()
            .any(|skill| skill.name == "private-skill")
    );
    assert!(
        !resources
            .skills()
            .iter()
            .any(|skill| skill.name == "unknown-skill")
    );
    let private_policy = binding.agent_resource_policy("private-agent").unwrap();
    let private_resources = binding.compile_agent_resources(&private_policy).unwrap();
    assert!(
        private_resources
            .skills()
            .iter()
            .any(|skill| skill.name == "private-skill")
    );
    assert!(
        !private_resources
            .skills()
            .iter()
            .any(|skill| skill.name == "shared-skill")
    );
}

#[test]
fn bundled_mcp_stays_in_mcp_partition() {
    let prepared = prepare_package(BundleSource::new(
        "bundled-mcp",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nidentity: { id: acme/bundled-mcp, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::bundle("acme/bundled-mcp"),
                [1; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "mcp/echo/ping",
                    "bundled-mcp__mcp__echo__ping",
                    Vec::new(),
                    Arc::new(NoopTool::new("bundled-mcp__mcp__echo__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-bundled-mcp-kind"))
        .unwrap();
    let policy = binding.agent_resource_policy("hya-main").unwrap();
    let candidates = binding.collect_resource_candidates(&policy).unwrap();
    assert!(
        candidates
            .mcp
            .contains_key("harness:mcp/bundled-mcp__mcp__echo__ping")
    );
    assert!(
        !candidates
            .tool
            .contains_key("harness:tool/bundled-mcp__mcp__echo__ping")
    );
}

#[test]
fn bundle_process_tool_is_owner_scoped_and_needs_no_bun_sidecar() {
    let prepared = prepare_package(BundleSource::new(
        "private-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                r#"kind: Bundle
identity: { id: acme/private-process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [fixture] }
resources:
  tools: [{ id: echo, path: echo.json }]
  hooks: [{ id: tool.execute.before, path: echo.json }]
agents:
  - hook_refs: [tool.execute.before]
    id: private-process-agent
    role: main
    resource_view: { allow: [echo] }
"#,
            ),
            SourceFile::new("echo.json", "{}"),
        ],
    ))
    .unwrap();
    let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/private-process"),
                    [2; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "echo",
                        "private-process__echo",
                        Vec::new(),
                        Arc::new(NoopTool::new("private-process__echo")),
                        ToolPermission::Tool,
                    )],
                )
                .with_hooks(Arc::new(crate::hooks::HookChain::new(Vec::new()))),
            ])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-private-process"))
        .unwrap();
    let policy = binding
        .agent_resource_policy("private-process-agent")
        .unwrap();
    let compiled = binding
        .compile_agent_resources(&policy)
        .expect("owner process resource must compile");
    assert!(compiled.resolve_tool("echo").is_some());
    assert_eq!(
        binding
            .bundle_hooks_for_agent("private-process-agent")
            .len(),
        1
    );
    assert!(binding.bundle_hooks_for_agent("hya-main").is_empty());
    assert!(
        !binding
            .has_selected_bundle_sidecar_capability("private-process-agent")
            .unwrap()
    );
    let root = binding
        .compile_agent_resources(&binding.agent_resource_policy("hya-main").unwrap())
        .unwrap();
    assert!(root.resolve_tool("private-process__echo").is_none());
}

#[test]
fn bundle_mcp_server_selection_expands_owner_tools_only() {
    let prepared = prepare_package(BundleSource::new(
        "private-mcp",
        vec![
            SourceFile::new(
                "bundle.yaml",
                r#"kind: Bundle
identity: { id: acme/private-mcp, version: 1.0.0, publisher: acme }
resources:
  mcp: [{ id: echo, path: mcp.json }]
agents:
  - id: private-mcp-agent
    role: main
    resource_view: { allow: [echo] }
"#,
            ),
            SourceFile::new("mcp.json", r#"{"command":["fixture"]}"#),
        ],
    ))
    .unwrap();
    let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::bundle("acme/private-mcp"),
                [3; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "mcp/echo/ping",
                    "private-mcp__mcp__echo__ping",
                    Vec::new(),
                    Arc::new(NoopTool::new("private-mcp__mcp__echo__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-private-mcp"))
        .unwrap();
    let policy = binding.agent_resource_policy("private-mcp-agent").unwrap();
    let compiled = binding
        .compile_agent_resources(&policy)
        .expect("selected MCP server must provide its tools");
    assert_eq!(
        compiled.resolve_tool("echo__ping").unwrap().permission,
        ToolPermission::Mcp
    );
    assert_eq!(
        domain_schema_names(&compiled),
        BTreeSet::from(["echo__ping".to_string()])
    );
    let root = binding
        .compile_agent_resources(&binding.agent_resource_policy("hya-main").unwrap())
        .unwrap();
    assert!(root.resolve_tool("private-mcp__mcp__echo__ping").is_none());
}

struct FingerprintTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl FingerprintTool {
    fn new(name: impl Into<String>, marker: &str) -> Self {
        Self {
            name: name.into(),
            description: format!("fingerprint schema {marker}"),
            input_schema: json!({
                "type": "object",
                "properties": {"marker": {"const": marker}},
            }),
        }
    }
}

#[async_trait]
impl Tool for FingerprintTool {
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
        Ok(json!({"ok": true}))
    }
}

#[test]
fn pure_registry_serves_builtin_skills_only() {
    let dir = std::env::temp_dir().join(format!("hya-pure-skills-{}", std::process::id()));
    let skill_dir = dir.join(".hya/skills/external-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: external-skill\ndescription: must not appear in pure mode\n---\nbody\n",
    )
    .unwrap();

    let catalog = Arc::new(TestCatalog::from_prepared(&[]).unwrap());
    let plain = test_runtime_registry(ToolRegistry::builtins(), catalog.clone());
    let pure = test_runtime_registry(ToolRegistry::builtins(), catalog).with_pure_skills(true);

    let plain_binding = plain.bind_turn(&dir).unwrap();
    let plain_names: Vec<String> = plain_binding
        .snapshot
        .skills
        .get(&dir)
        .map(|skills| skills.iter().map(|skill| skill.name.clone()).collect())
        .unwrap_or_default();
    assert!(
        plain_names.iter().any(|name| name == "external-skill"),
        "default registry discovers the project skill: {plain_names:?}"
    );

    let pure_binding = pure.bind_turn(&dir).unwrap();
    let pure_names: Vec<String> = pure_binding
        .snapshot
        .skills
        .get(&dir)
        .map(|skills| skills.iter().map(|skill| skill.name.clone()).collect())
        .unwrap_or_default();
    assert!(
        !pure_names.iter().any(|name| name == "external-skill"),
        "pure registry never reads external skill dirs: {pure_names:?}"
    );
    assert!(
        !pure_names.is_empty(),
        "pure registry still serves the embedded builtin catalog"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_turn_binding_pins_tools_across_later_publication() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/pin",
            agent("pin-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let workdir = PathBuf::from("/tmp/hya-resource-view-pin");
    let binding = registry.bind_turn(&workdir).unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("pin-agent", AgentToolPlane::Full)
        .unwrap();
    let before = binding.compile_agent_resources(&policy).unwrap();
    assert!(before.resolve_tool("read").is_some());
    assert!(before.resolve_tool("dynamic_marker").is_none());

    registry
        .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
        .unwrap();

    let after = binding.compile_agent_resources(&policy).unwrap();
    assert!(
        after.resolve_tool("dynamic_marker").is_none(),
        "retained TurnBinding must not observe later registry publication"
    );
    assert_eq!(
        before.tools.keys().collect::<BTreeSet<_>>(),
        after.tools.keys().collect::<BTreeSet<_>>(),
        "pinned binding must compile an identical public tool set"
    );
}

#[test]
fn alias_masking_publishes_and_compiles_bare_name_to_winner() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/mask",
            agent("mask-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::plugin("mask-src"),
                [14; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "fast",
                    "mask__fast",
                    vec!["bash".to_string()],
                    Arc::new(NoopTool::new("mask__fast")),
                    ToolPermission::Tool,
                )],
            )])
        })
        .expect("alias masking a built-in must publish");

    let workdir = PathBuf::from("/tmp/hya-alias-mask-builtin");
    let binding = registry.bind_turn(&workdir).unwrap();
    assert_eq!(
        binding.masks().get("bash").map(String::as_str),
        Some("mask__fast"),
        "the contributed alias must win the bare name from the built-in"
    );

    let policy = binding
        .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let names = compiled.tools.keys().cloned().collect::<BTreeSet<String>>();
    assert!(
        names.contains("bash") && !names.contains("harness:tool/bash"),
        "the masked built-in must be excluded while the bare name stays: {names:?}"
    );
    assert!(
        names.contains("harness:tool/mask__fast"),
        "the winning contributed tool must stay in the view: {names:?}"
    );
    let bash_schemas = compiled
        .tool_schemas()
        .into_iter()
        .filter(|schema| schema.name.as_str() == "bash")
        .collect::<Vec<_>>();
    assert_eq!(
        bash_schemas.len(),
        1,
        "the bare name must appear exactly once"
    );
    assert_eq!(
        bash_schemas[0].input_schema,
        json!({ "type": "object" }),
        "the advertised bash schema must be the contributed winner, not the built-in"
    );
    assert!(compiled.resolve_tool("bash").is_some());

    let mut removed = BTreeSet::new();
    removed.insert(RuntimeSourceId::plugin("mask-src"));
    registry
        .refresh(|candidate| {
            candidate.remove_sources(&removed);
            Ok(())
        })
        .expect("source removal must publish");

    let binding = registry.bind_turn(&workdir).unwrap();
    assert!(
        binding.masks().is_empty(),
        "removing the winner must dissolve the mask"
    );
    let policy = binding
        .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let names = compiled.tools.keys().cloned().collect::<BTreeSet<String>>();
    assert!(
        names.contains("harness:tool/bash"),
        "the built-in must be resolvable again after the masking source is removed: {names:?}"
    );
}

#[test]
fn cross_source_alias_mask_goes_to_greater_source_id() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/mask",
            agent("mask-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let contender = |configured_id: &str, digest: [u8; 32], canonical: &str| {
        RuntimeSource::new(
            RuntimeSourceId::plugin(configured_id),
            digest,
            Arc::new(()),
            vec![RuntimeSourceExport::tool(
                "lookup",
                canonical,
                vec!["lookup".to_string()],
                Arc::new(NoopTool::new(canonical)),
                ToolPermission::Tool,
            )],
        )
    };
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                contender("alpha", [16; 32], "alpha__lookup"),
                contender("beta", [17; 32], "beta__lookup"),
            ])
        })
        .expect("cross-source alias collisions must mask instead of rejecting");

    let workdir = PathBuf::from("/tmp/hya-alias-mask-cross-source");
    let binding = registry.bind_turn(&workdir).unwrap();
    assert_eq!(
        binding.masks().get("lookup").map(String::as_str),
        Some("beta__lookup"),
        "the lexicographically greater source id must win the bare name"
    );
    let policy = binding
        .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let names = compiled.tools.keys().cloned().collect::<BTreeSet<String>>();
    assert!(
        names.contains("lookup"),
        "the contested bare name must resolve in the view: {names:?}"
    );
    assert!(
        names.contains("harness:tool/beta__lookup"),
        "the winner keeps its qualified identity: {names:?}"
    );
    assert!(
        names.contains("alpha__lookup"),
        "the loser must stay resolvable via its qualified canonical: {names:?}"
    );
    assert!(
        compiled.resolve_tool("alpha__lookup").is_some(),
        "the loser's qualified canonical is the escape hatch"
    );
    let lookup_schemas = compiled
        .tool_schemas()
        .into_iter()
        .filter(|schema| schema.name.as_str() == "lookup")
        .collect::<Vec<_>>();
    assert_eq!(
        lookup_schemas.len(),
        1,
        "the bare name must appear exactly once"
    );
    assert_eq!(
        lookup_schemas[0].input_schema,
        json!({ "type": "object" }),
        "the advertised lookup schema must be the winner's tool"
    );
    assert!(
        !compiled
            .tool_schemas()
            .iter()
            .any(|schema| schema.name.as_str() == "beta__lookup"),
        "a masked-in winner must not also advertise its qualified spelling"
    );
}

#[test]
fn runtime_source_dispatch_identity_tracks_authoritative_source_semantics() {
    let source_identity =
        |configured_id: &str, declaration_digest: [u8; 32], resource_value: Value| {
            let catalog = Arc::new(
                TestCatalog::from_prepared(&[bundle_with_agent(
                    "hya/source-identity",
                    agent("source-identity", ResourceView::default()),
                    Vec::new(),
                )])
                .unwrap(),
            );
            let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
            let mut resources = BTreeMap::new();
            resources.insert("probe".to_string(), resource_value);
            registry
                .refresh(|candidate| {
                    candidate.upsert_sources(vec![
                        RuntimeSource::new(
                            RuntimeSourceId::plugin(configured_id),
                            declaration_digest,
                            Arc::new(()),
                            vec![RuntimeSourceExport::tool(
                                "probe",
                                "plugin__fixture__probe",
                                Vec::new(),
                                Arc::new(NoopTool::new("plugin__fixture__probe")),
                                ToolPermission::Tool,
                            )],
                        )
                        .with_resources(resources),
                    ])
                })
                .unwrap();
            let identity = registry
                .active()
                .tools
                .dispatch_identity_v1("plugin__fixture__probe");
            let Some(identity) = identity else {
                panic!("plugin source must expose dispatch identity");
            };
            assert_ne!(identity, [0_u8; 32]);
            identity
        };

    let baseline = source_identity("fixture", [1; 32], json!({"mode": "one"}));
    let same = source_identity("fixture", [1; 32], json!({"mode": "one"}));
    assert_eq!(same, baseline);
    assert_ne!(
        source_identity("fixture-other", [1; 32], json!({"mode": "one"})),
        baseline
    );
    assert_ne!(
        source_identity("fixture", [2; 32], json!({"mode": "one"})),
        baseline
    );
    assert_ne!(
        source_identity("fixture", [1; 32], json!({"mode": "two"})),
        baseline
    );
}

#[test]
fn runtime_semantic_fingerprint_is_generation_independent_and_base_section_sensitive() {
    let Some(next_generation) = ConfigGeneration::INITIAL.checked_next() else {
        panic!("test generation must have a successor");
    };

    let fixture = |catalog_marker: &str,
                   schema_marker: &str,
                   reverse_registration: bool,
                   first_dispatch_identity: [u8; 32],
                   permission_mode: Mode,
                   generation: ConfigGeneration| {
        let manifest = format!(
            r#"kind: Bundle
identity:
  id: hya/runtime-fingerprint
  version: 1.0.0
  publisher: hya-tests
agents:
  - id: fingerprint
    description: "manifest {catalog_marker}"
    role: main
"#
        );
        let prepared = prepare_package(BundleSource::new(
            "runtime-fingerprint",
            vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
        ));
        let Ok(prepared) = prepared else {
            panic!("runtime fingerprint fixture preparation failed: {prepared:?}");
        };
        let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
        let Ok(catalog) = catalog else {
            panic!("runtime fingerprint verified catalog construction failed: {catalog:?}");
        };

        let registry = ToolRegistry::builtins();
        let mut custom_tools = vec![
            (
                Arc::new(FingerprintTool::new("custom_one", schema_marker)) as Arc<dyn Tool>,
                first_dispatch_identity,
            ),
            (
                Arc::new(FingerprintTool::new("custom_two", "stable")) as Arc<dyn Tool>,
                [2; 32],
            ),
        ];
        if reverse_registration {
            custom_tools.reverse();
        }
        for (tool, dispatch_identity) in custom_tools {
            if let Err(error) = registry.register_with_permission_and_aliases_and_dispatch_identity(
                tool,
                ToolPermission::Tool,
                &[],
                dispatch_identity,
            ) {
                panic!("runtime fingerprint tool fixture registration failed: {error}");
            }
        }

        let rules =
            PermissionRules::new(vec![Rule::new(Action::Tool, "custom_one", permission_mode)]);
        let policy = InvocationPolicy::compile(
            PermissionModel::Default,
            vec![InvocationRule::new(
                PermissionTarget::Tool,
                "^custom_one$",
                Mode::Allow,
            )],
        );
        let Ok(policy) = policy else {
            panic!("runtime fingerprint invocation policy fixture must compile: {policy:?}");
        };
        let (permission, _asks) = PermissionPlane::new_with_policy(rules, policy);
        let tools = registry.snapshot();
        let snapshot = RuntimeSnapshot {
            generation,
            catalog: Arc::new(catalog),
            basic_tools: tools.clone(),
            tools,
            skills: BTreeMap::new(),
            sources: BTreeMap::new(),
            masks: Arc::new(BTreeMap::new()),
            schemes: Arc::new(BTreeMap::new()),
        };
        (
            TurnBinding {
                snapshot: Arc::new(snapshot),
                agent_model_preferences: Arc::new(BTreeMap::new()),
                agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                session_agent_models: Arc::new(BTreeMap::new()),
                place: BindingPlace::new(
                    PathBuf::from("/tmp/runtime-fingerprint"),
                    CatalogScope::Global,
                ),
            },
            permission,
        )
    };

    let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
        let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
            panic!("runtime semantic fingerprint should be available for this fixture");
        };
        assert_ne!(fingerprint, [0_u8; 32]);
        fingerprint
    };

    let (baseline_binding, baseline_permission) = fixture(
        "one",
        "one",
        false,
        [1; 32],
        Mode::Allow,
        ConfigGeneration::INITIAL,
    );
    let baseline = fingerprint(&baseline_binding, &baseline_permission);

    let (equivalent_binding, equivalent_permission) =
        fixture("one", "one", true, [1; 32], Mode::Allow, next_generation);
    assert_eq!(
        fingerprint(&equivalent_binding, &equivalent_permission),
        baseline,
        "fresh objects, registration order, and ConfigGeneration must not affect semantics"
    );

    let (catalog_binding, catalog_permission) = fixture(
        "two",
        "one",
        false,
        [1; 32],
        Mode::Allow,
        ConfigGeneration::INITIAL,
    );
    assert_ne!(
        fingerprint(&catalog_binding, &catalog_permission),
        baseline,
        "verified catalog semantics must affect the base fingerprint"
    );

    let (schema_binding, schema_permission) = fixture(
        "one",
        "two",
        false,
        [1; 32],
        Mode::Allow,
        ConfigGeneration::INITIAL,
    );
    assert_ne!(
        fingerprint(&schema_binding, &schema_permission),
        baseline,
        "tool schema semantics must affect the base fingerprint"
    );

    let (dispatch_binding, dispatch_permission) = fixture(
        "one",
        "one",
        false,
        [3; 32],
        Mode::Allow,
        ConfigGeneration::INITIAL,
    );
    assert_ne!(
        fingerprint(&dispatch_binding, &dispatch_permission),
        baseline,
        "explicit dispatch identity must affect the base fingerprint"
    );

    let (permission_binding, permission_variant) = fixture(
        "one",
        "one",
        false,
        [1; 32],
        Mode::Deny,
        ConfigGeneration::INITIAL,
    );
    assert_ne!(
        fingerprint(&permission_binding, &permission_variant),
        baseline,
        "permission-rule semantics must affect the base fingerprint"
    );
}

#[test]
fn runtime_semantic_fingerprint_tracks_selected_workdir_skill_semantics() {
    let skill_fixture = |name: &str,
                         description: &str,
                         content: &str,
                         allowed_tools: &[&str],
                         model: Option<&str>,
                         path: &str| {
        SkillCatalogEntry {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            allowed_tools: allowed_tools
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            model: model.map(str::to_string),
            path: PathBuf::from(path),
            dir: PathBuf::from(path)
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf),
            origin: hya_tool::SkillCatalogOrigin::Filesystem,
        }
    };
    let selected_skills = || {
        vec![
            skill_fixture(
                "alpha",
                "Alpha skill",
                "alpha body",
                &["custom_one"],
                Some("model-a"),
                "/tmp/runtime-fingerprint-skills/alpha/SKILL.md",
            ),
            skill_fixture(
                "beta",
                "Beta skill",
                "beta body",
                &[],
                None,
                "/tmp/runtime-fingerprint-skills/beta/SKILL.md",
            ),
        ]
    };
    let fixture = |selected: Vec<SkillCatalogEntry>, unrelated: Vec<SkillCatalogEntry>| {
        let manifest = r#"kind: Bundle
identity:
  id: hya/runtime-fingerprint-skills
  version: 1.0.0
  publisher: hya-tests
agents:
  - id: fingerprint-skills
    role: main
"#;
        let prepared = prepare_package(BundleSource::new(
            "runtime-fingerprint-skills",
            vec![SourceFile::new("bundle.yaml", manifest.as_bytes())],
        ));
        let Ok(prepared) = prepared else {
            panic!("skill fingerprint fixture preparation failed: {prepared:?}");
        };
        let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
        let Ok(catalog) = catalog else {
            panic!("skill fingerprint verified catalog construction failed: {catalog:?}");
        };
        let tools = ToolRegistry::builtins().snapshot();
        let workdir = PathBuf::from("/tmp/runtime-fingerprint-skills");
        let mut skills = BTreeMap::new();
        skills.insert(workdir.clone(), Arc::new(selected));
        if !unrelated.is_empty() {
            skills.insert(
                PathBuf::from("/tmp/runtime-fingerprint-unrelated"),
                Arc::new(unrelated),
            );
        }
        let snapshot = RuntimeSnapshot {
            generation: ConfigGeneration::INITIAL,
            catalog: Arc::new(catalog),
            basic_tools: tools.clone(),
            tools,
            skills,
            sources: BTreeMap::new(),
            masks: Arc::new(BTreeMap::new()),
            schemes: Arc::new(BTreeMap::new()),
        };
        let (permission, _asks) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::default(),
        );
        (
            TurnBinding {
                snapshot: Arc::new(snapshot),
                agent_model_preferences: Arc::new(BTreeMap::new()),
                agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                session_agent_models: Arc::new(BTreeMap::new()),
                place: BindingPlace::new(workdir, CatalogScope::Global),
            },
            permission,
        )
    };
    let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
        let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
            panic!("selected workdir skills must be fingerprintable");
        };
        assert_ne!(fingerprint, [0_u8; 32]);
        fingerprint
    };

    let (baseline_binding, baseline_permission) = fixture(selected_skills(), Vec::new());
    let baseline = fingerprint(&baseline_binding, &baseline_permission);
    let (equivalent_binding, equivalent_permission) = fixture(selected_skills(), Vec::new());
    assert_eq!(
        fingerprint(&equivalent_binding, &equivalent_permission),
        baseline,
        "fresh skill entries, catalogs, and permission planes must match"
    );

    let mut changed_content = selected_skills();
    changed_content[0].content = "changed body".to_string();
    let (content_binding, content_permission) = fixture(changed_content, Vec::new());
    assert_ne!(
        fingerprint(&content_binding, &content_permission),
        baseline,
        "skill content must affect the fingerprint"
    );

    let mut renamed = selected_skills();
    renamed[0].name = "alpha-renamed".to_string();
    let (rename_binding, rename_permission) = fixture(renamed, Vec::new());
    assert_ne!(
        fingerprint(&rename_binding, &rename_permission),
        baseline,
        "skill identity must affect the fingerprint"
    );

    let mut moved = selected_skills();
    moved[0].path = PathBuf::from("/tmp/runtime-fingerprint-skills/moved/SKILL.md");
    moved[0].dir = PathBuf::from("/tmp/runtime-fingerprint-skills/moved");
    let (path_binding, path_permission) = fixture(moved, Vec::new());
    assert_ne!(
        fingerprint(&path_binding, &path_permission),
        baseline,
        "skill path must affect the fingerprint"
    );

    let mut changed_description = selected_skills();
    changed_description[0].description = "changed description".to_string();
    let (description_binding, description_permission) = fixture(changed_description, Vec::new());
    assert_ne!(
        fingerprint(&description_binding, &description_permission),
        baseline,
        "skill semantic metadata must affect the fingerprint"
    );

    let mut changed_allowed_tools = selected_skills();
    changed_allowed_tools[0].allowed_tools = vec!["custom_two".to_string()];
    let (allowed_tools_binding, allowed_tools_permission) =
        fixture(changed_allowed_tools, Vec::new());
    assert_ne!(
        fingerprint(&allowed_tools_binding, &allowed_tools_permission),
        baseline,
        "skill semantic metadata must affect the fingerprint"
    );

    let mut changed_model = selected_skills();
    changed_model[0].model = Some("model-b".to_string());
    let (model_binding, model_permission) = fixture(changed_model, Vec::new());
    assert_ne!(
        fingerprint(&model_binding, &model_permission),
        baseline,
        "skill semantic metadata must affect the fingerprint"
    );

    let mut reversed = selected_skills();
    reversed.reverse();
    let (reversed_binding, reversed_permission) = fixture(reversed, Vec::new());
    assert_ne!(
        fingerprint(&reversed_binding, &reversed_permission),
        baseline,
        "selected skill order must preserve precedence semantics"
    );

    let unrelated = vec![skill_fixture(
        "unrelated",
        "unrelated skill",
        "unrelated body",
        &[],
        None,
        "/tmp/runtime-fingerprint-unrelated/unrelated/SKILL.md",
    )];
    let (unrelated_binding, unrelated_permission) = fixture(selected_skills(), unrelated);
    assert_eq!(
        fingerprint(&unrelated_binding, &unrelated_permission),
        baseline,
        "skills cached for another workdir must not affect this binding"
    );
}

#[test]
fn runtime_semantic_fingerprint_tracks_plugin_and_mcp_source_semantics() {
    let nested_value = |marker: &str, reverse: bool| {
        let mut nested = serde_json::Map::new();
        let entries = [
            ("marker", Value::String(marker.to_string())),
            ("enabled", Value::Bool(true)),
        ];
        if reverse {
            for (key, value) in entries.into_iter().rev() {
                nested.insert(key.to_string(), value);
            }
        } else {
            for (key, value) in entries {
                nested.insert(key.to_string(), value);
            }
        }
        let mut outer = serde_json::Map::new();
        if reverse {
            outer.insert("nested".to_string(), Value::Object(nested));
            outer.insert("version".to_string(), Value::from(1));
        } else {
            outer.insert("version".to_string(), Value::from(1));
            outer.insert("nested".to_string(), Value::Object(nested));
        }
        Value::Object(outer)
    };
    let fixture = |plugin_kind: RuntimeSourceKind,
                   plugin_id: &str,
                   plugin_digest: [u8; 32],
                   plugin_resource_marker: &str,
                   mcp_digest: [u8; 32],
                   mcp_resource_marker: &str,
                   include_mcp: bool,
                   reverse_sources: bool,
                   reverse_resources: bool| {
        let manifest = r#"kind: Bundle
identity:
  id: hya/runtime-fingerprint-sources
  version: 1.0.0
  publisher: hya-tests
agents:
  - id: fingerprint-sources
    role: main
"#;
        let prepared = prepare_package(BundleSource::new(
            "runtime-fingerprint-sources",
            vec![SourceFile::new("bundle.yaml", manifest.as_bytes())],
        ));
        let Ok(prepared) = prepared else {
            panic!("source fingerprint fixture preparation failed: {prepared:?}");
        };
        let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
        let Ok(catalog) = catalog else {
            panic!("source fingerprint verified catalog construction failed: {catalog:?}");
        };
        let registry = test_runtime_registry(ToolRegistry::builtins(), Arc::new(catalog));
        let mut plugin_resources = BTreeMap::new();
        plugin_resources.insert(
            "config".to_string(),
            nested_value(plugin_resource_marker, reverse_resources),
        );
        let probe_name = match plugin_kind {
            RuntimeSourceKind::Mcp => format!("mcp__{plugin_id}__probe"),
            _ => format!("plugin__{plugin_id}__probe"),
        };
        let plugin = RuntimeSource::new(
            RuntimeSourceId::new(plugin_kind, plugin_id),
            plugin_digest,
            Arc::new(()),
            vec![RuntimeSourceExport::tool(
                "probe",
                probe_name.clone(),
                vec!["plugin_probe".to_string()],
                Arc::new(NoopTool::new(&probe_name)),
                ToolPermission::Tool,
            )],
        )
        .with_resources(plugin_resources);
        let mut mcp_resources = BTreeMap::new();
        mcp_resources.insert(
            "config".to_string(),
            nested_value(mcp_resource_marker, reverse_resources),
        );
        let mcp = RuntimeSource::new(
            RuntimeSourceId::mcp("fixture-mcp"),
            mcp_digest,
            Arc::new(()),
            Vec::new(),
        )
        .with_resources(mcp_resources);
        let mut sources = vec![plugin];
        if include_mcp {
            sources.push(mcp);
        }
        if reverse_sources {
            sources.reverse();
        }
        let refreshed = registry.refresh(|candidate| candidate.upsert_sources(sources));
        let Ok(_) = refreshed else {
            panic!("source fingerprint refresh failed: {refreshed:?}");
        };
        let tools = registry.active();
        let (permission, _asks) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::default(),
        );
        (
            TurnBinding {
                agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                session_agent_models: Arc::new(BTreeMap::new()),
                snapshot: tools,
                agent_model_preferences: Arc::new(BTreeMap::new()),
                place: BindingPlace::new(
                    PathBuf::from("/tmp/runtime-fingerprint-sources"),
                    CatalogScope::Global,
                ),
            },
            permission,
        )
    };
    let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
        let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
            panic!("runtime source semantics must be fingerprintable");
        };
        assert_ne!(fingerprint, [0_u8; 32]);
        fingerprint
    };

    let (baseline_binding, baseline_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    let baseline = fingerprint(&baseline_binding, &baseline_permission);
    let (equivalent_binding, equivalent_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        true,
        true,
        true,
    );
    assert_eq!(
        fingerprint(&equivalent_binding, &equivalent_permission),
        baseline,
        "fresh owners, source order, and nested JSON order must not affect semantics"
    );

    let (source_kind_binding, source_kind_permission) = fixture(
        RuntimeSourceKind::Mcp,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&source_kind_binding, &source_kind_permission),
        baseline,
        "runtime source kind must affect the fingerprint"
    );

    let (plugin_id_binding, plugin_id_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin-other",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&plugin_id_binding, &plugin_id_permission),
        baseline,
        "plugin configured ID must affect the fingerprint"
    );

    let (plugin_digest_binding, plugin_digest_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [3; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&plugin_digest_binding, &plugin_digest_permission),
        baseline,
        "plugin declaration digest must affect the fingerprint"
    );

    let (plugin_resource_binding, plugin_resource_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-two",
        [2; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&plugin_resource_binding, &plugin_resource_permission),
        baseline,
        "plugin resource semantics must affect the fingerprint"
    );

    let (mcp_digest_binding, mcp_digest_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [3; 32],
        "mcp-one",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&mcp_digest_binding, &mcp_digest_permission),
        baseline,
        "zero-export MCP declaration digest must affect the fingerprint"
    );

    let (mcp_resource_binding, mcp_resource_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-two",
        true,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&mcp_resource_binding, &mcp_resource_permission),
        baseline,
        "zero-export MCP resources must affect the fingerprint"
    );

    let (without_mcp_binding, without_mcp_permission) = fixture(
        RuntimeSourceKind::Plugin,
        "fixture-plugin",
        [1; 32],
        "plugin-one",
        [2; 32],
        "mcp-one",
        false,
        false,
        false,
    );
    assert_ne!(
        fingerprint(&without_mcp_binding, &without_mcp_permission),
        baseline,
        "removing a zero-export MCP source must affect the fingerprint"
    );
}

#[test]
fn missing_and_filtered_alias_targets_fail_typed() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-miss",
            agent(
                "alias-miss",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "marker".to_string(),
                        "harness:tool/dynamic_marker".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    // Register after construction so basic_tools stays builtins-only.
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-miss"))
        .unwrap();
    let policy = binding.agent_resource_policy("alias-miss").unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::UnknownResourceReference { .. }) => {}
        Ok(_) => panic!("expected unknown filtered alias target"),
        Err(other) => panic!("expected unknown filtered alias target, got {other:?}"),
    }
}

#[test]
fn deny_filtered_alias_target_fails_typed() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-denied",
            agent(
                "alias-denied",
                ResourceView {
                    allow: Vec::new(),
                    deny: vec!["harness:tool/read".to_string()],
                    aliases: BTreeMap::from([(
                        "reader".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-denied"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("alias-denied", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::UnknownResourceReference { .. }) => {}
        Ok(_) => panic!("expected denied alias target to fail"),
        Err(other) => panic!("expected denied alias target to fail, got {other:?}"),
    }
}

#[test]
fn alias_cannot_override_qualified_stable_name() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-qualified",
            agent(
                "alias-qualified",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "harness:tool/read".to_string(),
                        "harness:tool/write".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-qualified"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("alias-qualified", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::AliasCollision { name, .. }) => {
            assert_eq!(name, "harness:tool/read");
        }
        Ok(_) => panic!("expected qualified alias override to fail"),
        Err(other) => panic!("expected alias collision, got {other:?}"),
    }
}

#[test]
fn alias_collision_with_public_short_name_fails_typed() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-collide",
            agent(
                "alias-collide",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "read".to_string(),
                        "harness:tool/write".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-collide"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("alias-collide", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::AliasCollision { bundle_id, name }) => {
            assert_eq!(bundle_id, "hya/alias-collide");
            assert_eq!(name, "read");
        }
        Ok(_) => panic!("expected alias collision"),
        Err(other) => panic!("expected alias collision, got {other:?}"),
    }
}

#[test]
fn local_skill_short_name_wins_and_filtered_local_restores_harness_short() {
    let local = PreparedResource {
        local_id: "shared".to_string(),
        stable_id: "bundle:hya/skill-collision/skill/shared".to_string(),
        source_path: "resources/skills/shared.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("shared", "LOCAL"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/skill-collision",
            agent("skill-agent", ResourceView::default()),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let workdir = tempfile_skill_workdir("shared", "HARNESS");
    let binding = registry.bind_turn(&workdir).unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("skill-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let names = compiled
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(names.contains("shared"));
    assert!(names.contains("harness:skill/shared"));
    let shared = compiled
        .skills()
        .iter()
        .find(|skill| skill.name == "shared")
        .unwrap();
    assert!(shared.content.contains("LOCAL"));

    let filtered_catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/skill-collision",
            agent(
                "skill-agent",
                ResourceView {
                    allow: vec![
                        "harness:skill/shared".to_string(),
                        "harness:tool/skill".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![PreparedResource {
                local_id: "shared".to_string(),
                stable_id: "bundle:hya/skill-collision/skill/shared".to_string(),
                source_path: "resources/skills/shared.md".to_string(),
                digest: "test-only".to_string(),
                content: skill_md("shared", "LOCAL"),
                binary_base64: None,
                aliases: Vec::new(),
            }],
        )])
        .unwrap(),
    );
    let filtered_registry = test_runtime_registry(ToolRegistry::builtins(), filtered_catalog);
    let filtered_binding = filtered_registry.bind_turn(&workdir).unwrap();
    let filtered_policy = filtered_binding
        .agent_resource_policy_on_plane("skill-agent", AgentToolPlane::Full)
        .unwrap();
    let filtered = filtered_binding
        .compile_agent_resources(&filtered_policy)
        .unwrap();
    let filtered_names = filtered
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        filtered_names,
        BTreeSet::from(["shared", "harness:skill/shared"]),
        "filtering the local restores harness short and keeps qualified spelling"
    );
    assert!(
        filtered
            .skills()
            .iter()
            .any(|skill| skill.name == "shared" && skill.content.contains("HARNESS"))
    );
}

#[test]
fn model_schemas_use_explicit_aliases_and_canonical_short_names() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/schema-dispatch",
            agent(
                "sd-agent",
                ResourceView {
                    allow: vec![
                        "harness:tool/read".to_string(),
                        "harness:tool/write".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "reader".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-schema-dispatch"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("sd-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let schema_names = domain_schema_names(&compiled);
    assert_eq!(
        schema_names,
        BTreeSet::from(["reader".to_string(), "write".to_string()])
    );
    assert!(compiled.resolve_tool("reader").is_some());
    assert!(compiled.resolve_tool("read").is_none());
    assert!(compiled.resolve_tool("write").is_some());
    assert!(
        compiled.resolve_tool("harness:tool/read").is_some(),
        "alias must preserve the exact qualified binding"
    );
    assert!(compiled.resolve_tool("harness:tool/write").is_some());
}

#[test]
fn a_bundle_local_allow_list_yields_no_skill_facade_and_inlines_the_skill() {
    let local = PreparedResource {
        local_id: "bundle-skill".to_string(),
        stable_id: "bundle:hya/none-inline/skill/bundle-skill".to_string(),
        source_path: "resources/skills/bundle-skill.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("bundle-skill", "INLINE_BODY_MARKER"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/none-inline",
            agent(
                "none-agent",
                ResourceView {
                    allow: vec!["bundle:hya/none-inline/skill/bundle-skill".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-none-inline"))
        .unwrap();
    let policy = binding.agent_resource_policy("none-agent").unwrap();
    assert_eq!(
        policy.selected_bundle_skill_ids(),
        &["bundle:hya/none-inline/skill/bundle-skill".to_string()]
    );
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    assert!(
        domain_tool_names(&compiled).is_empty(),
        "an allow list of only bundle-local resources admits no harness domain tool"
    );
    assert!(
        compiled.resolve_tool("skill").is_none(),
        "no harness skill tool is inserted when none is selected"
    );
    assert!(
        compiled.resolve_tool("dynamic_marker").is_none(),
        "dispatch must not fall back to the live registry"
    );
    assert!(
        compiled.resolve_tool("bash").is_none(),
        "dispatch must not fall back to builtins outside the compiled view"
    );
    assert!(
        has_mail_only_read(&compiled),
        "without its own read the view gets only the mail-only channel read"
    );
    let section = compiled
        .skills_prompt_section()
        .expect("selected local static skill must produce prompt content");
    assert!(
        section.contains("INLINE_BODY_MARKER"),
        "local static skill body must be inlined when no skill facade exists: {section}"
    );
    assert!(
        section.contains("bundle-skill"),
        "local static skill name must remain consumable via prompt content"
    );
    let unique_paths = compiled
        .skills()
        .iter()
        .map(|skill| skill.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unique_paths.len(),
        1,
        "short and qualified public spellings share one selected local skill"
    );
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == "bundle-skill")
    );
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| { skill.name == "bundle:hya/none-inline/skill/bundle-skill" }),
        "qualified public spelling must be real for selected local skills"
    );
}

#[test]
fn qualified_public_spellings_namespace_alias_and_dispatch() {
    let local = PreparedResource {
        local_id: "probe".to_string(),
        stable_id: "bundle:hya/qualified-map/skill/probe".to_string(),
        source_path: "resources/skills/probe.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("probe", "LOCAL_PROBE"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/qualified-map",
            agent(
                "q-agent",
                ResourceView {
                    allow: vec![
                        "harness:tool/read".to_string(),
                        "harness:tool/write".to_string(),
                        "bundle:hya/qualified-map/skill/probe".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::from([
                        ("reader".to_string(), "harness:tool/read".to_string()),
                        ("book".to_string(), "harness:tool/read".to_string()),
                    ]),
                    namespace: Some("custom.ns".to_string()),
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-qualified-map"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("q-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let schema_names = domain_schema_names(&compiled);
    assert_eq!(
        schema_names,
        BTreeSet::from([
            "book".to_string(),
            "reader".to_string(),
            "write".to_string()
        ])
    );
    assert!(compiled.resolve_tool("reader").is_some());
    assert!(compiled.resolve_tool("book").is_some());
    assert!(
        compiled.resolve_tool("read").is_none(),
        "alias replaces the ordinary short spelling"
    );
    assert!(
        compiled.resolve_tool("harness:tool/read").is_some(),
        "exact qualified binding must remain after aliases"
    );
    assert!(compiled.resolve_tool("write").is_some());
    assert!(compiled.resolve_tool("harness:tool/write").is_some());
    let skill_names = compiled
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        skill_names.contains("probe"),
        "bundle-local short remains available: {skill_names:?}"
    );
    assert!(
        skill_names.contains("bundle:custom.ns/skill/probe"),
        "namespace changes only the bundle-local qualified public spelling: {skill_names:?}"
    );
    assert!(
        !skill_names.contains("bundle:hya/qualified-map/skill/probe")
            || skill_names.contains("bundle:custom.ns/skill/probe"),
        "custom namespace must be the public qualified spelling"
    );
}

#[test]
fn harness_mcp_is_source_owned_full_only_and_pins_with_binding() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[
            bundle_with_agent(
                "hya/mcp-full",
                agent("full-mcp", ResourceView::default()),
                Vec::new(),
            ),
            bundle_with_agent(
                "hya/mcp-basic",
                agent("basic-mcp", ResourceView::default()),
                Vec::new(),
            ),
            bundle_with_agent(
                "hya/mcp-allow",
                agent(
                    "allow-mcp",
                    ResourceView {
                        allow: vec!["harness:mcp/mcp__fixture__ping".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                Vec::new(),
            ),
            bundle_with_agent(
                "hya/mcp-deny",
                agent(
                    "deny-mcp",
                    ResourceView {
                        allow: Vec::new(),
                        deny: vec!["harness:mcp/mcp__fixture__ping".to_string()],
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                Vec::new(),
            ),
        ])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::mcp("fixture"),
                [7; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    Vec::new(),
                    Arc::new(NoopTool::new("mcp__fixture__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let workdir = Path::new("/tmp/hya-mcp-kind");
    let binding = registry.bind_turn(workdir).unwrap();

    let full = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(full.resolve_tool("mcp__fixture__ping").is_some());
    assert!(
        full.resolve_tool("harness:mcp/mcp__fixture__ping")
            .is_some()
    );
    assert!(
        full.resolve_tool("harness:tool/mcp__fixture__ping")
            .is_none(),
        "MCP must not be re-homed under the tool kind"
    );

    let basic = binding
        .compile_agent_resources(&binding.agent_resource_policy("basic-mcp").unwrap())
        .unwrap();
    assert!(basic.resolve_tool("mcp__fixture__ping").is_none());
    assert!(
        basic
            .resolve_tool("harness:mcp/mcp__fixture__ping")
            .is_none()
    );

    let allowed = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("allow-mcp", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(allowed.resolve_tool("mcp__fixture__ping").is_some());
    assert!(!domain_tool_names(&allowed).contains("read"));

    let denied = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("deny-mcp", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(denied.resolve_tool("mcp__fixture__ping").is_none());
    assert!(denied.resolve_tool("read").is_some());

    // Pin: remove MCP source after binding capture.
    registry
        .refresh(|candidate| {
            let mut removed = BTreeSet::new();
            removed.insert(RuntimeSourceId::mcp("fixture"));
            candidate.remove_sources(&removed);
            Ok(())
        })
        .unwrap();
    let pinned = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(
        pinned.resolve_tool("mcp__fixture__ping").is_some(),
        "old TurnBinding must pin the prior MCP view"
    );
    let fresh = registry.bind_turn(workdir).unwrap();
    let after = fresh
        .compile_agent_resources(
            &fresh
                .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(after.resolve_tool("mcp__fixture__ping").is_none());
}

#[test]
fn global_canonical_reference_rejects_wrong_kind_and_cross_kind_short_ambiguity() {
    let local = PreparedResource {
        local_id: "shared".to_string(),
        stable_id: "bundle:hya/global-ref/skill/shared".to_string(),
        source_path: "resources/skills/shared.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("shared", "LOCAL"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let wrong_kind = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/global-ref",
            agent(
                "wrong-kind",
                ResourceView {
                    allow: vec!["harness:skill/read".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), wrong_kind);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-global-wrong-kind"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("wrong-kind", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::UnknownResourceReference { reference, .. }) => {
            assert_eq!(reference, "harness:skill/read");
        }
        Ok(_) => panic!("wrong-kind prefix must typed-reject"),
        Err(other) => panic!("expected unknown resource reference, got {other:?}"),
    }

    let ambiguous = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/global-ref",
            agent(
                "ambiguous",
                ResourceView {
                    allow: vec!["shared".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), ambiguous);
    // Register a harness tool whose short name collides with the local skill.
    registry
        .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-global-ambiguous"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("ambiguous", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::NamespaceCollision { name, .. }) => {
            assert_eq!(name, "shared");
        }
        Ok(_) => panic!("cross-kind short ambiguity must typed-reject"),
        Err(other) => panic!("expected namespace collision, got {other:?}"),
    }
}

#[test]
fn alias_cannot_occupy_same_target_stable_spelling() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-self",
            agent(
                "alias-self",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "read".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-self"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("alias-self", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::AliasCollision { name, .. }) => {
            assert_eq!(name, "read");
        }
        Ok(_) => panic!("alias occupying the target short spelling must fail"),
        Err(other) => panic!("expected alias collision, got {other:?}"),
    }
}

#[test]
fn tool_and_skill_may_share_public_spelling_independently() {
    let local = PreparedResource {
        local_id: "read".to_string(),
        stable_id: "bundle:hya/cross-kind-ok/skill/read".to_string(),
        source_path: "resources/skills/read.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("read", "SKILL_READ_BODY"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/cross-kind-ok",
            agent(
                "cross-ok",
                ResourceView {
                    allow: vec![
                        "harness:tool/read".to_string(),
                        "bundle:hya/cross-kind-ok/skill/read".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-cross-kind-ok"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("cross-ok", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding
        .compile_agent_resources(&policy)
        .expect("tool and skill may share public spelling `read`");
    let tool = compiled
        .resolve_tool("read")
        .expect("tool public name `read` must dispatch");
    assert_eq!(tool.tool.name(), "read");
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == "read" && skill.content.contains("SKILL_READ_BODY")),
        "skill public name `read` must remain independently addressable"
    );
}

#[test]
fn tool_mcp_public_name_collision_is_typed_rejected() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/tool-mcp-collide",
            agent(
                "collide",
                ResourceView {
                    allow: vec![
                        "harness:tool/read".to_string(),
                        "harness:mcp/mcp__fixture__ping".to_string(),
                    ],
                    deny: Vec::new(),
                    // Alias the tool onto the MCP public short spelling.
                    aliases: BTreeMap::from([(
                        "mcp__fixture__ping".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::mcp("fixture"),
                [1; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    Vec::new(),
                    Arc::new(NoopTool::new("mcp__fixture__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-tool-mcp-collide"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("collide", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::NamespaceCollision { name, .. })
        | Err(BundleError::AliasCollision { name, .. }) => {
            assert_eq!(name, "mcp__fixture__ping");
        }
        Ok(_) => panic!("tool/MCP shared dispatch collision must typed-reject"),
        Err(other) => panic!("expected tool/MCP collision, got {other:?}"),
    }
}

#[test]
fn cross_kind_ambiguous_alias_target_fails_typed() {
    let local = PreparedResource {
        local_id: "shared".to_string(),
        stable_id: "bundle:hya/alias-ambiguous/skill/shared".to_string(),
        source_path: "resources/skills/shared.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("shared", "LOCAL"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/alias-ambiguous",
            agent(
                "alias-amb",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([("marker".to_string(), "shared".to_string())]),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-alias-ambiguous"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("alias-amb", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::NamespaceCollision { name, .. }) => {
            assert_eq!(name, "shared");
        }
        Ok(_) => panic!("cross-kind ambiguous alias target must typed-reject"),
        Err(other) => panic!("expected namespace collision, got {other:?}"),
    }
}

#[test]
fn both_planes_keep_builtin_aliases_out_of_model_schemas() {
    let expected = ToolRegistry::builtins()
        .schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(expected.len(), 28);
    for (agent_id, plane, workdir) in [
        (
            "full-alias",
            AgentToolPlane::Full,
            "/tmp/hya-builtin-alias-full",
        ),
        (
            "internal-public-alias",
            AgentToolPlane::InternalPublic,
            "/tmp/hya-builtin-alias-internal-public",
        ),
    ] {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/builtin-alias",
                agent(agent_id, ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry.bind_turn(Path::new(workdir)).unwrap();
        let policy = binding
            .agent_resource_policy_on_plane(agent_id, plane)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            compiled.resolve_tool("apply_patch").is_some(),
            "{agent_id}: canonical apply_patch must remain public"
        );
        for alias in ["fetch", "search", "patch", "plan", "shell", "question"] {
            assert!(
                compiled.resolve_tool(alias).is_some(),
                "{agent_id}: hidden alias `{alias}` must remain dispatchable"
            );
        }
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        // A bundle agent that can spawn nobody gets no `task`/`archive`.
        let mut expected = expected.clone();
        for name in crate::coordination::SPAWN_TOOLS {
            expected.remove(name);
        }
        assert_eq!(
            schema_names, expected,
            "{agent_id}: canonical schema set drifted"
        );
        assert!(
            schema_names.contains("apply_patch"),
            "{agent_id}: canonical schema must remain model-facing"
        );
        assert!(
            !schema_names.contains("patch") && !schema_names.contains("shell"),
            "{agent_id}: hidden dispatch aliases must not be advertised"
        );
        assert!(
            schema_names.iter().all(|name| !name.contains([':', '/'])),
            "{agent_id}: qualified dispatch identities must not reach providers: {schema_names:?}"
        );
    }
}

#[test]
fn explicit_view_alias_suppresses_candidate_aliases_for_target() {
    // Consult23: after ResourceView explicitly aliases a selected canonical
    // target, only the explicit public aliases and the qualified stable name
    // remain callable. Candidate registry aliases (e.g. `patch`) must not
    // re-enter the public map and bypass the mapping.
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/view-alias-suppress",
            agent(
                "suppress-agent",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "applier".to_string(),
                        "harness:tool/apply_patch".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-view-alias-suppress"))
        .unwrap();
    let compiled = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("suppress-agent", AgentToolPlane::Full)
                .unwrap(),
        )
        .expect("explicit view alias of apply_patch must compile");

    assert!(
        compiled.resolve_tool("applier").is_some(),
        "explicit view alias `applier` must be callable"
    );
    assert!(
        compiled.resolve_tool("harness:tool/apply_patch").is_some(),
        "qualified stable name must remain callable"
    );
    assert!(
        compiled.resolve_tool("apply_patch").is_none(),
        "ordinary short spelling must be removed after explicit aliasing"
    );
    assert!(
        compiled.resolve_tool("patch").is_none(),
        "preexisting candidate alias `patch` must not re-enter after explicit aliasing"
    );

    let schema_names = compiled
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    assert!(
        schema_names
            .iter()
            .all(|name| compiled.resolve_tool(name).is_some())
    );
    assert!(schema_names.contains("applier"));
    assert!(!schema_names.contains("harness:tool/apply_patch"));
    assert!(!schema_names.contains("apply_patch"));
    assert!(!schema_names.contains("patch"));
    assert!(
        schema_names
            .iter()
            .all(|name| !name.contains(':') && !name.contains('/'))
    );
}

#[test]
fn invalid_provider_tool_alias_fails_during_resource_compile() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/invalid-provider-alias",
            agent(
                "invalid-alias-agent",
                ResourceView {
                    allow: vec!["harness:tool/read".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "bad:name".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-invalid-provider-alias"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("invalid-alias-agent", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::InvalidManifest { detail, .. }) => {
            assert!(detail.contains("model-facing tool name `bad:name`"));
        }
        Ok(_) => panic!("invalid provider Tool alias must fail before a request"),
        Err(other) => panic!("expected invalid manifest, got {other:?}"),
    }
}

#[test]
fn explicit_view_alias_collides_with_candidate_alias_even_same_target() {
    // Explicit view alias named `patch` targeting harness:tool/apply_patch
    // collides with the existing candidate alias of that same tool.
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/view-alias-candidate-collide",
            agent(
                "collide-agent",
                ResourceView {
                    allow: Vec::new(),
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "patch".to_string(),
                        "harness:tool/apply_patch".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-view-alias-candidate-collide"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("collide-agent", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::AliasCollision { name, .. }) => {
            assert_eq!(name, "patch");
        }
        Ok(_) => panic!(
            "explicit alias occupying an existing candidate alias must typed-reject even for the same target"
        ),
        Err(other) => panic!("expected alias collision, got {other:?}"),
    }
}

#[test]
fn mcp_and_skill_may_share_public_spelling_independently() {
    // Only tool versus MCP share invocation syntax. An MCP public spelling
    // and an independent skill public spelling may be identical and both
    // remain addressable.
    let local = PreparedResource {
        local_id: "pingy".to_string(),
        stable_id: "bundle:hya/mcp-skill-ok/skill/pingy".to_string(),
        source_path: "resources/skills/pingy.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("pingy", "SKILL_PINGY_BODY"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/mcp-skill-ok",
            agent(
                "mcp-skill-ok",
                ResourceView {
                    allow: vec![
                        "harness:mcp/mcp__fixture__ping".to_string(),
                        "bundle:hya/mcp-skill-ok/skill/pingy".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::mcp("fixture"),
                [5; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    vec!["pingy".to_string()],
                    Arc::new(NoopTool::new("mcp__fixture__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-mcp-skill-ok"))
        .unwrap();
    let compiled = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("mcp-skill-ok", AgentToolPlane::Full)
                .unwrap(),
        )
        .expect("MCP and skill may share public spelling `pingy`");
    let mcp = compiled
        .resolve_tool("pingy")
        .expect("MCP public spelling `pingy` must dispatch");
    assert_eq!(mcp.tool.name(), "mcp__fixture__ping");
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == "pingy" && skill.content.contains("SKILL_PINGY_BODY")),
        "skill public name `pingy` must remain independently addressable"
    );
}

#[test]
fn full_mcp_preserves_source_alias_under_mcp_kind() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/mcp-alias",
            agent("mcp-alias", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::mcp("fixture"),
                [3; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    vec!["pingy".to_string()],
                    Arc::new(NoopTool::new("mcp__fixture__ping")),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let binding = registry.bind_turn(Path::new("/tmp/hya-mcp-alias")).unwrap();
    let compiled = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("mcp-alias", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(compiled.resolve_tool("mcp__fixture__ping").is_some());
    assert!(
        compiled.resolve_tool("pingy").is_some(),
        "source export alias must be a public MCP spelling"
    );
    assert!(
        compiled
            .resolve_tool("harness:mcp/mcp__fixture__ping")
            .is_some()
    );
    let schema_names = compiled
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    assert!(
        schema_names
            .iter()
            .all(|name| compiled.resolve_tool(name).is_some())
    );
    assert!(schema_names.contains("mcp__fixture__ping"));
    assert!(!schema_names.contains("pingy"));
    assert!(!schema_names.contains("harness:mcp/mcp__fixture__ping"));
}

#[test]
fn prepared_skill_alias_is_public_spelling_for_skill_plane() {
    let local = PreparedResource {
        local_id: "docs".to_string(),
        stable_id: "bundle:hya/skill-alias/skill/docs".to_string(),
        source_path: "resources/skills/docs.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("docs", "DOCS_BODY"),
        binary_base64: None,
        aliases: vec!["handbook".to_string()],
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/skill-alias",
            agent(
                "skill-alias",
                ResourceView {
                    allow: vec!["bundle:hya/skill-alias/skill/docs".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-skill-alias"))
        .unwrap();
    let compiled = binding
        .compile_agent_resources(&binding.agent_resource_policy("skill-alias").unwrap())
        .unwrap();
    assert!(compiled.skills().iter().any(|skill| skill.name == "docs"));
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == "handbook" && skill.content.contains("DOCS_BODY")),
        "PreparedResource alias must be a dispatchable skill public spelling"
    );
}

#[test]
fn alias_cannot_impersonate_filtered_stable_name() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/filtered-stable",
            agent(
                "filtered-stable",
                ResourceView {
                    allow: vec!["harness:tool/read".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "write".to_string(),
                        "harness:tool/read".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-filtered-stable"))
        .unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("filtered-stable", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::AliasCollision { name, .. }) => {
            assert_eq!(name, "write");
        }
        Ok(_) => panic!("alias must not impersonate unselected stable identity"),
        Err(other) => panic!("expected alias collision, got {other:?}"),
    }
}

#[test]
fn aliased_skill_facade_drives_prompt_not_public_key_skill() {
    let local = PreparedResource {
        local_id: "bundle-skill".to_string(),
        stable_id: "bundle:hya/facade-alias/skill/bundle-skill".to_string(),
        source_path: "resources/skills/bundle-skill.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("bundle-skill", "SHOULD_NOT_INLINE"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/facade-alias",
            agent(
                "facade-alias",
                ResourceView {
                    allow: vec![
                        "harness:tool/skill".to_string(),
                        "bundle:hya/facade-alias/skill/bundle-skill".to_string(),
                    ],
                    deny: Vec::new(),
                    aliases: BTreeMap::from([(
                        "load_skill".to_string(),
                        "harness:tool/skill".to_string(),
                    )]),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-facade-alias"))
        .unwrap();
    let compiled = binding
        .compile_agent_resources(
            &binding
                .agent_resource_policy_on_plane("facade-alias", AgentToolPlane::Full)
                .unwrap(),
        )
        .unwrap();
    assert!(compiled.resolve_tool("load_skill").is_some());
    assert!(
        compiled.resolve_tool("skill").is_none(),
        "resource-view alias replaces the ordinary short spelling of the facade"
    );
    let section = compiled
        .skills_prompt_section()
        .expect("facade selected => on-demand skill index");
    assert!(
        !section.contains("SHOULD_NOT_INLINE"),
        "aliased skill facade must not fall through to body inlining: {section}"
    );
    assert!(
        section.contains("bundle-skill"),
        "index must list the selected skill: {section}"
    );
}

#[test]
fn skill_prompt_lists_all_dispatchable_spellings_with_short_preference() {
    let local = PreparedResource {
        local_id: "probe".to_string(),
        stable_id: "bundle:hya/spellings/skill/probe".to_string(),
        source_path: "resources/skills/probe.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("probe", "PROBE_BODY"),
        binary_base64: None,
        aliases: vec!["probe-alias".to_string()],
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/spellings",
            agent(
                "spellings",
                ResourceView {
                    allow: vec!["bundle:hya/spellings/skill/probe".to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry.bind_turn(Path::new("/tmp/hya-spellings")).unwrap();
    let compiled = binding
        .compile_agent_resources(&binding.agent_resource_policy("spellings").unwrap())
        .unwrap();
    let section = compiled
        .skills_prompt_section()
        .expect("a bundle-local-only view inlines selected local static skills");
    assert!(
        section.contains("probe"),
        "short spelling preferred in prompt: {section}"
    );
    assert!(
        section.contains("probe-alias") || section.contains("bundle:hya/spellings/skill/probe"),
        "prompt must expose additional dispatchable spellings honestly: {section}"
    );
    assert_eq!(
        section.matches("PROBE_BODY").count(),
        1,
        "content must be emitted once with a spelling list: {section}"
    );
}

#[test]
fn selected_harness_skill_without_facade_fails_typed() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/no-facade",
            agent(
                "no-facade",
                ResourceView {
                    allow: Vec::new(),
                    deny: vec!["harness:tool/skill".to_string()],
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let workdir = tempfile_skill_workdir("workdir-only", "MUST_NOT_INLINE");
    let binding = registry.bind_turn(&workdir).unwrap();
    let policy = binding
        .agent_resource_policy_on_plane("no-facade", AgentToolPlane::Full)
        .unwrap();
    match binding.compile_agent_resources(&policy) {
        Err(BundleError::InvalidManifest { detail, .. }) => {
            assert!(
                detail.contains("skill") || detail.contains("facade"),
                "typed rejection must mention missing skill facade: {detail}"
            );
        }
        Err(BundleError::UnknownResourceReference { .. }) => {}
        Ok(compiled) => {
            let section = compiled.skills_prompt_section().unwrap_or_default();
            panic!(
                "selected harness skills without facade must typed-reject, not expose content: {section}"
            );
        }
        Err(other) => panic!("expected typed facade rejection, got {other:?}"),
    }
}

#[test]
fn bundle_id_with_kind_path_segments_parses_structurally() {
    let bundle_id = "hya/tool/skill/mcp-nest";
    let local = PreparedResource {
        local_id: "docs".to_string(),
        stable_id: format!("bundle:{bundle_id}/skill/docs"),
        source_path: "resources/skills/docs.md".to_string(),
        digest: "test-only".to_string(),
        content: skill_md("docs", "NESTED_BUNDLE_BODY"),
        binary_base64: None,
        aliases: Vec::new(),
    };
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            bundle_id,
            agent(
                "nested-agent",
                ResourceView {
                    allow: vec![format!("bundle:{bundle_id}/skill/docs")],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
            ),
            vec![local],
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-nested-bundle-id"))
        .unwrap();
    let compiled = binding
        .compile_agent_resources(&binding.agent_resource_policy("nested-agent").unwrap())
        .expect("rightmost kind/local parse must accept nested kind segments in bundle id");
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == "docs" && skill.content.contains("NESTED_BUNDLE_BODY"))
    );
    assert!(
        compiled
            .skills()
            .iter()
            .any(|skill| skill.name == format!("bundle:{bundle_id}/skill/docs"))
    );
}

#[test]
fn bundle_sidecar_collision_requires_provider_safe_names_for_both_targets() {
    let bundle_id = "hya/sidecar-map";
    let mut bundle = bundle_with_agent(
        bundle_id,
        agent("sidecar-agent", ResourceView::default()),
        Vec::new(),
    );
    bundle.tools.push(PreparedResource {
        local_id: "echo".to_string(),
        stable_id: format!("bundle:{bundle_id}/tool/echo"),
        source_path: "tools/echo.js".to_string(),
        digest: "test-only-digest".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    });
    let catalog = Arc::new(TestCatalog::from_prepared(&[bundle]).unwrap());

    let tools = ToolRegistry::builtins();
    tools
        .register_with_permission(Arc::new(NoopTool::new("echo")), ToolPermission::Tool)
        .unwrap();
    let registry = test_runtime_registry(tools, catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-sidecar-tool-map"))
        .unwrap();
    let mut policy = binding
        .agent_resource_policy_on_plane("sidecar-agent", AgentToolPlane::Full)
        .unwrap();
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(NoopTool::new(format!("bundle:{bundle_id}/tool/echo"))),
        permission: ToolPermission::Tool,
    };

    match binding
        .compile_agent_resources_with_sidecar_tools(&policy, std::slice::from_ref(&sidecar_tool))
    {
        Err(BundleError::InvalidManifest { detail, .. }) => {
            assert!(detail.contains("harness:tool/echo"));
            assert!(detail.contains("no provider-safe schema name"));
        }
        Ok(_) => panic!("selected harness Tool must not silently vanish from schemas"),
        Err(other) => panic!("expected provider-safe schema rejection, got {other:?}"),
    }

    policy
        .resource_view
        .aliases
        .insert("harness_echo".to_string(), "harness:tool/echo".to_string());
    let compiled = binding
        .compile_agent_resources_with_sidecar_tools(&policy, &[sidecar_tool])
        .expect("an explicit provider-safe alias must preserve both selected Tools");
    let schema_names = compiled
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    assert!(schema_names.contains("echo"));
    assert!(schema_names.contains("harness_echo"));
    assert!(!schema_names.contains(&format!("bundle:{bundle_id}/tool/echo")));
    assert!(!schema_names.contains("harness:tool/echo"));

    let bundle_short = compiled.resolve_tool("echo").unwrap();
    assert_eq!(
        bundle_short.tool.name(),
        format!("bundle:{bundle_id}/tool/echo")
    );
    let bundle_qualified = compiled
        .resolve_tool(&format!("bundle:{bundle_id}/tool/echo"))
        .unwrap();
    assert_eq!(
        bundle_qualified.tool.name(),
        format!("bundle:{bundle_id}/tool/echo")
    );
    let harness_alias = compiled.resolve_tool("harness_echo").unwrap();
    assert_eq!(harness_alias.tool.name(), "echo");
    let harness_qualified = compiled.resolve_tool("harness:tool/echo").unwrap();
    assert_eq!(harness_qualified.tool.name(), "echo");
}

#[test]
fn captured_agent_resource_policy_retains_disjoint_bundle_tool_and_hook_ids() {
    // One agent per bundle, so disjointness is now across two installed
    // bundles rather than across agents inside one.
    let alpha_bundle_id = "hya/disjoint-alpha";
    let beta_bundle_id = "hya/disjoint-beta";
    let alpha_tool_id = format!("bundle:{alpha_bundle_id}/tool/alpha");
    let beta_tool_id = format!("bundle:{beta_bundle_id}/tool/beta");
    let alpha_hook_id = format!("bundle:{alpha_bundle_id}/hook/event");
    let beta_hook_id = format!("bundle:{beta_bundle_id}/hook/tool.execute.before");

    let mut alpha = agent(
        "alpha-agent",
        ResourceView {
            allow: vec![alpha_tool_id.clone()],
            deny: Vec::new(),
            aliases: BTreeMap::new(),
            namespace: None,
        },
    );
    alpha.hook_refs = vec![alpha_hook_id.clone()];

    let mut beta = agent(
        "beta-agent",
        ResourceView {
            allow: vec![beta_tool_id.clone()],
            deny: Vec::new(),
            aliases: BTreeMap::new(),
            namespace: None,
        },
    );
    beta.hook_refs = vec![beta_hook_id.clone()];

    let mut alpha_bundle = bundle_with_agent(alpha_bundle_id, alpha, Vec::new());
    alpha_bundle.tools = vec![PreparedResource {
        local_id: "alpha".to_string(),
        stable_id: alpha_tool_id.clone(),
        source_path: "extensions/alpha.js".to_string(),
        digest: "alpha-tool".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    }];
    alpha_bundle.hooks = vec![PreparedResource {
        local_id: "event".to_string(),
        stable_id: alpha_hook_id.clone(),
        source_path: "extensions/event.js".to_string(),
        digest: "alpha-hook".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    }];

    let mut beta_bundle = bundle_with_agent(beta_bundle_id, beta, Vec::new());
    beta_bundle.tools = vec![PreparedResource {
        local_id: "beta".to_string(),
        stable_id: beta_tool_id.clone(),
        source_path: "extensions/beta.js".to_string(),
        digest: "beta-tool".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    }];
    beta_bundle.hooks = vec![PreparedResource {
        local_id: "tool.execute.before".to_string(),
        stable_id: beta_hook_id.clone(),
        source_path: "extensions/before.js".to_string(),
        digest: "beta-hook".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    }];

    let catalog = Arc::new(TestCatalog::from_prepared(&[alpha_bundle, beta_bundle]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-disjoint-sidecars"))
        .unwrap();
    let alpha_policy = binding.agent_resource_policy("alpha-agent").unwrap();
    let beta_policy = binding.agent_resource_policy("beta-agent").unwrap();

    let alpha_selected_tools = alpha_policy
        .selected_bundle_tool_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let beta_selected_tools = beta_policy
        .selected_bundle_tool_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        alpha_selected_tools,
        BTreeSet::from([alpha_tool_id.clone()])
    );
    assert_eq!(beta_selected_tools, BTreeSet::from([beta_tool_id.clone()]));

    let alpha_hook_ids = alpha_policy
        .canonical_hook_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let beta_hook_ids = beta_policy
        .canonical_hook_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(alpha_hook_ids, BTreeSet::from([alpha_hook_id.clone()]));
    assert_eq!(beta_hook_ids, BTreeSet::from([beta_hook_id.clone()]));
    assert!(alpha_hook_ids.is_disjoint(&beta_hook_ids));

    let alpha_compiled = binding
        .compile_agent_resources_with_sidecar_tools(
            &alpha_policy,
            &[ResolvedTool {
                tool: Arc::new(NoopTool::new(alpha_tool_id.clone())),
                permission: ToolPermission::Tool,
            }],
        )
        .unwrap();
    let beta_compiled = binding
        .compile_agent_resources_with_sidecar_tools(
            &beta_policy,
            &[ResolvedTool {
                tool: Arc::new(NoopTool::new(beta_tool_id.clone())),
                permission: ToolPermission::Tool,
            }],
        )
        .unwrap();

    let alpha_compiled_hooks = alpha_compiled
        .canonical_hook_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let beta_compiled_hooks = beta_compiled
        .canonical_hook_ids()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(alpha_compiled_hooks, alpha_hook_ids);
    assert_eq!(beta_compiled_hooks, beta_hook_ids);

    let alpha_names = domain_tool_names(&alpha_compiled);
    let beta_names = domain_tool_names(&beta_compiled);
    assert!(alpha_names.contains("alpha"));
    assert!(alpha_names.contains(alpha_tool_id.as_str()));
    assert!(!alpha_names.contains("beta"));
    assert!(!alpha_names.contains(beta_tool_id.as_str()));
    assert!(beta_names.contains("beta"));
    assert!(beta_names.contains(beta_tool_id.as_str()));
    assert!(!beta_names.contains("alpha"));
    assert!(!beta_names.contains(alpha_tool_id.as_str()));
    assert!(alpha_names.is_disjoint(&beta_names));
}

fn tempfile_skill_workdir(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "hya-resource-view-skill-{}-{}",
        name,
        std::process::id()
    ));
    let skill_dir = path.join(".hya/skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), skill_md(name, body)).unwrap();
    path
}

/// A tool that records every reference argument it is dispatched with, so
/// scheme-dispatch tests can observe exactly what the owner received.
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
            description: "records dispatch references".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if let Some(reference) = input.get("reference").and_then(Value::as_str) {
            self.seen.lock().unwrap().push(reference.to_string());
        }
        Ok(json!({ "output": "owner body" }))
    }
}

fn dispatch_ctx(workdir: &Path) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: hya_tool::TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        agents: Default::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: tokio_util::sync::CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[test]
fn source_schema_claims_publish_and_dispatch_through_compiled_views() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/scheme",
            agent("scheme-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let owner = ReferenceRecordingTool::new("scheme__query");
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![scheme_source(
                "scheme-src",
                vec![RuntimeSourceExport::tool(
                    "scheme__query",
                    "scheme__query",
                    Vec::new(),
                    owner.clone(),
                    ToolPermission::ReadOnly,
                )],
                vec![SourceSchema {
                    scheme: "db".to_string(),
                    canonical_tool: "scheme__query".to_string(),
                    writable: false,
                }],
            )])
        })
        .expect("a source claiming its own export's scheme must publish");

    let workdir = PathBuf::from("/tmp/hya-scheme-dispatch");
    let binding = registry.bind_turn(&workdir).unwrap();
    let binding_binding = &binding;
    let schemes = binding_binding.schemes();
    assert_eq!(
        schemes.get("db").map(SchemeBinding::canonical_tool),
        Some("scheme__query"),
        "the winning binding must be published on the snapshot"
    );
    assert_eq!(
        binding.scheme_chain("db"),
        vec![("plugin:scheme-src".to_string(), "scheme__query".to_string())],
        "the chain lists the sole claimant"
    );
    let effective = registry.effective_schemes();
    assert_eq!(effective.schemes.len(), 1);

    let policy = binding
        .agent_resource_policy_on_plane("scheme-agent", AgentToolPlane::Full)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();

    let ctx = dispatch_ctx(&workdir);
    let read = compiled.resolve_tool("read").unwrap();
    let result = futures::executor::block_on(
        read.tool
            .execute(&ctx, json!({ "path": "db://x/y?head=2" })),
    )
    .unwrap();
    assert_eq!(
        result.get("output").and_then(Value::as_str),
        Some("owner body"),
        "read over a registered scheme dispatches the owner tool"
    );
    assert_eq!(
        owner.references(),
        vec!["db://x/y?head=2".to_string()],
        "the owner receives the full handle text as its reference"
    );

    let write = compiled.resolve_tool("write").unwrap();
    let error = futures::executor::block_on(
        write
            .tool
            .execute(&ctx, json!({ "path": "db://x/y", "content": "row" })),
    )
    .expect_err("a read-only scheme must reject write dispatch");
    assert!(
        error.to_string().contains("db:// is read-only"),
        "write dispatch must fail with the NotWritable-style error: {error}"
    );
    assert!(
        owner.references().len() == 1,
        "the rejected write must not reach the owner"
    );
}

#[test]
fn scheme_dispatch_stays_absent_when_the_view_lacks_the_owner() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/scheme-scope",
            agent("scheme-scope-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let owner = ReferenceRecordingTool::new("scheme__query");
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![scheme_source(
                "scheme-src",
                vec![RuntimeSourceExport::tool(
                    "scheme__query",
                    "scheme__query",
                    Vec::new(),
                    owner.clone(),
                    ToolPermission::ReadOnly,
                )],
                vec![SourceSchema {
                    scheme: "db".to_string(),
                    canonical_tool: "scheme__query".to_string(),
                    writable: false,
                }],
            )])
        })
        .expect("a valid scheme claim must publish");

    let workdir = PathBuf::from("/tmp/hya-scheme-scope");
    let binding = registry.bind_turn(&workdir).unwrap();
    // The bundle agent's InternalPublic plane never contains plugin tools,
    // so the owning tool is absent from this view.
    let policy = binding
        .agent_resource_policy_on_plane("scheme-scope-agent", AgentToolPlane::InternalPublic)
        .unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    assert!(
        compiled.resolve_tool("scheme__query").is_none(),
        "the owner tool must be absent from the restricted view"
    );

    let ctx = dispatch_ctx(&workdir);
    let read = compiled.resolve_tool("read").unwrap();
    let error = futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "db://x/y" })))
        .expect_err("a view without the owner keeps the historical unknown-scheme error");
    assert!(
        error.to_string().contains("unknown handle scheme"),
        "no global fallback may dispatch outside the view: {error}"
    );
    assert!(
        owner.references().is_empty(),
        "the owner tool must never be invoked from a view it is not in"
    );
}

/// A bundle agent whose view selects both the owner tool and the scheme
/// reads `scheme://` handles through the view's own sidecar tool.
#[test]
fn bundle_agent_view_with_sidecar_owner_reads_registered_scheme() {
    let view = ResourceView {
        allow: vec!["query".to_string(), "harness:tool/read".to_string()],
        deny: Vec::new(),
        aliases: BTreeMap::new(),
        namespace: None,
    };
    let mut bundle = bundle_with_agent("hya/vecdb", agent("vecdb-lead", view), Vec::new());
    bundle.tools.push(PreparedResource {
        local_id: "query".to_string(),
        stable_id: "bundle:hya/vecdb/tool/query".to_string(),
        source_path: "extensions/runtime.js".to_string(),
        digest: "test-only".to_string(),
        content: "export default {}".to_string(),
        binary_base64: None,
        aliases: Vec::new(),
    });
    let catalog = Arc::new(TestCatalog::from_prepared(&[bundle]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    registry
        .refresh(|candidate| {
            candidate.replace_sources_of_kind(
                RuntimeSourceKind::Bundle,
                vec![bundle_scheme_source(
                    "hya/vecdb",
                    "db",
                    "bundle:hya/vecdb/tool/query",
                )],
            )
        })
        .expect("the bundle claim must publish");

    let workdir = PathBuf::from("/tmp/hya-bundle-scheme-dispatch");
    let binding = registry.bind_turn(&workdir).unwrap();
    let policy = binding.agent_resource_policy("vecdb-lead").unwrap();
    let owner = ReferenceRecordingTool::new("bundle:hya/vecdb/tool/query");
    let sidecar = ResolvedTool {
        tool: owner.clone(),
        permission: ToolPermission::Tool,
    };
    let compiled = binding
        .compile_agent_resources_with_sidecar_tools(&policy, &[sidecar])
        .unwrap();

    let ctx = dispatch_ctx(&workdir);
    let read = compiled.resolve_tool("read").unwrap();
    let result =
        futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "db://rows/42" })))
            .unwrap();
    assert_eq!(
        result.get("output").and_then(Value::as_str),
        Some("owner body"),
        "read over the registered scheme dispatches the view's sidecar tool"
    );
    assert_eq!(
        owner.references(),
        vec!["db://rows/42".to_string()],
        "the owner receives the full handle text as its reference"
    );
}

/// Whether `name` spells a harness coordination tool (short or qualified).
fn is_coordination_spelling(name: &str) -> bool {
    let short = name.strip_prefix("harness:tool/").unwrap_or(name);
    crate::coordination::COORDINATION_TOOLS.contains(&short)
}

/// Whether the view's `read` is the injected mail-only channel reader.
fn has_mail_only_read(compiled: &CompiledResourceView) -> bool {
    compiled
        .resolve_tool("read")
        .is_some_and(|read| read.tool.schema().description.starts_with("Read team mail"))
}

/// Public tool names minus the harness-injected coordination set: the
/// domain tools a `resource_view` actually narrows.
#[expect(clippy::nonminimal_bool, reason = "preserve the upstream filter")]
fn domain_tool_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
    let mail_only_read = has_mail_only_read(compiled);
    compiled
        .tools
        .keys()
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .filter(|name| !is_coordination_spelling(name) && !(mail_only_read && name == "read"))
        .collect()
}

/// Model-facing schema names minus the harness-injected coordination set.
#[expect(clippy::nonminimal_bool, reason = "preserve the upstream filter")]
fn domain_schema_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
    let mail_only_read = has_mail_only_read(compiled);
    compiled
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .filter(|name| !is_coordination_spelling(name) && !(mail_only_read && name == "read"))
        .collect()
}

// ---- Coordination tools (0.41.0): harness-owned, injected at startup ----

const WITHOUT_CHANNEL_TOOLS: [&str; 4] = [
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network-tools",
    "hya/todo-tools",
];

fn view(allow: &[&str], deny: &[&str]) -> ResourceView {
    ResourceView {
        allow: allow.iter().map(|entry| (*entry).to_string()).collect(),
        deny: deny.iter().map(|entry| (*entry).to_string()).collect(),
        aliases: BTreeMap::new(),
        namespace: None,
    }
}

/// Compile `agent` (installed as its own bundle) and return the view.
fn compile_bundle_agent(
    tools: ToolRegistry,
    agent: PreparedAgent,
) -> Result<Arc<CompiledResourceView>, BundleError> {
    let stable_id = agent.id.as_str().to_string();
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            &format!("hya/coord-{stable_id}"),
            agent,
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(tools, catalog);
    let binding = registry
        .bind_turn(&PathBuf::from("/tmp/hya-coordination-tools"))
        .unwrap();
    let policy = binding.agent_resource_policy(&stable_id)?;
    binding.compile_agent_resources(&policy)
}

fn schema_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
    compiled
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

#[test]
fn coordination_tools_join_a_narrow_bundle_view() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("narrow-scout", view(&["harness:tool/grep"], &[])),
    )
    .unwrap();
    let names = schema_names(&compiled);
    for expected in ["grep", "report", "wait", "send", "list_channel", "read"] {
        assert!(names.contains(expected), "missing `{expected}`: {names:?}");
    }
    for absent in ["task", "archive", "bash", "glob", "edit", "write"] {
        assert!(!names.contains(absent), "`{absent}` leaked: {names:?}");
    }
}

/// The core-agents preset's `resource_view` narrows built-ins too: the
/// read-only planner and scout never see the write or shell tools, and the
/// scout cannot spawn; the default agent keeps the full set.
#[test]
fn read_only_builtins_are_denied_write_and_shell_tools() {
    let registry = test_runtime_registry(
        ToolRegistry::builtins(),
        Arc::new(TestCatalog::from_prepared(&[]).unwrap()),
    );
    let binding = registry
        .bind_turn(&PathBuf::from("/tmp/hya-read-only-builtins"))
        .unwrap();
    let names = |stable_id: &str| {
        let policy = binding.agent_resource_policy(stable_id).unwrap();
        schema_names(&binding.compile_agent_resources(&policy).unwrap())
    };
    let writes = ["write", "edit", "apply_patch", "bash"];

    let plan = names("hya-plan");
    for denied in writes {
        assert!(!plan.contains(denied), "hya-plan sees `{denied}`: {plan:?}");
    }
    assert!(plan.contains("grep") && plan.contains("task"), "{plan:?}");

    let scout = names("hya-scout");
    for denied in writes.into_iter().chain(["task", "archive"]) {
        assert!(
            !scout.contains(denied),
            "hya-scout sees `{denied}`: {scout:?}"
        );
    }
    assert!(
        scout.contains("grep") && scout.contains("read"),
        "{scout:?}"
    );

    let main = names("hya-main");
    for kept in writes {
        assert!(main.contains(kept), "hya-main lacks `{kept}`: {main:?}");
    }
}

#[test]
fn injected_read_serves_channel_mail_but_never_files() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("mail-reader", view(&["harness:tool/grep"], &[])),
    )
    .unwrap();
    let schema = compiled
        .tool_schemas()
        .into_iter()
        .find(|schema| schema.name.as_str() == "read")
        .expect("channel read is advertised");
    assert!(
        schema.description.contains("channel://"),
        "{}",
        schema.description
    );
    let read = compiled.resolve_tool("read").unwrap();
    let ctx = dispatch_ctx(Path::new("/tmp/hya-coordination-tools"));
    let error =
        futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "Cargo.toml" })))
            .unwrap_err();
    assert!(
        matches!(&error, ToolError::Input(message) if message.contains("channel://")),
        "a file path must be refused with the channel spelling: {error:?}"
    );
}

#[test]
fn a_view_that_selects_read_keeps_the_full_read_tool() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("file-reader", view(&["harness:tool/read"], &[])),
    )
    .unwrap();
    let read = compiled
        .tool_schemas()
        .into_iter()
        .find(|schema| schema.name.as_str() == "read")
        .unwrap();
    assert!(
        read.description.starts_with("Read a file"),
        "{}",
        read.description
    );
}

#[test]
fn spawn_rights_add_task_and_archive() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        PreparedAgent {
            can_spawn: vec![AgentName::new("hya-scout")],
            ..agent("narrow-lead", view(&["harness:tool/grep"], &[]))
        },
    )
    .unwrap();
    let names = schema_names(&compiled);
    assert!(
        names.contains("task") && names.contains("archive"),
        "{names:?}"
    );
}

#[test]
fn a_default_view_without_spawn_rights_has_no_task_or_archive() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("leaf-worker", ResourceView::default()),
    )
    .unwrap();
    let names = schema_names(&compiled);
    assert!(
        names.contains("bash") && names.contains("wait"),
        "{names:?}"
    );
    assert!(
        !names.contains("task") && !names.contains("archive"),
        "an agent that can spawn nobody must not see task/archive: {names:?}"
    );
}

#[test]
fn without_channel_tools_no_channel_tools_are_injected() {
    let compiled = compile_bundle_agent(
        ToolRegistry::from_tool_families(&WITHOUT_CHANNEL_TOOLS),
        agent("quiet-scout", view(&["harness:tool/grep"], &[])),
    )
    .unwrap();
    let names = schema_names(&compiled);
    assert_eq!(
        names,
        ["grep", "wait"].into_iter().map(str::to_string).collect(),
        "only the extended-tools wait joins when the channel family is absent"
    );
}

#[test]
fn explicit_coordination_entries_dedupe_with_the_injected_set() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
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
    let schemas = compiled.tool_schemas();
    for name in ["report", "wait", "list_channel"] {
        assert_eq!(
            schemas
                .iter()
                .filter(|schema| schema.name.as_str() == name)
                .count(),
            1,
            "`{name}` must be advertised exactly once"
        );
    }
}

#[test]
fn deny_removes_a_safe_coordination_tool() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        PreparedAgent {
            can_spawn: vec![AgentName::new("hya-scout")],
            ..agent(
                "no-task-lead",
                view(&["harness:tool/grep"], &["harness:tool/task", "send"]),
            )
        },
    )
    .unwrap();
    let names = schema_names(&compiled);
    assert!(
        !names.contains("task") && !names.contains("send"),
        "{names:?}"
    );
    assert!(
        names.contains("archive") && names.contains("report"),
        "{names:?}"
    );
}

#[test]
fn deny_of_report_is_rejected() {
    let error = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("stuck-scout", view(&[], &["harness:tool/report"])),
    )
    .err()
    .expect("denying report must be rejected");
    assert!(
        matches!(&error, BundleError::InvalidManifest { detail, .. } if detail.contains("report")),
        "{error:?}"
    );
}

#[test]
fn deny_of_read_keeps_the_channel_mail_path() {
    let compiled = compile_bundle_agent(
        ToolRegistry::builtins(),
        agent("no-files", view(&[], &["harness:tool/read"])),
    )
    .unwrap();
    let read = compiled
        .tool_schemas()
        .into_iter()
        .find(|schema| schema.name.as_str() == "read")
        .expect("channel read survives a read deny");
    assert!(
        read.description.contains("channel://"),
        "{}",
        read.description
    );
}

#[test]
fn builtin_agents_keep_every_coordination_tool() {
    let catalog = Arc::new(TestCatalog::from_prepared(&[]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let binding = registry
        .bind_turn(&PathBuf::from("/tmp/hya-coordination-tools"))
        .unwrap();
    let policy = binding.agent_resource_policy("hya-main").unwrap();
    let compiled = binding.compile_agent_resources(&policy).unwrap();
    let names = schema_names(&compiled);
    for expected in [
        "task",
        "archive",
        "wait",
        "report",
        "send",
        "list_channel",
        "read",
    ] {
        assert!(names.contains(expected), "missing `{expected}`: {names:?}");
    }
}

fn skill_md(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {name}\n---\n{body}\n")
}
