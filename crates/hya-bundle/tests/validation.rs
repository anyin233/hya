use hya_bundle::{BundleError, BundleSource, SourceFile, prepare_builtins};

fn agent_source(
    root: &str,
    bundle_id: &str,
    local_id: &str,
    stable_id: &str,
    can_spawn: &[&str],
) -> BundleSource {
    let can_spawn = if can_spawn.is_empty() {
        "      []".to_string()
    } else {
        can_spawn
            .iter()
            .map(|target| format!("      - {target}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let manifest = format!(
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
agents:
  - local_id: {local_id}
    stable_id: {stable_id}
    role: main
    spawn_lifecycle: transient
    harness_access: full
    can_spawn:
{can_spawn}
"#,
    );
    BundleSource::new(
        root,
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

#[test]
fn resource_view_alias_cannot_occupy_a_bundle_local_short_name() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/test
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: local-docs
      path: resources/skills/local-docs.md
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      aliases:
        local-docs: bundle:hya/test/skill/local-docs
"#;
    let result = prepare_builtins(vec![BundleSource::new(
        "alias-conflict",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "resources/skills/local-docs.md",
                b"# Local docs\n".as_slice(),
            ),
        ],
    )]);

    let Err(error) = result else {
        panic!("alias conflict was accepted");
    };
    assert_eq!(
        error,
        BundleError::AliasCollision {
            bundle_id: "hya/test".to_string(),
            name: "local-docs".to_string(),
        }
    );
}

#[test]
fn can_spawn_references_resolve_after_the_full_catalog_is_known() {
    let valid = prepare_builtins(vec![
        agent_source(
            "alpha",
            "hya/alpha",
            "lead",
            "alpha-lead",
            &["bundle:hya/beta/agent/worker"],
        ),
        agent_source("beta", "hya/beta", "worker", "beta-worker", &[]),
    ]);
    let Ok(valid) = valid else {
        panic!("cross-bundle reference failed: {valid:?}");
    };
    let lead = &valid.bundles()[0].agents[0];
    assert_eq!(
        lead.can_spawn
            .iter()
            .map(|agent| agent.as_str())
            .collect::<Vec<_>>(),
        ["beta-worker"]
    );

    let invalid = prepare_builtins(vec![agent_source(
        "alpha",
        "hya/alpha",
        "lead",
        "alpha-lead",
        &["missing-agent"],
    )]);
    let Err(error) = invalid else {
        panic!("missing can_spawn target was accepted");
    };
    assert_eq!(
        error,
        BundleError::UnknownAgentReference {
            bundle_id: "hya/alpha".to_string(),
            agent_id: "alpha-lead".to_string(),
            reference: "missing-agent".to_string(),
        }
    );
}

#[test]
fn stable_agent_id_cannot_shadow_a_qualified_catalog_id() {
    let result = prepare_builtins(vec![
        agent_source(
            "alpha",
            "hya/alpha",
            "intruder",
            "bundle:hya/beta/agent/worker",
            &[],
        ),
        agent_source("beta", "hya/beta", "worker", "beta-worker", &[]),
    ]);
    assert_eq!(
        result.err(),
        Some(BundleError::NamespaceCollision {
            bundle_id: "hya/beta".to_string(),
            name: "bundle:hya/beta/agent/worker".to_string(),
        })
    );
}

#[test]
fn unsupported_resource_profile_is_rejected_as_a_feature_not_ignored() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/profile
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    resource_profile:
      max_depth: 2
      per_team_turn_budget: 8
    harness_access: full
"#;
    let result = prepare_builtins(vec![BundleSource::new(
        "resource-profile",
        vec![SourceFile::new("bundle.yaml", manifest.as_slice())],
    )]);
    let Err(error) = result else {
        panic!("unsupported resource profile was accepted");
    };
    assert_eq!(
        error,
        BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/profile".to_string(),
            feature: "agents[].resource_profile".to_string(),
        }
    );
}

#[test]
fn resource_view_targets_validate_against_the_complete_catalog() {
    let alpha = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/alpha
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: alpha-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      aliases:
        beta-docs: bundle:hya/beta/skill/docs
"#;
    let beta = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/beta
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agents:
  - local_id: worker
    stable_id: beta-worker
    role: subagent
    spawn_lifecycle: transient
    harness_access: full
"#;
    let valid = prepare_builtins(vec![
        BundleSource::new(
            "alpha",
            vec![SourceFile::new("bundle.yaml", alpha.as_slice())],
        ),
        BundleSource::new(
            "beta",
            vec![
                SourceFile::new("bundle.yaml", beta.as_slice()),
                SourceFile::new("resources/skills/docs.md", b"# Docs\n".as_slice()),
            ],
        ),
    ]);
    assert!(valid.is_ok(), "valid cross-bundle alias failed: {valid:?}");

    let invalid_manifest = std::str::from_utf8(alpha)
        .map(|source| source.replace("hya/beta/skill/docs", "hya/missing/skill/docs"));
    let Ok(invalid_manifest) = invalid_manifest else {
        panic!("fixture is not UTF-8");
    };
    let invalid = prepare_builtins(vec![BundleSource::new(
        "alpha",
        vec![SourceFile::new(
            "bundle.yaml",
            invalid_manifest.into_bytes(),
        )],
    )]);
    let Err(error) = invalid else {
        panic!("missing cross-bundle resource was accepted");
    };
    assert_eq!(
        error,
        BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".to_string(),
            kind: "resource".to_string(),
            reference: "bundle:hya/missing/skill/docs".to_string(),
        }
    );
}

