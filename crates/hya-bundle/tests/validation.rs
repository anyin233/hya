use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ExportKind, PreparedCatalog, SourceFile,
    prepare_builtins, prepare_package,
};

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
            "resources:\n  mcp:\n    - id: docs\n      path: resources/mcp/docs.json\n",
            "resources.mcp",
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
fn helper_dependency_and_import_fields_are_rejected_by_source_schema() {
    for fragment in [
        "resources:\n  files: []\n",
        "resources:\n  helpers: []\n",
        "resources:\n  dependencies: []\n",
        "helpers: []\n",
        "dependencies: []\n",
        "imports: []\n",
    ] {
        let result = prepare_builtins(vec![BundleSource::new(
            "source-schema",
            vec![SourceFile::new("bundle.yaml", minimal_manifest(fragment))],
        )]);
        assert!(
            matches!(result, Err(BundleError::InvalidManifest { .. })),
            "schema fragment was accepted: {fragment:?}"
        );
    }
}

#[test]
fn executable_resource_requires_exact_path_extension_join() {
    let manifest = |tool_path: &str, extension_path: &str| {
        format!(
            r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: {tool_path}
extensions:
  js:
    - id: runtime
      path: {extension_path}
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - echo
"#,
        )
    };
    let content = b"export const runtime = true;\n".to_vec();

    let positive = prepare_package(BundleSource::new(
        "exact-path-extension-join-positive",
        vec![
            SourceFile::new(
                "bundle.yaml",
                manifest("extensions/runtime.js", "extensions/runtime.js").into_bytes(),
            ),
            SourceFile::new("extensions/runtime.js", content.clone()),
        ],
    ));
    let Ok(prepared) = positive else {
        panic!("exact-path resource reuse was rejected: {positive:?}");
    };
    let bundle = &prepared.bundles()[0];
    let tool = &bundle.tools[0];
    let extension = &bundle.extensions[0];
    assert_eq!(tool.source_path, "extensions/runtime.js");
    assert_eq!(extension.source_path, "extensions/runtime.js");
    assert_eq!(tool.content, extension.content);
    assert_eq!(tool.digest, extension.digest);

    let negative = prepare_package(BundleSource::new(
        "exact-path-extension-join-negative",
        vec![
            SourceFile::new(
                "bundle.yaml",
                manifest("tools/echo.js", "extensions/runtime.js").into_bytes(),
            ),
            SourceFile::new("tools/echo.js", content.clone()),
            SourceFile::new("extensions/runtime.js", content),
        ],
    ));
    let Err(BundleError::UnsupportedBundleFeature { bundle_id, .. }) = negative else {
        panic!("non-extension tool path was accepted: {negative:?}");
    };
    assert_eq!(bundle_id, "hya/executable");
}

#[test]
fn executable_resource_rejects_ambiguous_extension_path_join() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime-a
      path: extensions/runtime.js
    - id: runtime-b
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - echo
"#;
    let result = prepare_package(BundleSource::new(
        "ambiguous-extension-path-join",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    ));

    assert_eq!(
        result.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/executable".to_string(),
            feature: "ambiguous executable resource:bundle:hya/executable/tool/echo".to_string(),
        })
    );
}

#[test]
fn unreachable_extension_is_rejected_during_prepare() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
"#;
    let result = prepare_package(BundleSource::new(
        "unreachable-extension",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    ));

    assert_eq!(
        result.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/executable".to_string(),
            feature: "unreachable extension:bundle:hya/executable/extension/runtime".to_string(),
        })
    );
}

#[test]
fn js_bundle_resources_prepare_decode_and_resolve_canonically() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
      aliases:
        - say
  hooks:
    - id: event
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - echo
    hook_refs:
      - bundle:hya/executable/hook/event
