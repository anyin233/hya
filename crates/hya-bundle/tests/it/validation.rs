//! Manifest validation: aliases, `can_spawn`, executable/hook rules, removed keys, and namespaces.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ExportKind, PreparedCatalog, SourceFile,
    prepare_package,
};
use sha2::{Digest, Sha256};

fn agent_source(root: &str, bundle_id: &str, stable_id: &str, can_spawn: &[&str]) -> BundleSource {
    let can_spawn = if can_spawn.is_empty() {
        "      []".to_string()
    } else {
        can_spawn
            .iter()
            .map(|t| format!("      - {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: {stable_id}\n    role: main\n    can_spawn:\n{can_spawn}\n"
    );
    BundleSource::new(
        root,
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

fn minimal_manifest(extra: &str) -> Vec<u8> {
    let extra = extra
        .lines()
        .map(|line| format!("  {line}\n"))
        .collect::<String>();
    format!(
        "kind: Bundle\nidentity:\n  id: hya/minimal\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: minimal-lead\n    role: main\n{extra}"
    )
    .into_bytes()
}

fn namespace_source(namespace: Option<&str>, id: &str) -> BundleSource {
    let ns_line = namespace
        .map(|v| format!("namespace: {v}\n"))
        .unwrap_or_default();
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: {id}\n  version: 1.0.0\n  publisher: hya\n{ns_line}agents:\n  - id: lead\n    role: main\n    prompt: prompts/lead.md\n"
    );
    BundleSource::new(
        "ns-test",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("prompts/lead.md", b"You are the lead.\n".as_slice()),
        ],
    )
}

#[test]
fn resource_view_aliases_cross_bundle_rejection_and_canonicalization() {
    // Alias cannot collide with a bundle-local short name.
    let collision = prepare_package(BundleSource::new(
        "alias-conflict",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/test, version: 1.0.0, publisher: hya }\nresources:\n  skills: [{ id: local-docs, path: resources/skills/local-docs.md }]\nagents:\n  - id: lead\n    role: main\n    resource_view:\n      aliases:\n        local-docs: bundle:hya/test/skill/local-docs\n".as_slice(),
            ),
            SourceFile::new("resources/skills/local-docs.md", b"# Local\n".as_slice()),
        ],
    ));
    assert_eq!(
        collision.err(),
        Some(BundleError::AliasCollision {
            bundle_id: "hya/test".into(),
            name: "local-docs".into(),
        })
    );

    // Cross-bundle resource reference rejected.
    let cross = prepare_package(BundleSource::new(
        "alpha",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: hya/alpha, version: 1.0.0, publisher: hya }\nagents:\n  - id: alpha-lead\n    role: main\n    resource_view:\n      aliases:\n        beta-docs: bundle:hya/beta/skill/docs\n".as_slice(),
        )],
    ));
    assert_eq!(
        cross.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".into(),
            kind: "resource".into(),
            reference: "bundle:hya/beta/skill/docs".into(),
        })
    );

    // Local & qualified allow/deny entries canonicalize & deduplicate.
    let canon = prepare_package(BundleSource::new(
        "canonical-view",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/canonical-view, version: 1.0.0, publisher: hya }\nresources:\n  skills: [{ id: docs, path: resources/skills/docs.md }]\nagents:\n  - id: canonical-lead\n    role: main\n    resource_view:\n      allow: [docs, bundle:hya/canonical-view/skill/docs]\n      deny: [docs]\n".as_slice(),
            ),
            SourceFile::new("resources/skills/docs.md", b"# Docs\n".as_slice()),
        ],
    ))
    .expect("canonical view");
    let view = &canon.bundles()[0].agents()[0].resource_view;
    assert_eq!(view.allow, ["bundle:hya/canonical-view/skill/docs"]);
    assert_eq!(view.deny, ["bundle:hya/canonical-view/skill/docs"]);
}

