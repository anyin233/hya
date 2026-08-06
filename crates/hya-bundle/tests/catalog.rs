//! Catalog assembly: bundle sources, duplicate detection, and the spawn graph.

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ExportKind, PreparedCatalog, PreparedResource,
    SourceFile, prepare_builtins, prepare_package,
};
use sha2::{Digest, Sha256};

fn source(root: &str, bundle_id: &str, stable_agent_id: &str, content: &str) -> BundleSource {
    let manifest = format!(
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agents:
  - local_id: lead
    stable_id: {stable_agent_id}
    role: main
    spawn_lifecycle: transient
    harness_access: full
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

fn installed_duplicate_package_source(
    root: &str,
    local_agent_id: &str,
    stable_agent_id: &str,
) -> BundleSource {
    let manifest = format!(
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/duplicate-package
  version: 1.0.0
  publisher: hya
agents:
  - local_id: {local_agent_id}
    stable_id: {stable_agent_id}
    role: main
    spawn_lifecycle: transient
    harness_access: full
"#,
    );
    BundleSource::new(
        root,
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

fn spawn_graph_source() -> BundleSource {
    BundleSource::new(
        "spawn-graph",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/spawn-graph
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    can_spawn: [worker]
  - local_id: worker
    stable_id: worker
    role: subagent
    spawn_lifecycle: transient
    harness_access: full
  - local_id: compaction
    stable_id: compaction
    role: subagent
    spawn_lifecycle: transient
    harness_access: full
"#,
        )],
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
fn empty_prepared_catalog_is_rejected() {
    assert_eq!(
        BundleCatalog::from_prepared(&[]).err(),
        Some(BundleError::EmptyPreparedCatalog)
    );
}

#[test]
fn zero_bundle_prepared_document_never_yields_empty_catalog() {
    // Canonical empty prepared document: matching digest, valid shape, zero bundles.
    let bytes = br#"{"format_version":1,"bundles":[],"index":[]}"#;
    let expected_digest = digest(bytes);
    let prepared = PreparedCatalog::decode(bytes, &expected_digest);
    let Ok(prepared) = prepared else {
        panic!("empty prepared document should still decode as prepared data: {prepared:?}");
    };
    assert!(
        prepared.bundles().is_empty(),
        "fixture must remain a zero-bundle prepared document"
    );
    assert_eq!(
        BundleCatalog::from_prepared(prepared.bundles()).err(),
        Some(BundleError::EmptyPreparedCatalog),
        "zero bundles must never become an empty BundleCatalog"
    );
}

#[test]
fn only_verified_prepared_catalogs_supply_catalog_semantic_identity() {
    let prepared = prepare_builtins(vec![source(
        "catalog-semantic-identity",
        "hya/catalog-semantic-identity",
        "catalog-semantic-identity",
        "catalog docs",
    )]);
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
    let builtins = prepare_builtins(vec![source(
        "catalog-merge-builtin",
        "hya/catalog-merge-builtin",
        "catalog-merge-builtin",
        "builtin docs",
    )]);
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
    let prepared = prepare_builtins(vec![source(
        "catalog-mcp",
        "hya/catalog-mcp",
        "catalog-mcp",
        "catalog docs",
    )]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let [prepared_bundle] = prepared.bundles() else {
        panic!("fixture must prepare exactly one bundle");
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
        BundleCatalog::from_prepared(&[bundle]).err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/catalog-mcp".to_string(),
            feature: "resources.mcp".to_string(),
        })
    );
}

#[test]
fn catalog_rejects_unsupported_hook_local_id_from_prepared_data() {
    let prepared = prepare_builtins(vec![BundleSource::new(
        "catalog-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
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
agents:
  - local_id: lead
    stable_id: catalog-hook
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - bundle:hya/catalog-hook/hook/event
"#
                .as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {};\n"),
        ],
    )]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let [prepared_bundle] = prepared.bundles() else {
        panic!("fixture must prepare exactly one bundle");
    };
    let mut bundle = prepared_bundle.clone();
    bundle.hooks[0].local_id = "audit".to_string();
    bundle.hooks[0].stable_id = "bundle:hya/catalog-hook/hook/audit".to_string();

    assert_eq!(
        BundleCatalog::from_prepared(&[bundle]).err(),
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
        "alpha",
        "duplicate-alpha",
    ));
    let Ok(alpha) = alpha else {
        panic!("alpha package preparation failed: {alpha:?}");
    };
    let beta = prepare_package(installed_duplicate_package_source(
        "duplicate-package-beta",
        "beta",
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
    let prepared = prepare_builtins(vec![
        source("alpha", "hya/alpha", "alpha", "alpha docs"),
        source("beta", "hya/beta", "beta", "beta docs"),
    ]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let catalog = BundleCatalog::from_prepared(prepared.bundles());
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
fn reserved_system_agent_is_not_an_ordinary_spawn_target() {
    let prepared = prepare_builtins(vec![spawn_graph_source()]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let catalog = BundleCatalog::from_prepared(prepared.bundles());
    let Ok(catalog) = catalog else {
        panic!("catalog construction failed: {catalog:?}");
    };

    assert!(catalog.resolve_spawn("lead", "worker").is_ok());
    assert!(matches!(
        catalog.resolve_spawn("lead", "compaction"),
        Err(hya_bundle::BundleError::AgentSpawnNotAllowed { .. })
    ));
    assert!(
        catalog.resolve_agent("compaction").is_some(),
        "the fixed Harness system lookup must remain exact and available"
    );
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
                    r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agents:
  - local_id: lead
    stable_id: nested-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
"#
                )
                .into_bytes(),
            ),
            SourceFile::new("resources/skills/docs.md", b"nested docs"),
        ],
    );
    let Ok(prepared) = prepare_builtins(vec![source]) else {
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
