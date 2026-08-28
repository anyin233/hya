//! Catalog assembly: bundle sources, duplicate detection, and the spawn graph.

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ExportKind, PreparedCatalog,
    PreparedInstallableBundle, PreparedResource, SourceFile, prepare_package,
};
use sha2::{Digest, Sha256};

fn source(root: &str, bundle_id: &str, stable_agent_id: &str, content: &str) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agent:
  id: {stable_agent_id}
  role: main
  spawn_lifecycle: transient
"#,
    );
    BundleSource::new(
        root,
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("resources/skills/docs.md", content.as_bytes()),
        ],
    )
}

fn installed_duplicate_package_source(root: &str, stable_agent_id: &str) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: hya/duplicate-package
  version: 1.0.0
  publisher: hya
agent:
  id: {stable_agent_id}
  role: main
  spawn_lifecycle: transient
"#,
    );
    BundleSource::new(
        root,
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

fn digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        assert!(write!(encoded, "{byte:02x}").is_ok());
    }
    encoded
}

#[test]
fn empty_prepared_catalog_is_accepted() {
    // Built-in agents no longer live in bundles, so a fresh install has zero
    // installed bundles and must still produce a usable catalog.
    let catalog = BundleCatalog::from_prepared(&[]);
    let Ok(catalog) = catalog else {
        panic!("empty catalog must be valid: {catalog:?}");
    };
    assert!(catalog.bundles().is_empty());
    assert!(catalog.resolve_agent("anything").is_none());
}

#[test]
fn zero_bundle_prepared_document_yields_an_empty_catalog() {
    // Canonical empty prepared document: matching digest, valid shape, zero bundles.
    let bytes = br#"{"format_version":2,"bundles":[],"index":[]}"#;
    let expected_digest = digest(bytes);
    let prepared = PreparedCatalog::decode(bytes, &expected_digest);
    let Ok(prepared) = prepared else {
        panic!("empty prepared document should still decode as prepared data: {prepared:?}");
    };
    assert!(
        prepared.bundles().is_empty(),
        "fixture must remain a zero-bundle prepared document"
    );
    let catalog = BundleCatalog::from_prepared(prepared.bundles());
    let Ok(catalog) = catalog else {
        panic!("zero bundles must be a valid catalog: {catalog:?}");
    };
    assert!(catalog.bundles().is_empty());
}

#[test]
fn only_verified_prepared_catalogs_supply_catalog_semantic_identity() {
    let prepared = prepare_package(source(
        "catalog-semantic-identity",
        "hya/catalog-semantic-identity",
        "catalog-semantic-identity",
        "catalog docs",
    ));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };

    let verified = BundleCatalog::from_verified_catalogs(&[&prepared]);
    let Ok(verified) = verified else {
        panic!("verified catalog construction failed: {verified:?}");
    };
    let verified_again = BundleCatalog::from_verified_catalogs(&[&prepared]);
    let Ok(verified_again) = verified_again else {
        panic!("verified catalog construction failed: {verified_again:?}");
    };
    let unverified = BundleCatalog::from_prepared(prepared.bundles());
    let Ok(unverified) = unverified else {
        panic!("catalog construction failed: {unverified:?}");
    };

    let Some(identity) = verified.semantic_identity_v1() else {
        panic!("verified catalog must expose semantic identity bytes");
    };
    let Some(identity_again) = verified_again.semantic_identity_v1() else {
        panic!("verified catalog must expose semantic identity bytes");
    };
    assert!(!identity.is_empty());
    assert_eq!(identity, identity_again);
    assert_eq!(unverified.semantic_identity_v1(), None);
}

#[test]
fn verified_catalog_merge_matches_flat_verified_construction() {
    let builtins = prepare_package(source(
        "catalog-merge-builtin",
        "hya/catalog-merge-builtin",
        "catalog-merge-builtin",
        "builtin docs",
    ));
    let Ok(builtins) = builtins else {
        panic!("builtin preparation failed: {builtins:?}");
    };
    let installed = prepare_package(source(
        "catalog-merge-installed",
        "hya/catalog-merge-installed",
        "catalog-merge-installed",
        "installed docs",
    ));
    let Ok(installed) = installed else {
        panic!("installed preparation failed: {installed:?}");
    };

    let direct = BundleCatalog::from_verified_catalogs(&[&builtins, &installed]);
    let Ok(direct) = direct else {
        panic!("direct catalog construction failed: {direct:?}");
    };
    let base = BundleCatalog::from_verified_catalogs(&[&builtins]);
    let Ok(base) = base else {
        panic!("base catalog construction failed: {base:?}");
    };
    let merged = base.with_verified_catalogs(&[&installed]);
    let Ok(merged) = merged else {
        panic!("catalog merge failed: {merged:?}");
    };

    assert_eq!(merged.bundles(), direct.bundles());
    let Some(identity) = merged.semantic_identity_v1() else {
        panic!("merged catalog must expose semantic identity bytes");
    };
    assert!(!identity.is_empty());
    assert_eq!(merged.semantic_identity_v1(), direct.semantic_identity_v1());
}