#[test]
fn can_spawn_verbatim_sorting_catalog_id_shadowing_and_duplicates() {
    let prepared = prepare_package(agent_source(
        "alpha",
        "hya/alpha",
        "alpha-lead",
        &["missing-agent", "beta-worker", "missing-agent"],
    ))
    .expect("unresolved can_spawn prepares");
    assert_eq!(
        prepared.bundles()[0].agents()[0]
            .can_spawn
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>(),
        ["beta-worker", "missing-agent"]
    );

    // Stable agent id cannot shadow another bundle's qualified catalog id.
    let intruder = prepare_package(agent_source(
        "alpha",
        "hya/alpha",
        "bundle:hya/beta/agent/beta-worker",
        &[],
    ))
    .expect("intruder prepares alone");
    let beta = prepare_package(agent_source("beta", "hya/beta", "beta-worker", &[])).expect("beta");
    assert_eq!(
        BundleCatalog::from_prepared(&[intruder.bundles(), beta.bundles()].concat()).err(),
        Some(BundleError::NamespaceCollision {
            bundle_id: "hya/beta".into(),
            name: "bundle:hya/beta/agent/beta-worker".into(),
        })
    );

    // Duplicate stable agent id across bundles, wrong kind, and parent source path.
    let a = prepare_package(agent_source("a", "hya/a", "same", &[])).expect("a");
    let b = prepare_package(agent_source("b", "hya/b", "same", &[])).expect("b");
    assert_eq!(
        BundleCatalog::from_prepared(&[a.bundles(), b.bundles()].concat()).err(),
        Some(BundleError::DuplicateStableAgentId {
            stable_id: "same".into()
        })
    );
    let wrong_kind = String::from_utf8(minimal_manifest(""))
        .unwrap()
        .replace("kind: Bundle", "kind: UnsupportedBundle");
    assert!(matches!(
        prepare_package(BundleSource::new(
            "wrong-kind",
            vec![SourceFile::new("bundle.yaml", wrong_kind.into_bytes())],
        )),
        Err(BundleError::WrongKind { .. })
    ));
    assert!(matches!(
        prepare_package(BundleSource::new(
            "parent-path",
            vec![SourceFile::new("../bundle.yaml", minimal_manifest(""))],
        )),
        Err(BundleError::InvalidSourcePath { .. })
    ));
}

#[test]
fn unsupported_features_unknown_fields_and_missing_references_fail_typed() {
    // Unsupported resource_profile.
    assert_eq!(
        prepare_package(BundleSource::new(
            "resource-profile",
            vec![SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/profile, version: 1.0.0, publisher: hya }\nagents:\n  - id: lead\n    role: main\n    resource_profile: { max_depth: 2, per_team_turn_budget: 8 }\n".as_slice(),
            )],
        ))
        .err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/profile".into(),
            feature: "agent.resource_profile".into(),
        })
    );

    // Unknown/removed agent and source-schema fields.
    for fragment in [
        "  hidden: true\n",
        "  temperature: 0.2\n",
        "  top_p: 0.8\n",
        "  steps: 3\n",
        "  options: {}\n",
        "  request: {}\n",
        "  permission_overlay: {}\n",
        "  permissions: []\n",
        "  permission: {}\n",
        "  tools: {}\n",
        "  readonly: true\n",
        "resources:\n  files: []\n",
        "resources:\n  helpers: []\n",
        "resources:\n  dependencies: []\n",
        "helpers: []\n",
        "dependencies: []\n",
        "imports: []\n",
    ] {
        assert!(
            matches!(
                prepare_package(BundleSource::new(
                    "unknown-field",
                    vec![SourceFile::new("bundle.yaml", minimal_manifest(fragment))],
                )),
                Err(BundleError::InvalidManifest { .. })
            ),
            "field silently accepted: {fragment:?}"
        );
    }

    // Missing prompt reference.
    assert_eq!(
        prepare_package(BundleSource::new(
            "missing-prompt",
            vec![SourceFile::new(
                "bundle.yaml",
                minimal_manifest("  prompt: prompts/missing.md\n"),
            )],
        ))
        .err(),
        Some(BundleError::MissingReference {
            bundle_id: "hya/minimal".into(),
            path: "prompts/missing.md".into(),
        })
    );

    // Reserved extensions.rust without extensions.process.
    let manifest = String::from_utf8(minimal_manifest("")).unwrap().replace(
        "agents:\n",
        "extensions:\n  rust:\n    - id: runtime\n      path: extensions/runtime\nagents:\n",
    );
    assert_eq!(
        prepare_package(BundleSource::new(
            "extensions.rust",
            vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
        ))
        .err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/minimal".into(),
            feature: "extensions.rust".into(),
        })
    );
}

