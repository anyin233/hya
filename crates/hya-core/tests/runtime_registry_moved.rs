//! Integration tests for runtime source publication and naming.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleError, BundleIdentity, BundleSource, ModelPolicy,
    PreparedAgent, PreparedResource, ResourceView, SourceFile, prepare_package,
};
use hya_core::agent_catalog::AgentCatalog;
use hya_core::runtime_registry::*;
use hya_proto::{AgentName, ToolName, ToolSchema};
use hya_tool::{SkillCatalogEntry, Tool, ToolCtx, ToolError, ToolPermission, ToolRegistry};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[path = "runtime_registry_moved/support.rs"]
mod support;
use support::*;

#[test]
fn source_name_registry_rejects_bare_plugin_tool_names() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/name-registry",
            agent("name-registry", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let error = registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::plugin("bare-tools"),
                [9; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "read",
                    "read",
                    Vec::new(),
                    Arc::new(NoopTool::new("read")),
                    ToolPermission::Tool,
                )],
            )])
        })
        .expect_err("bare plugin tool names must be rejected");
    let RuntimeRefreshError::NamingConflicts(report) = error else {
        panic!("expected a naming conflict report: {error:?}");
    };
    assert!(
        report.contains("plugin:bare-tools") && report.contains("`read`"),
        "report must name the source and the bare name: {report}"
    );
}

#[test]
fn source_name_registry_groups_cross_source_duplicates() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/name-registry",
            agent("name-registry", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let error = registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::plugin("dup-a"),
                    [10; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "dup__tool",
                        "dup__tool",
                        Vec::new(),
                        Arc::new(NoopTool::new("dup__tool")),
                        ToolPermission::Tool,
                    )],
                ),
                RuntimeSource::new(
                    RuntimeSourceId::plugin("dup-b"),
                    [11; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "dup__tool",
                        "dup__tool",
                        Vec::new(),
                        Arc::new(NoopTool::new("dup__tool")),
                        ToolPermission::Tool,
                    )],
                ),
            ])
        })
        .expect_err("cross-source duplicates must be rejected");
    let RuntimeRefreshError::NamingConflicts(report) = error else {
        panic!("expected a naming conflict report: {error:?}");
    };
    assert!(
        report.contains("plugin:dup-a") && report.contains("plugin:dup-b"),
        "report must list every conflicting source: {report}"
    );
}

#[test]
fn protected_read_alias_is_a_hard_error() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/name-registry",
            agent("name-registry", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let error = registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::plugin("read-masker"),
                [15; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "reader",
                    "read_masker__reader",
                    vec!["read".to_string()],
                    Arc::new(NoopTool::new("read_masker__reader")),
                    ToolPermission::Tool,
                )],
            )])
        })
        .expect_err("no source may mask the protected `read` tool");
    let RuntimeRefreshError::NamingConflicts(report) = error else {
        panic!("expected a naming conflict report: {error:?}");
    };
    assert!(
        report.contains("protected tool `read` cannot be masked"),
        "report must name the read protection: {report}"
    );
}

#[test]
fn masks_accessor_reports_only_contested_bare_names() {
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
                RuntimeSourceId::plugin("acc-src"),
                [18; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "acc",
                    "acc__tool",
                    vec!["solo".to_string(), "grep".to_string()],
                    Arc::new(NoopTool::new("acc__tool")),
                    ToolPermission::Tool,
                )],
            )])
        })
        .expect("mixed contested and uncontested aliases must publish");

    let workdir = PathBuf::from("/tmp/hya-mask-table");
    let binding = registry.bind_turn(&workdir).unwrap();
    let mut expected = BTreeMap::new();
    expected.insert("grep".to_string(), "acc__tool".to_string());
    assert_eq!(
        binding.masks(),
        &expected,
        "only the built-in contest belongs in the mask table"
    );
    assert!(
        binding.resolve_tool("solo").is_some(),
        "an uncontested alias keeps working through the registry"
    );
}