#[test]
fn catalog_rejects_bundle_mcp_even_from_prepared_data() {
    let prepared = prepare_package(source(
        "catalog-mcp",
        "hya/catalog-mcp",
        "catalog-mcp",
        "catalog docs",
    ));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let [PreparedInstallableBundle::Agent(prepared_bundle)] = prepared.bundles() else {
        panic!("fixture must prepare exactly one AgentBundle");
    };
    let mut bundle = prepared_bundle.clone();
    let content = "{}";
    bundle.mcp.push(PreparedResource {
        local_id: "docs".to_string(),
        stable_id: "bundle:hya/catalog-mcp/mcp/docs".to_string(),
        source_path: "resources/mcp/docs.json".to_string(),
        digest: digest(content.as_bytes()),
        content: content.to_string(),
        aliases: Vec::new(),
    });

    assert_eq!(
        BundleCatalog::from_prepared(&[PreparedInstallableBundle::Agent(bundle)]).err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/catalog-mcp".to_string(),
            feature: "resources.mcp".to_string(),
        })
    );
}

#[test]
fn catalog_rejects_unsupported_hook_local_id_from_prepared_data() {
    let prepared = prepare_package(BundleSource::new(
        "catalog-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/catalog-hook
  version: 1.0.0
  publisher: hya
resources:
  hooks:
    - id: event
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: catalog-hook
  role: main
  spawn_lifecycle: transient
  hook_refs:
    - bundle:hya/catalog-hook/hook/event
"#
                .as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {};\n"),
        ],
    ));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let [PreparedInstallableBundle::Agent(prepared_bundle)] = prepared.bundles() else {
        panic!("fixture must prepare exactly one AgentBundle");
    };
    let mut bundle = prepared_bundle.clone();
    bundle.hooks[0].local_id = "audit".to_string();
    bundle.hooks[0].stable_id = "bundle:hya/catalog-hook/hook/audit".to_string();

    assert_eq!(
        BundleCatalog::from_prepared(&[PreparedInstallableBundle::Agent(bundle)]).err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/catalog-hook".to_string(),
            feature: "hook:audit".to_string(),
        })
    );
}

#[test]
fn catalog_rejects_duplicate_bundle_identity_with_disjoint_exports() {
    let alpha = prepare_package(installed_duplicate_package_source(
        "duplicate-package-alpha",
        "duplicate-alpha",
    ));
    let Ok(alpha) = alpha else {
        panic!("alpha package preparation failed: {alpha:?}");
    };
    let beta = prepare_package(installed_duplicate_package_source(
        "duplicate-package-beta",
        "duplicate-beta",
    ));
    let Ok(beta) = beta else {
        panic!("beta package preparation failed: {beta:?}");
    };
    let [alpha] = alpha.bundles() else {
        panic!("alpha package must prepare exactly one bundle");
    };
    let [beta] = beta.bundles() else {
        panic!("beta package must prepare exactly one bundle");
    };
    let bundles = vec![alpha.clone(), beta.clone()];

    assert!(matches!(
        BundleCatalog::from_prepared(&bundles),
        Err(BundleError::DuplicateBundleId { bundle_id }) if bundle_id == "hya/duplicate-package"
    ));
}

#[test]
fn bundle_local_short_name_wins_and_qualified_name_is_exact() {
    let alpha = prepare_package(source("alpha", "hya/alpha", "alpha", "alpha docs"));
    let Ok(alpha) = alpha else {
        panic!("alpha preparation failed: {alpha:?}");
    };
    let beta = prepare_package(source("beta", "hya/beta", "beta", "beta docs"));
    let Ok(beta) = beta else {
        panic!("beta preparation failed: {beta:?}");
    };
    let bundles = [alpha.bundles(), beta.bundles()].concat();
    let catalog = BundleCatalog::from_prepared(&bundles);
    let Ok(catalog) = catalog else {
        panic!("catalog construction failed: {catalog:?}");
    };

    let local = catalog.resolve_resource("hya/alpha", ExportKind::Skill, "docs");
    let Ok(local) = local else {
        panic!("local resolution failed: {local:?}");
    };
    assert_eq!(local.stable_id, "bundle:hya/alpha/skill/docs");
    assert_eq!(local.content, "alpha docs");

    let qualified =
        catalog.resolve_resource("hya/alpha", ExportKind::Skill, "bundle:hya/beta/skill/docs");
    let Ok(qualified) = qualified else {
        panic!("qualified resolution failed: {qualified:?}");
    };
    assert_eq!(qualified.stable_id, "bundle:hya/beta/skill/docs");
    assert_eq!(qualified.content, "beta docs");
}