#[test]
fn executable_resource_extension_join_ambiguity_and_reachability() {
    let manifest = |tool_path: &str, ext_block: &str| {
        format!(
            "kind: Bundle\nidentity: {{ id: hya/executable, version: 1.0.0, publisher: hya }}\nresources:\n  tools: [{{ id: echo, path: {tool_path} }}]\nextensions:\n  js:\n{ext_block}\nagents:\n  - id: lead\n    role: main\n    resource_view:\n      allow: [echo]\n"
        )
    };
    let js = b"export const runtime = true;\n".as_slice();

    // Positive exact-path join.
    let ok = prepare_package(BundleSource::new(
        "join-ok",
        vec![
            SourceFile::new(
                "bundle.yaml",
                manifest(
                    "extensions/runtime.js",
                    "    - { id: runtime, path: extensions/runtime.js }",
                ),
            ),
            SourceFile::new("extensions/runtime.js", js),
        ],
    ))
    .expect("exact-path join");
    assert_eq!(
        ok.bundles()[0].tools()[0].digest,
        ok.bundles()[0].extensions()[0].digest
    );

    // Mismatched path rejected.
    let mismatch = prepare_package(BundleSource::new(
        "join-mismatch",
        vec![
            SourceFile::new(
                "bundle.yaml",
                manifest(
                    "tools/echo.js",
                    "    - { id: runtime, path: extensions/runtime.js }",
                ),
            ),
            SourceFile::new("tools/echo.js", js),
            SourceFile::new("extensions/runtime.js", js),
        ],
    ));
    assert!(matches!(
        mismatch,
        Err(BundleError::UnsupportedBundleFeature { bundle_id, .. }) if bundle_id == "hya/executable"
    ));

    // Ambiguous duplicate extension paths rejected.
    let ambiguous = prepare_package(BundleSource::new(
        "join-ambiguous",
        vec![
            SourceFile::new(
                "bundle.yaml",
                manifest(
                    "extensions/runtime.js",
                    "    - { id: runtime-a, path: extensions/runtime.js }\n    - { id: runtime-b, path: extensions/runtime.js }",
                ),
            ),
            SourceFile::new("extensions/runtime.js", js),
        ],
    ));
    assert_eq!(
        ambiguous.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/executable".into(),
            feature: "ambiguous executable resource:bundle:hya/executable/tool/echo".into(),
        })
    );

    // Unreachable extension rejected.
    let unreachable = prepare_package(BundleSource::new(
        "unreachable-ext",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/executable, version: 1.0.0, publisher: hya }\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents: [{ id: lead, role: main }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", js),
        ],
    ));
    assert_eq!(
        unreachable.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/executable".into(),
            feature: "unreachable extension:bundle:hya/executable/extension/runtime".into(),
        })
    );
}