/// Installed Plugin-kind bundle hooks reach bundle agents too: a bundle
/// agent's chain is every Plugin's hooks (source-id order, the very same
/// retained dispatchers built-in agents get) followed by its own bundle's
/// `hook_refs`-scoped hooks. An agent without `hook_refs` still gets the
/// Plugin hooks.
#[test]
fn bundle_agents_receive_installed_plugin_hooks_before_their_own() {
    let plugin_b = prepare_package(BundleSource::new(
        "plugin-b",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nidentity: { id: acme/plugin-b, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let plugin_a = prepare_package(BundleSource::new(
        "plugin-a",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nidentity: { id: acme/plugin-a, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let agents = prepare_package(BundleSource::new(
        "owner",
        vec![
            SourceFile::new(
                "bundle.yaml",
                r#"kind: Bundle
identity: { id: acme/owner, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [fixture] }
resources:
  hooks: [{ id: chat.params, path: hook.json }]
agents:
  - id: hooked-agent
    role: main
    hook_refs: [chat.params]
  - id: plain-agent
    role: main
"#,
            ),
            SourceFile::new("hook.json", "{}"),
        ],
    ))
    .unwrap();
    let catalog =
        Arc::new(TestCatalog::from_verified_catalogs(&[&plugin_b, &agents, &plugin_a]).unwrap());
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let empty = || -> Arc<dyn hya_core::hooks::HookDispatcher> {
        Arc::new(hya_core::hooks::HookChain::new(Vec::new()))
    };
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/plugin-b"),
                    [4; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(empty()),
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/owner"),
                    [5; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(empty()),
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/plugin-a"),
                    [6; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(empty()),
            ])
        })
        .unwrap();
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-plugin-hooks-bundle-agents"))
        .unwrap();

    let plugin_a_hooks = binding.bundle_hooks("acme/plugin-a").unwrap();
    let plugin_b_hooks = binding.bundle_hooks("acme/plugin-b").unwrap();
    let owner_hooks = binding.bundle_hooks("acme/owner").unwrap();

    let builtin = binding.bundle_hooks_for_agent("hya-main");
    assert_eq!(builtin.len(), 2, "built-ins keep every Plugin's hooks");
    assert!(Arc::ptr_eq(&builtin[0], &plugin_a_hooks));
    assert!(Arc::ptr_eq(&builtin[1], &plugin_b_hooks));

    let hooked = binding.bundle_hooks_for_agent("hooked-agent");
    assert_eq!(
        hooked.len(),
        3,
        "Plugin hooks, then the owner's scoped hooks"
    );
    assert!(Arc::ptr_eq(&hooked[0], &plugin_a_hooks));
    assert!(Arc::ptr_eq(&hooked[1], &plugin_b_hooks));
    assert!(
        !Arc::ptr_eq(&hooked[2], &owner_hooks),
        "the owner's hooks stay filtered by hook_refs"
    );

    let plain = binding.bundle_hooks_for_agent("plain-agent");
    assert_eq!(plain.len(), 2, "no hook_refs still gets the Plugin hooks");
    assert!(Arc::ptr_eq(&plain[0], &plugin_a_hooks));
    assert!(Arc::ptr_eq(&plain[1], &plugin_b_hooks));
    assert!(binding.bundle_hooks_for_agent("missing-agent").is_empty());
}

#[test]
fn allow_and_deny_reject_alias_spellings() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[
            bundle_with_agent(
                "hya/allow-alias",
                agent(
                    "allow-alias",
                    ResourceView {
                        allow: vec!["patch".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                Vec::new(),
            ),
            bundle_with_agent(
                "hya/deny-alias",
                agent(
                    "deny-alias",
                    ResourceView {
                        allow: Vec::new(),
                        deny: vec!["patch".to_string()],
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
    let binding = registry
        .bind_turn(Path::new("/tmp/hya-filter-alias"))
        .unwrap();
    for agent_id in ["allow-alias", "deny-alias"] {
        match binding.agent_resource_policy(agent_id) {
            Err(BundleError::UnknownResourceReference { reference, .. }) => {
                assert_eq!(reference, "patch");
            }
            Ok(_) => panic!("{agent_id}: alias spelling in allow/deny must typed-reject"),
            Err(other) => panic!("{agent_id}: expected unknown reference, got {other:?}"),
        }
    }
}

#[test]
fn two_sources_claiming_one_scheme_mask_by_greater_source_id() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/scheme-mask",
            agent("scheme-mask-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let exports = |name: &str| {
        vec![RuntimeSourceExport::tool(
            name,
            name,
            Vec::new(),
            Arc::new(NoopTool::new(name)),
            ToolPermission::ReadOnly,
        )]
    };
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                scheme_source(
                    "aaa-src",
                    exports("aaa__query"),
                    vec![SourceSchema {
                        scheme: "db".to_string(),
                        canonical_tool: "aaa__query".to_string(),
                        writable: false,
                    }],
                ),
                scheme_source(
                    "zzz-src",
                    exports("zzz__query"),
                    vec![SourceSchema {
                        scheme: "db".to_string(),
                        canonical_tool: "zzz__query".to_string(),
                        writable: true,
                    }],
                ),
            ])
        })
        .expect("two sources may contest one scheme");

    let binding = registry
        .bind_turn(&PathBuf::from("/tmp/hya-scheme-mask"))
        .unwrap();
    let winner = binding.schemes().get("db").expect("scheme must resolve");
    assert_eq!(winner.owner(), "plugin:zzz-src");
    assert_eq!(winner.canonical_tool(), "zzz__query");
    assert!(winner.writable());
    assert_eq!(
        binding.scheme_chain("db"),
        vec![
            ("plugin:aaa-src".to_string(), "aaa__query".to_string()),
            ("plugin:zzz-src".to_string(), "zzz__query".to_string()),
        ],
        "the chain lists every claimant in ascending source id order"
    );
}

#[test]
fn foreign_tool_and_protected_scheme_claims_are_one_grouped_error() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/scheme-bad",
            agent("scheme-bad-agent", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
    let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
    let error = registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                scheme_source(
                    "bad-a",
                    vec![RuntimeSourceExport::tool(
                        "bad-a__query",
                        "bad-a__query",
                        Vec::new(),
                        Arc::new(NoopTool::new("bad-a__query")),
                        ToolPermission::ReadOnly,
                    )],
                    vec![SourceSchema {
                        scheme: "db".to_string(),
                        canonical_tool: "not-exported__tool".to_string(),
                        writable: false,
                    }],
                ),
                scheme_source(
                    "bad-b",
                    Vec::new(),
                    vec![
                        SourceSchema {
                            scheme: "local".to_string(),
                            canonical_tool: "bad-b__put".to_string(),
                            writable: true,
                        },
                        SourceSchema {
                            scheme: "d".to_string(),
                            canonical_tool: "bad-b__put".to_string(),
                            writable: true,
                        },
                    ],
                ),
            ])
        })
        .expect_err("invalid scheme claims must be rejected");
    let RuntimeRefreshError::SchemaConflicts(report) = error else {
        panic!("expected a grouped schema conflict report");
    };
    for fragment in [
        "plugin:bad-a",
        "not-exported__tool",
        "plugin:bad-b",
        "internal scheme `local`",
        "scheme `d`",
    ] {
        assert!(
            report.contains(fragment),
            "grouped report must contain {fragment:?}:\n{report}"
        );
    }
}

/// A Bundle source exports no registry tools, so its schema claim names the
/// owning tool by its view-scoped stable id; publication must still admit it.
#[test]
fn bundle_source_schema_claim_publishes_without_registry_exports() {
    let catalog = Arc::new(
        TestCatalog::from_prepared(&[bundle_with_agent(
            "hya/vecdb",
            agent("vecdb-lead", ResourceView::default()),
            Vec::new(),
        )])
        .unwrap(),
    );
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
        .expect("a bundle schema claim on its own stable-id tool must publish");

    let binding = registry
        .bind_turn(&PathBuf::from("/tmp/hya-bundle-scheme"))
        .unwrap();
    let winner = binding.schemes().get("db").expect("scheme must resolve");
    assert_eq!(winner.owner(), "bundle:hya/vecdb");
    assert_eq!(winner.canonical_tool(), "bundle:hya/vecdb/tool/query");
    assert_eq!(
        binding.scheme_chain("db"),
        vec![(
            "bundle:hya/vecdb".to_string(),
            "bundle:hya/vecdb/tool/query".to_string(),
        )],
        "the chain lists the bundle claimant with its stable-id tool"
    );
}