#[test]
fn resolved_resource_view_references_are_canonicalized_after_lookup() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/canonical-view
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: resources/skills/docs.md
agents:
  - local_id: lead
    stable_id: canonical-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - docs
        - bundle:hya/canonical-view/skill/docs
      deny:
        - bundle:hya/canonical-view/skill/docs
        - docs
"#;
    let prepared = prepare_builtins(vec![BundleSource::new(
        "canonical-view",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("resources/skills/docs.md", b"# Docs\n".as_slice()),
        ],
    )]);
    let Ok(prepared) = prepared else {
        panic!("valid resource view failed: {prepared:?}");
    };

    let view = &prepared.bundles()[0].agents[0].resource_view;
    assert_eq!(view.allow, ["bundle:hya/canonical-view/skill/docs"]);
    assert_eq!(view.deny, ["bundle:hya/canonical-view/skill/docs"]);
}

fn minimal_manifest(extra: &str) -> Vec<u8> {
    format!(
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/minimal
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: minimal-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
{extra}"#,
    )
    .into_bytes()
}

#[test]
fn invalid_schema_references_and_executable_features_fail_typed() {
    for unsupported_field in [
        "    hidden: true\n",
        "    temperature: 0.2\n",
        "    top_p: 0.8\n",
        "    steps: 3\n",
        "    options: {}\n",
        "    request: {}\n",
        "    permission_overlay: {}\n",
        "    permissions: []\n",
        "    permission: {}\n",
        "    tools: {}\n",
        "    readonly: true\n",
    ] {
        let unknown = prepare_builtins(vec![BundleSource::new(
            "unknown-field",
            vec![SourceFile::new(
                "bundle.yaml",
                minimal_manifest(unsupported_field),
            )],
        )]);
        assert!(
            matches!(unknown, Err(BundleError::InvalidManifest { .. })),
            "field was silently accepted: {unsupported_field}"
        );
    }

    let missing = prepare_builtins(vec![BundleSource::new(
        "missing-prompt",
        vec![SourceFile::new(
            "bundle.yaml",
            minimal_manifest("    prompt: prompts/missing.md\n"),
        )],
    )]);
    assert_eq!(
        missing.err(),
        Some(BundleError::MissingReference {
            bundle_id: "hya/minimal".to_string(),
            path: "prompts/missing.md".to_string(),
        })
    );

    let base_manifest = String::from_utf8(minimal_manifest(""));
    let Ok(base_manifest) = base_manifest else {
        panic!("fixture is not UTF-8");
    };
    for (declaration, feature) in [
        (
            "resources:\n  tools:\n    - id: shell\n      path: extensions/shell.js\n",
            "resources.tools",
        ),
        (
            "resources:\n  mcp:\n    - id: docs\n      path: resources/mcp/docs.json\n",
            "resources.mcp",
        ),
        (
            "resources:\n  hooks:\n    - id: audit\n      path: resources/hooks/audit.json\n",
            "resources.hooks",
        ),
        (
            "extensions:\n  js:\n    - id: runtime\n      path: extensions/runtime.js\n",
            "extensions.js",
        ),
        (
            "extensions:\n  rust:\n    - id: runtime\n      path: extensions/runtime\n",
            "extensions.rust",
        ),
    ] {
        let manifest = base_manifest.replace("agents:\n", &format!("{declaration}agents:\n"));
        let result = prepare_builtins(vec![BundleSource::new(
            feature,
            vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
        )]);
        assert_eq!(
            result.err(),
            Some(BundleError::UnsupportedBundleFeature {
                bundle_id: "hya/minimal".to_string(),
                feature: feature.to_string(),
            })
        );
    }
}

#[test]
fn duplicate_stable_ids_wrong_kind_and_parent_paths_are_rejected() {
    let duplicate = prepare_builtins(vec![
        agent_source("a", "hya/a", "lead", "same", &[]),
        agent_source("b", "hya/b", "lead", "same", &[]),
    ]);
    assert_eq!(
        duplicate.err(),
        Some(BundleError::DuplicateStableAgentId {
            stable_id: "same".to_string(),
        })
    );

    let wrong_kind = String::from_utf8(minimal_manifest(""))
        .map(|manifest| manifest.replace("kind: AgentBundle", "kind: Plugin"));
    let Ok(wrong_kind) = wrong_kind else {
        panic!("fixture is not UTF-8");
    };
    let wrong_kind = prepare_builtins(vec![BundleSource::new(
        "wrong-kind",
        vec![SourceFile::new("bundle.yaml", wrong_kind.into_bytes())],
    )]);
    assert!(matches!(wrong_kind, Err(BundleError::WrongKind { .. })));

    let parent = prepare_builtins(vec![BundleSource::new(
        "parent-path",
        vec![SourceFile::new("../bundle.yaml", minimal_manifest(""))],
    )]);
    assert!(matches!(parent, Err(BundleError::InvalidSourcePath { .. })));
}