#[test]
fn js_bundle_resources_prepare_decode_and_resolve_canonically() {
    let prepared = prepare_package(BundleSource::new(
        "js-resources",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/executable, version: 1.0.0, publisher: hya }\nresources:\n  tools: [{ id: echo, path: extensions/runtime.js, aliases: [say] }]\n  hooks: [{ id: event, path: extensions/runtime.js }]\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents:\n  - id: lead\n    role: main\n    resource_view:\n      allow: [echo]\n    hook_refs: [bundle:hya/executable/hook/event]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export const runtime = true;\n".as_slice()),
        ],
    ))
    .expect("JS resources prepare");
    let bundle = &prepared.bundles()[0];
    let tool = &bundle.tools()[0];
    let hook = &bundle.hooks()[0];
    let ext = &bundle.extensions()[0];
    assert_eq!(tool.stable_id, "bundle:hya/executable/tool/echo");
    assert_eq!(tool.aliases, ["say"]);
    assert_eq!(hook.stable_id, "bundle:hya/executable/hook/event");
    assert_eq!(ext.stable_id, "bundle:hya/executable/extension/runtime");
    assert_eq!(tool.digest, hook.digest);
    assert_eq!(tool.digest, ext.digest);
    assert_eq!(
        bundle.agents()[0].resource_view.allow,
        ["bundle:hya/executable/tool/echo"]
    );
    assert_eq!(
        bundle.agents()[0].hook_refs,
        ["bundle:hya/executable/hook/event"]
    );

    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode");
    assert_eq!(decoded.bundles(), prepared.bundles());
    let catalog = BundleCatalog::from_prepared(prepared.bundles()).expect("catalog");
    assert_eq!(
        catalog.resolve_resource("hya/executable", ExportKind::Tool, "say"),
        Ok(tool)
    );
    assert_eq!(
        catalog.resolve_resource(
            "hya/executable",
            ExportKind::Hook,
            "bundle:hya/executable/hook/event"
        ),
        Ok(hook)
    );
    assert_eq!(
        catalog.resolve_resource("hya/executable", ExportKind::Extension, "runtime"),
        Ok(ext)
    );
}

#[test]
fn hook_local_ids_limit_protocol_names_even_when_unreferenced() {
    let prep = |local_id: &str, referenced: bool| {
        let refs = if referenced {
            format!("    hook_refs: [{local_id}]\n")
        } else {
            String::new()
        };
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/executable, version: 1.0.0, publisher: hya }}\nresources:\n  hooks: [{{ id: {local_id}, path: extensions/runtime.js }}]\nextensions:\n  js: [{{ id: runtime, path: extensions/runtime.js }}]\nagents:\n  - id: lead\n    role: main\n{refs}"
        );
        prepare_package(BundleSource::new(
            format!("hook-{local_id}"),
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
            ],
        ))
    };
    for local_id in ["event", "tool.execute.before", "tool.execute.after"] {
        let prepared = prep(local_id, true).expect("supported hook");
        assert_eq!(
            prepared.bundles()[0].agents()[0].hook_refs,
            [format!("bundle:hya/executable/hook/{local_id}")]
        );
    }
    for referenced in [true, false] {
        assert_eq!(
            prep("audit", referenced).err(),
            Some(BundleError::UnsupportedBundleFeature {
                bundle_id: "hya/executable".into(),
                feature: "hook:audit".into(),
            })
        );
    }
}