#[test]
fn bundle_id_with_kind_segments_resolves_structurally() {
    let bundle_id = "hya/tool/skill/mcp-nest";
    let source = BundleSource::new(
        "nested-kinds",
        vec![
            SourceFile::new(
                "bundle.yaml",
                format!(
                    r#"kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agent:
  id: nested-lead
  role: main
  spawn_lifecycle: transient
"#
                )
                .into_bytes(),
            ),
            SourceFile::new("resources/skills/docs.md", b"nested docs"),
        ],
    );
    let Ok(prepared) = prepare_package(source) else {
        panic!("prepare nested bundle id");
    };
    let Ok(catalog) = BundleCatalog::from_prepared(prepared.bundles()) else {
        panic!("catalog");
    };
    let Ok(local) = catalog.resolve_resource(bundle_id, ExportKind::Skill, "docs") else {
        panic!("local short");
    };
    assert_eq!(local.stable_id, format!("bundle:{bundle_id}/skill/docs"));
    let Ok(qualified) = catalog.resolve_resource(
        bundle_id,
        ExportKind::Skill,
        &format!("bundle:{bundle_id}/skill/docs"),
    ) else {
        panic!("qualified must parse kind from rightmost segments");
    };
    assert_eq!(qualified.content, "nested docs");
}

fn workflow_source(bundle_id: &str, workflow_id: &str, agent_id: &str) -> BundleSource {
    let manifest = format!(
        r#"kind: WorkflowBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
workflow:
  id: {workflow_id}
  path: workflows/{workflow_id}.hya.md
agents:
  - id: {agent_id}
    role: subagent
    prompt: prompts/{agent_id}.md
    spawn_lifecycle: transient
"#
    );
    let workflow = format!(
        r#"---
kind: Workflow
name: {workflow_id}
description: Catalog Workflow.
nodes:
  run:
    agent: {agent_id}
    directive: Run the stage.
---
flowchart TD
  run
"#
    );
    BundleSource::new(
        bundle_id,
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new(
                format!("workflows/{workflow_id}.hya.md"),
                workflow.into_bytes(),
            ),
            SourceFile::new(
                format!("prompts/{agent_id}.md"),
                b"Run the stage.\n".as_slice(),
            ),
        ],
    )
}

#[test]
fn workflow_catalog_indexes_agents_and_qualified_workflow() {
    let prepared = prepare_package(workflow_source("hya/workflow-catalog", "demo", "worker"));
    let Ok(prepared) = prepared else {
        panic!("WorkflowBundle preparation failed: {prepared:?}");
    };
    let catalog = BundleCatalog::from_prepared(prepared.bundles());
    let Ok(catalog) = catalog else {
        panic!("WorkflowBundle catalog construction failed: {catalog:?}");
    };
    let Some((owner, agent)) = catalog.resolve_agent_entry("worker") else {
        panic!("Workflow Agent was not indexed");
    };
    assert_eq!(owner, "hya/workflow-catalog");
    assert_eq!(agent.id.as_str(), "worker");
    let Some((owner, workflow)) =
        catalog.resolve_workflow_entry("bundle:hya/workflow-catalog/workflow/demo")
    else {
        panic!("qualified Workflow was not indexed");
    };
    assert_eq!(owner, "hya/workflow-catalog");
    assert_eq!(workflow.id, "demo");
    assert_eq!(catalog.resolve_workflow("demo"), Some(workflow));
}

#[test]
fn workflow_catalog_rejects_ambiguous_bare_names_but_keeps_qualified_ids() {
    let first = prepare_package(workflow_source("hya/workflow-one", "demo", "worker-one"));
    let Ok(first) = first else {
        panic!("first WorkflowBundle preparation failed: {first:?}");
    };
    let second = prepare_package(workflow_source("hya/workflow-two", "demo", "worker-two"));
    let Ok(second) = second else {
        panic!("second WorkflowBundle preparation failed: {second:?}");
    };
    let bundles = [first.bundles(), second.bundles()].concat();
    let catalog = BundleCatalog::from_prepared(&bundles);
    let Ok(catalog) = catalog else {
        panic!("WorkflowBundle catalog construction failed: {catalog:?}");
    };
    assert!(catalog.resolve_workflow("demo").is_none());
    assert!(
        catalog
            .resolve_workflow("bundle:hya/workflow-one/workflow/demo")
            .is_some()
    );
    assert!(catalog.resolve_agent("worker-one").is_some());
    assert!(catalog.resolve_agent("worker-two").is_some());
}