"#;
    let prepared = prepare_package(BundleSource::new(
        "js-resources",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    ));
    let Ok(prepared) = prepared else {
        panic!("JavaScript bundle resources were rejected: {prepared:?}");
    };

    let bundle = &prepared.bundles()[0];
    let tool = &bundle.tools[0];
    assert_eq!(tool.stable_id, "bundle:hya/executable/tool/echo");
    assert_eq!(tool.source_path, "extensions/runtime.js");
    assert_eq!(tool.content, "export const runtime = true;\n");
    assert_eq!(tool.aliases, ["say"]);
    assert!(!tool.digest.is_empty());

    let hook = &bundle.hooks[0];
    assert_eq!(hook.stable_id, "bundle:hya/executable/hook/event");
    assert_eq!(hook.source_path, "extensions/runtime.js");
    assert_eq!(hook.content, "export const runtime = true;\n");
    assert!(hook.aliases.is_empty());
    assert_eq!(hook.digest, tool.digest);

    let extension = &bundle.extensions[0];
    assert_eq!(
        extension.stable_id,
        "bundle:hya/executable/extension/runtime"
    );
    assert_eq!(extension.source_path, "extensions/runtime.js");
    assert_eq!(extension.content, "export const runtime = true;\n");
    assert!(extension.aliases.is_empty());
    assert_eq!(extension.digest, tool.digest);

    let agent = &bundle.agents[0];
    assert_eq!(
        agent.resource_view.allow,
        ["bundle:hya/executable/tool/echo"]
    );
    assert_eq!(agent.hook_refs, ["bundle:hya/executable/hook/event"]);

    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest());
    let Ok(decoded) = decoded else {
        panic!("prepared JavaScript bundle failed to decode: {decoded:?}");
    };
    assert_eq!(decoded.bundles(), prepared.bundles());

    let catalog = BundleCatalog::from_prepared(prepared.bundles());
    let Ok(catalog) = catalog else {
        panic!("prepared JavaScript bundle failed catalog validation: {catalog:?}");
    };
    assert_eq!(
        catalog.resolve_resource("hya/executable", ExportKind::Tool, "say"),
        Ok(tool)
    );
    assert_eq!(
        catalog.resolve_resource(
            "hya/executable",
            ExportKind::Hook,
            "bundle:hya/executable/hook/event",
        ),
        Ok(hook)
    );
    assert_eq!(
        catalog.resolve_resource("hya/executable", ExportKind::Extension, "runtime"),
        Ok(extension)
    );
}

#[test]
fn hook_local_ids_are_limited_to_supported_protocol_names() {
    let prepare_hook = |local_id: &str, source_path: &str| {
        let manifest = format!(
            r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  hooks:
    - id: {local_id}
      path: {source_path}
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - {local_id}
"#,
        );
        prepare_package(BundleSource::new(
            format!("hook-local-id-{local_id}"),
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new(source_path, b"export default {};\n".to_vec()),
            ],
        ))
    };

    for (local_id, source_path) in [
        ("event", "extensions/runtime.js"),
        ("tool.execute.before", "extensions/runtime.js"),
        ("tool.execute.after", "extensions/runtime.js"),
    ] {
        let prepared = prepare_hook(local_id, source_path);
        let Ok(prepared) = prepared else {
            panic!("supported hook local ID `{local_id}` was rejected: {prepared:?}");
        };
        let agent = &prepared.bundles()[0].agents[0];
        assert_eq!(
            agent.hook_refs,
            [format!("bundle:hya/executable/hook/{local_id}")]
        );
    }

    let unsupported = prepare_hook("audit", "extensions/runtime.js");
    assert_eq!(
        unsupported.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/executable".to_string(),
            feature: "hook:audit".to_string(),
        })
    );
}

#[test]
fn unreferenced_unsupported_hook_local_id_is_rejected_before_publication() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/unreferenced-hook
  version: 1.0.0
  publisher: hya
resources:
  hooks:
    - id: audit
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
"#;
    let result = prepare_builtins(vec![BundleSource::new(
        "unreferenced-hook",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    )]);

    assert_eq!(
        result.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/unreferenced-hook".to_string(),
            feature: "hook:audit".to_string(),
        })
    );
}

#[test]
fn hook_refs_are_canonicalized_from_local_alias_and_stable_spellings() {
    for hook_ref in ["event", "notify", "bundle:hya/executable/hook/event"] {
        let manifest = format!(
            r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  hooks:
    - id: event
      path: extensions/runtime.js
      aliases:
        - notify
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - {hook_ref}
"#,
        );
        let prepared = prepare_package(BundleSource::new(
            format!("hook-ref-{hook_ref}"),
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new(
                    "extensions/runtime.js",
                    b"export const event = true;\n".as_slice(),
                ),
            ],
        ));
        let Ok(prepared) = prepared else {
            panic!("hook reference `{hook_ref}` was rejected: {prepared:?}");
        };
        let agent = &prepared.bundles()[0].agents[0];
        assert_eq!(agent.hook_refs, ["bundle:hya/executable/hook/event"]);
    }
}