#[test]
fn hook_refs_canonicalization_cross_kind_ambiguity_and_duplicates() {
    // Local, alias, and stable spellings all canonicalize to the stable id.
    for hook_ref in ["event", "notify", "bundle:hya/executable/hook/event"] {
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/executable, version: 1.0.0, publisher: hya }}\nresources:\n  hooks: [{{ id: event, path: extensions/runtime.js, aliases: [notify] }}]\nextensions:\n  js: [{{ id: runtime, path: extensions/runtime.js }}]\nagents:\n  - id: lead\n    role: main\n    hook_refs: [{hook_ref}]\n"
        );
        let prepared = prepare_package(BundleSource::new(
            "hook-ref",
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new(
                    "extensions/runtime.js",
                    b"export const event = true;\n".as_slice(),
                ),
            ],
        ))
        .expect("hook ref");
        assert_eq!(
            prepared.bundles()[0].agents()[0].hook_refs,
            ["bundle:hya/executable/hook/event"]
        );
    }

    // Duplicate hook_refs rejected after canonicalization.
    for [first, second] in [
        ["event", "event"],
        ["event", "notify"],
        ["notify", "bundle:hya/executable/hook/event"],
    ] {
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/executable, version: 1.0.0, publisher: hya }}\nresources:\n  hooks: [{{ id: event, path: extensions/runtime.js, aliases: [notify] }}]\nextensions:\n  js: [{{ id: runtime, path: extensions/runtime.js }}]\nagents:\n  - id: lead\n    role: main\n    hook_refs: [{first}, {second}]\n"
        );
        assert_eq!(
            prepare_package(BundleSource::new(
                "dup-hook",
                vec![
                    SourceFile::new("bundle.yaml", manifest.into_bytes()),
                    SourceFile::new(
                        "extensions/runtime.js",
                        b"export const event = true;\n".as_slice()
                    ),
                ],
            ))
            .err(),
            Some(BundleError::AliasCollision {
                bundle_id: "hya/executable".into(),
                name: "bundle:hya/executable/hook/event".into(),
            })
        );
    }

    // Cross-kind ambiguous short name rejected as AliasCollision.
    assert_eq!(
        prepare_package(BundleSource::new(
            "ambiguous-hook",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    b"kind: Bundle\nidentity: { id: hya/executable, version: 1.0.0, publisher: hya }\nresources:\n  tools: [{ id: shared, path: extensions/runtime.js }]\n  hooks: [{ id: event, path: extensions/runtime.js, aliases: [shared] }]\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents: [{ id: lead, role: main, hook_refs: [shared] }]\n".as_slice(),
                ),
                SourceFile::new("extensions/runtime.js", b"export const shared = true;\n".as_slice()),
            ],
        ))
        .err(),
        Some(BundleError::AliasCollision {
            bundle_id: "hya/executable".into(),
            name: "shared".into(),
        })
    );
}

#[test]
fn hook_refs_reject_unknown_wrong_kind_and_harness_prefixed() {
    let err_unknown = prepare_package(BundleSource::new(
        "unknown-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/executable, version: 1.0.0, publisher: hya }\nresources:\n  hooks: [{ id: event, path: extensions/runtime.js }]\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents: [{ id: lead, role: main, hook_refs: [missing] }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
        ],
    ));
    assert_eq!(
        err_unknown.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/executable".into(),
            kind: "hook".into(),
            reference: "missing".into(),
        })
    );

    let err_kind = prepare_package(BundleSource::new(
        "wrong-kind-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/executable, version: 1.0.0, publisher: hya }\nresources:\n  tools: [{ id: echo, path: extensions/runtime.js }]\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents: [{ id: lead, role: main, hook_refs: [bundle:hya/executable/tool/echo] }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
        ],
    ));
    assert_eq!(
        err_kind.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/executable".into(),
            kind: "hook".into(),
            reference: "bundle:hya/executable/tool/echo".into(),
        })
    );

    for raw_ref in [
        "harness:hook/event",
        "harness:hook/tool.execute.before",
        "harness:hook/tool.execute.after",
        "harness:hook/unknown",
        "harness:hook/",
        "harness:hook",
    ] {
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/executable, version: 1.0.0, publisher: hya }}\nagents: [{{ id: lead, role: main, hook_refs: [\"{raw_ref}\"] }}]\n"
        );
        assert_eq!(
            prepare_package(BundleSource::new(
                "harness-hook",
                vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
            ))
            .err(),
            Some(BundleError::UnknownResourceReference {
                bundle_id: "hya/executable".into(),
                kind: "hook".into(),
                reference: raw_ref.into(),
            })
        );
    }

    assert_eq!(
        prepare_package(BundleSource::new(
            "harness-rv",
            vec![SourceFile::new(
                "bundle.yaml",
                minimal_manifest("  resource_view:\n    allow:\n      - harness:hook/event\n"),
            )],
        ))
        .err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/minimal".into(),
            kind: "resource".into(),
            reference: "harness:hook/event".into(),
        })
    );
}

