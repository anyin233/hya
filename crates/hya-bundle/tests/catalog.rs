use hya_bundle::{BundleCatalog, BundleSource, ExportKind, SourceFile, prepare_builtins};

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