#[test]
fn unqualified_hook_ref_is_rejected_when_short_name_is_cross_kind_ambiguous() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: shared
      path: extensions/runtime.js
  hooks:
    - id: event
      path: extensions/runtime.js
      aliases:
        - shared
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - shared
"#;
    let result = prepare_package(BundleSource::new(
        "ambiguous-hook-ref",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const shared = true;\n".as_slice(),
            ),
        ],
    ));
    let Err(error) = result else {
        panic!("ambiguous unqualified hook reference was accepted");
    };
    assert_eq!(
        error,
        BundleError::AliasCollision {
            bundle_id: "hya/executable".to_string(),
            name: "shared".to_string(),
        }
    );
}

#[test]
fn hook_refs_reject_unknown_and_wrong_kind_as_hook_references() {
    let unknown_local = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
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
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - missing
"#;
    let unknown_local_result = prepare_package(BundleSource::new(
        "unknown-local-hook",
        vec![
            SourceFile::new("bundle.yaml", unknown_local.as_slice()),
            SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
        ],
    ));
    assert_eq!(
        unknown_local_result.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/executable".to_string(),
            kind: "hook".to_string(),
            reference: "missing".to_string(),
        })
    );

    let wrong_kind = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - bundle:hya/executable/tool/echo
"#;
    let wrong_kind_result = prepare_package(BundleSource::new(
        "wrong-kind-hook",
        vec![
            SourceFile::new("bundle.yaml", wrong_kind.as_slice()),
            SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
        ],
    ));
    assert_eq!(
        wrong_kind_result.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/executable".to_string(),
            kind: "hook".to_string(),
            reference: "bundle:hya/executable/tool/echo".to_string(),
        })
    );
}

#[test]
fn harness_prefixed_hook_refs_are_rejected_as_unknown_bundle_hooks() {
    for (index, raw_hook_ref) in [
        "harness:hook/event",
        "harness:hook/tool.execute.before",
        "harness:hook/tool.execute.after",
        "harness:hook/unknown",
        "harness:hook/",
        "harness:hook",
    ]
    .into_iter()
    .enumerate()
    {
        let manifest = format!(
            r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - "{raw_hook_ref}"
"#,
        );
        let result = prepare_builtins(vec![BundleSource::new(
            format!("harness-hook-ref-{index}"),
            vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
        )]);
        assert_eq!(
            result.err(),
            Some(BundleError::UnknownResourceReference {
                bundle_id: "hya/executable".to_string(),
                kind: "hook".to_string(),
                reference: raw_hook_ref.to_string(),
            })
        );
    }
}

#[test]
fn harness_prefixed_hook_resource_view_reference_is_rejected() {
    let result = prepare_builtins(vec![BundleSource::new(
        "harness-resource-view-ref",
        vec![SourceFile::new(
            "bundle.yaml",
            minimal_manifest("    resource_view:\n      allow:\n        - harness:hook/event\n"),
        )],
    )]);

    assert_eq!(
        result.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/minimal".to_string(),
            kind: "resource".to_string(),
            reference: "harness:hook/event".to_string(),
        })
    );
}

#[test]
fn duplicate_hook_refs_are_rejected_after_canonicalization() {
    for (index, hook_refs) in [
        ["event", "event"],
        ["event", "notify"],
        ["notify", "bundle:hya/executable/hook/event"],
    ]
    .into_iter()
    .enumerate()
    {
        let [first, second] = hook_refs;
        let manifest = format!(
            r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/executable
  version: 1.0.0
  publisher: hya
resources:
  hooks:
    - id: event
      path: extensions/runtime.js
      aliases:
        - notify
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    hook_refs:
      - {first}
      - {second}
"#,
        );
        let result = prepare_package(BundleSource::new(
            format!("duplicate-hook-refs-{index}"),
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new(
                    "extensions/runtime.js",
                    b"export const event = true;\n".as_slice(),
                ),
            ],
        ));
        assert_eq!(
            result.err(),
            Some(BundleError::AliasCollision {
                bundle_id: "hya/executable".to_string(),
                name: "bundle:hya/executable/hook/event".to_string(),
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