#[test]
fn spawn_lifecycle_removed_and_old_v3_catalog_requires_reinstall() {
    let flow = "---\nkind: Workflow\nname: flow\ndescription: One stage.\nnodes:\n  work:\n    agent: flow-worker\n    directive: Work.\n---\nflowchart TD\n  work\n";
    for (name, manifest) in [
        (
            "agent",
            "kind: Bundle\nidentity: { id: acme/solo, version: 1.0.0, publisher: acme }\nagents: [{ id: solo, role: subagent, spawn_lifecycle: transient }]\n",
        ),
        (
            "agent-set",
            "kind: Bundle\nidentity: { id: acme/set, version: 1.0.0, publisher: acme }\nagents: [{ id: worker, role: subagent, spawn_lifecycle: resident }]\n",
        ),
        (
            "workflow",
            "kind: Bundle\nidentity: { id: acme/flow, version: 1.0.0, publisher: acme }\nworkflow: { id: flow, path: workflows/flow.hya.md }\nagents: [{ id: flow-worker, role: subagent, spawn_lifecycle: transient }]\n",
        ),
    ] {
        let mut files = vec![SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec())];
        if name == "workflow" {
            files.push(SourceFile::new(
                "workflows/flow.hya.md",
                flow.as_bytes().to_vec(),
            ));
        }
        let err =
            prepare_package(BundleSource::new(name, files)).expect_err("spawn_lifecycle rejected");
        assert!(
            matches!(&err, BundleError::RemovedManifestKey { key, .. } if key == "spawn_lifecycle")
        );
        let text = err.to_string();
        assert!(
            text.contains("spawn_lifecycle") && text.contains("resident"),
            "{name}: {text}"
        );
    }

    let prepared = prepare_package(agent_source("old", "acme/old", "old-lead", &[])).expect("prep");
    let mut doc: serde_json::Value = serde_json::from_slice(prepared.bytes()).expect("JSON");
    doc["format_version"] = serde_json::json!(3);
    doc["agents"][0]["spawn_lifecycle"] = serde_json::json!("transient");
    let bytes = serde_json::to_vec(&doc).expect("encode");
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let err = PreparedCatalog::decode(&bytes, &digest).expect_err("v3 rejected");
    assert_eq!(err, BundleError::UnsupportedPreparedFormat { found: 3 });
    assert!(err.to_string().contains("please reinstall"));
}

#[test]
fn namespace_defaults_explicit_empty_and_workflow_resolution() {
    for (ns, expected) in [
        (None, "acme-tools"),
        (Some("acme"), "acme"),
        (Some(""), "acme-tools"),
    ] {
        let cat = prepare_package(namespace_source(ns, "hya/acme-tools")).expect("prepare");
        assert_eq!(cat.bundles()[0].namespace.as_deref(), Some(expected));
    }

    let wf = prepare_package(BundleSource::new(
        "ns-flow",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/flow-pack, version: 1.0.0, publisher: hya }\nnamespace: flow\nworkflow: { id: demo, path: workflows/demo.hya.md }\nagents: [{ id: worker, role: subagent, prompt: prompts/worker.md }]\n".as_slice(),
            ),
            SourceFile::new(
                "workflows/demo.hya.md",
                b"---\nkind: Workflow\nname: demo\ndescription: demo\nnodes:\n  execute:\n    agent: worker\n    directive: Work.\n---\nflowchart TD\n  execute\n".as_slice(),
            ),
            SourceFile::new("prompts/worker.md", b"Work.\n".as_slice()),
        ],
    ))
    .expect("workflow namespace");
    assert_eq!(wf.bundles()[0].namespace.as_deref(), Some("flow"));
}

#[test]
fn namespace_rejects_invalid_reserved_and_invalid_default_tokens() {
    for bad in ["bad__ns", "has.dot", "has/slash", "has space", "__lead"] {
        assert!(matches!(
            prepare_package(namespace_source(Some(bad), "hya/acme-tools")),
            Err(BundleError::InvalidNamespace { namespace, .. }) if namespace == bad
        ));
    }
    for reserved in ["mcp", "harness", "builtin", "plugin"] {
        assert!(matches!(
            prepare_package(namespace_source(Some(reserved), "hya/acme-tools")),
            Err(BundleError::InvalidNamespace { .. })
        ));
    }
    assert!(matches!(
        prepare_package(namespace_source(None, "hya/my.tool")),
        Err(BundleError::InvalidNamespace { .. })
    ));
}
