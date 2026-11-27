//! Source preparation, digesting, rehashing, markdown bundles, agent sets,
//! channel templates, manifest fields, process resources, TUI manifests, and version references.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ChannelCapability, ChannelParticipantRole,
    ChannelScope, ChannelTemplateKind, HandoffPolicy, PreparedCatalog, PreparedChannelParticipant,
    SourceFile, inspect_public_package, prepare_package, write_public_package,
};
use hya_version::VERSION;
use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn rehash_bundle(document: &mut serde_json::Value) -> Vec<u8> {
    let mut digest_input = document.clone();
    digest_input
        .as_object_mut()
        .expect("prepared bundle is an object")
        .remove("digest");
    let bundle_digest =
        sha256_hex(&serde_json::to_vec(&digest_input).expect("encode digest input"));
    document["digest"] = serde_json::Value::String(bundle_digest);
    serde_json::to_vec(document).expect("encode document")
}

fn agent_source(root: &str, bundle_id: &str, stable_id: &str, reverse_files: bool) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: {stable_id}\n    role: main\n    prompt: prompts/lead.md\n"
    );
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new(
            "prompts/lead.md",
            format!("You are {stable_id}.\n").into_bytes(),
        ),
    ];
    if reverse_files {
        files.reverse();
    }
    BundleSource::new(root, files)
}

fn skill_source(resource_path: &str) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/skills\n  version: 1.0.0\n  publisher: hya\nresources:\n  skills:\n    - id: docs\n      path: {resource_path}\nagents:\n  - id: lead\n    role: main\n"
    );
    BundleSource::new(
        "skill-source",
        vec![
            SourceFile::new("./bundle.yaml", manifest.into_bytes()),
            SourceFile::new(resource_path, b"# Docs\n".as_slice()),
        ],
    )
}

fn workflow_source(include_orphan: bool) -> BundleSource {
    let extra = if include_orphan {
        "  - id: orphan\n    role: subagent\n    prompt: prompts/orphan.md\n"
    } else {
        ""
    };
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/workflow-demo\n  version: 1.0.0\n  publisher: hya\nworkflow:\n  id: demo\n  path: workflows/demo.hya.md\nagents:\n  - id: worker\n    role: subagent\n    prompt: prompts/worker.md\n{extra}"
    );
    let workflow = b"---\nkind: Workflow\nname: demo\ndescription: One packaged Workflow.\nnodes:\n  execute:\n    agent: worker\n    directive: Execute the request.\n---\nflowchart TD\n  execute\n";
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new("workflows/demo.hya.md", workflow.as_slice()),
        SourceFile::new("prompts/worker.md", b"Execute carefully.\n".as_slice()),
    ];
    if include_orphan {
        files.push(SourceFile::new(
            "prompts/orphan.md",
            b"Unreachable.\n".as_slice(),
        ));
    }
    BundleSource::new("workflow-source", files)
}

fn workflow_closure_source(include_verifier: bool, include_helper: bool) -> BundleSource {
    let verifier = if include_verifier {
        "  - id: verifier\n    role: subagent\n    prompt: prompts/verifier.md\n"
    } else {
        ""
    };
    let helper = if include_helper {
        "  - id: helper\n    role: subagent\n    prompt: prompts/helper.md\n"
    } else {
        ""
    };
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/workflow-closure\n  version: 1.0.0\n  publisher: hya\nworkflow:\n  id: closure\n  path: workflows/closure.hya.md\nagents:\n  - id: worker\n    role: subagent\n    prompt: prompts/worker.md\n    can_spawn:\n      - helper\n{verifier}{helper}"
    );
    let workflow = b"---\nkind: Workflow\nname: closure\ndescription: Verifier and helper Workflow.\nnodes:\n  execute:\n    agent: worker\n    directive: Execute the request.\n    mode: loop\n    verify:\n      agent: verifier\n      until: the result is valid\n      max_iterations: 2\n---\nflowchart TD\n  execute\n";
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new("workflows/closure.hya.md", workflow.as_slice()),
        SourceFile::new("prompts/worker.md", b"Execute carefully.".as_slice()),
    ];
    if include_verifier {
        files.push(SourceFile::new(
            "prompts/verifier.md",
            b"Verify.".as_slice(),
        ));
    }
    if include_helper {
        files.push(SourceFile::new("prompts/helper.md", b"Help.".as_slice()));
    }
    BundleSource::new("workflow-closure", files)
}

fn schema_bundle(schemas_block: Option<&str>) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/schema-demo\n  version: 1.0.0\n  publisher: hya\n{}resources:\n  tools:\n    - id: query\n      path: extensions/runtime.js\nextensions:\n  js:\n    - id: runtime\n      path: extensions/runtime.js\nagents:\n  - id: lead\n    role: main\n    resource_view:\n      allow:\n        - query\n        - runtime\n",
        schemas_block.unwrap_or_default()
    );
    BundleSource::new(
        "schema-demo",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("extensions/runtime.js", b"export default {}".as_slice()),
        ],
    )
}

fn process_bundle(process_block: Option<&str>) -> BundleSource {
    let process = process_block.map_or_else(String::new, |block| format!("  process:\n{block}"));
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/process-demo\n  version: 1.0.0\n  publisher: hya\nextensions:\n{process}\nagents:\n  - id: process-lead\n    role: main\n"
    );
    BundleSource::new(
        "process-demo",
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

fn mcp_bundle(mcp_list: &str, files: &[(&str, &str)]) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nidentity:\n  id: hya/mcp-demo\n  version: 1.0.0\n  publisher: hya\nresources:\n  mcp:\n{mcp_list}\nagents:\n  - id: mcp-lead\n    role: main\n"
    );
    let mut sources = vec![SourceFile::new("bundle.yaml", manifest.into_bytes())];
    for (path, content) in files {
        sources.push(SourceFile::new(*path, content.as_bytes().to_vec()));
    }
    BundleSource::new("mcp-demo", sources)
}

fn legacy_fields_source(agent_fields: &str, process_fields: &str) -> BundleSource {
    BundleSource::new(
        "legacy-manifest",
        vec![SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: Bundle\nidentity: {{ id: acme/legacy, version: 1.0.0, publisher: acme }}\nagents: [{{ id: legacy, role: main{agent_fields} }}]\nextensions:\n  process: {{ kind: bun, command: [bun, run, runtime.ts]{process_fields} }}\n"
            ),
        )],
    )
}

#[test]
fn old_agent_bundle_kind_agentless_multi_agent_and_mixed_catalog() {
    let old = prepare_package(BundleSource::new(
        "old-agent-bundle",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: AgentBundle\nidentity: { id: test/old, version: 1.0.0, publisher: test }\nagent: { id: old-agent, role: main, prompt: prompt.md }\n".as_slice(),
        )],
    ));
    assert!(matches!(&old, Err(BundleError::WrongKind { found, .. }) if found == "AgentBundle"));

    let plugin = prepare_package(BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: hya/plugin-pack, version: 1.0.0, publisher: hya }\n"
                .as_slice(),
        )],
    ))
    .expect("agentless bundle prepares");
    assert_eq!(plugin.bundles()[0].kind().as_str(), "Bundle");
    assert!(plugin.bundles()[0].agents().is_empty());

    let multi = prepare_package(BundleSource::new(
        "agent-set",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/agent-set, version: 1.0.0, publisher: hya }\nagents:\n  - { id: alpha, role: main, prompt: prompts/alpha.md }\n  - { id: beta, role: subagent, prompt: prompts/beta.md }\n".as_slice(),
            ),
            SourceFile::new("prompts/alpha.md", b"Alpha.\n".as_slice()),
            SourceFile::new("prompts/beta.md", b"Beta.\n".as_slice()),
        ],
    ))
    .expect("multi-agent bundle prepares");
    let doc: serde_json::Value = serde_json::from_slice(multi.bytes()).expect("JSON");
    assert!(
        doc.get("kind").is_none() && doc.get("index").is_none() && doc.get("bundles").is_none()
    );
    assert_eq!(doc["agents"].as_array().expect("agents").len(), 2);

    let decoded = PreparedCatalog::decode(multi.bytes(), multi.digest()).expect("decode");
    let mixed = [plugin.bundles(), decoded.bundles()].concat();
    let catalog = BundleCatalog::from_prepared(&mixed).expect("mixed catalog");
    assert_eq!(catalog.bundles().len(), 2);
    assert_eq!(
        catalog.resolve_agent("alpha").map(|a| a.id.as_str()),
        Some("alpha")
    );
    assert_eq!(
        catalog.resolve_agent("beta").map(|a| a.id.as_str()),
        Some("beta")
    );

    // Static rust extension without extensions.process is unsupported.
    let rust_ext = prepare_package(BundleSource::new(
        "plugin-rust",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: hya/plugins-rust, version: 1.0.0, publisher: hya }\nextensions:\n  rust: [{ id: runtime, path: runtime.rs }]\n".as_slice(),
        )],
    ));
    assert!(matches!(
        rust_ext,
        Err(BundleError::UnsupportedBundleFeature { feature, .. }) if feature == "extensions.rust"
    ));
}

#[test]
fn static_skill_round_trips_and_malformed_declarations_are_rejected() {
    let prepared = prepare_package(BundleSource::new(
        "plugin-skill",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/plugins-demo, version: 1.0.0, publisher: hya }\nresources:\n  skills: [{ id: guide, path: skills/guide.md }]\n".as_slice(),
            ),
            SourceFile::new("skills/guide.md", b"Use the guide.\n".as_slice()),
        ],
    ))
    .expect("skill bundle prepares");
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode");
    assert_eq!(decoded.bundles()[0].skills.len(), 1);
    assert_eq!(decoded.bundles()[0].skills[0].local_id, "guide");

    for key in ["agent", "agents", "workflow", "channels"] {
        let text = format!(
            "kind: Bundle\nidentity: {{ id: hya/plugins-demo, version: 1.0.0, publisher: hya }}\n{key}: {{}}\n"
        );
        assert!(
            prepare_package(BundleSource::new(
                "plugin-invalid",
                vec![SourceFile::new("bundle.yaml", text.into_bytes())],
            ))
            .is_err(),
            "Bundle must reject malformed `{key}: {{}}`"
        );
    }
}

#[test]
fn deterministic_prepare_normalized_paths_directory_reader_and_process_digest() {
    let forward = prepare_package(agent_source("a", "hya/alpha", "alpha", false)).expect("fwd");
    let reverse = prepare_package(agent_source("a", "hya/alpha", "alpha", true)).expect("rev");
    assert_eq!(forward.bytes(), reverse.bytes());
    assert_eq!(forward.digest(), reverse.digest());
    assert_eq!(forward.index()[0].bundle_id, "hya/alpha");

    let canonical = prepare_package(skill_source("resources/skills/docs.md")).expect("canon");
    let dotted = prepare_package(skill_source("resources/./skills/docs.md")).expect("dotted");
    assert_eq!(canonical.bytes(), dotted.bytes());
    assert_eq!(canonical.digest(), dotted.digest());
    assert_eq!(
        canonical.bundles()[0].skills()[0].source_path,
        "resources/skills/docs.md"
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/directory");
    let dir_prepared =
        prepare_package(BundleSource::read_directory(&root).expect("read dir")).expect("prep dir");
    assert_eq!(dir_prepared.index()[0].bundle_id, "hya/directory-fixture");
    assert_eq!(
        dir_prepared.bundles()[0].agents()[0].prompt.as_deref(),
        Some("You are the directory fixture.")
    );

    let wf_cmd = |cmd: &str| {
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/workflow-demo, version: 1.0.0, publisher: hya }}\nworkflow: {{ id: demo, path: workflows/demo.hya.md }}\nagents: [{{ id: worker, role: subagent, prompt: prompts/worker.md }}]\nextensions:\n  process: {{ kind: rust, command: [{cmd}] }}\n"
        );
        prepare_package(workflow_source(false).with_file("bundle.yaml", manifest.into_bytes()))
            .expect("process workflow")
    };
    assert_ne!(
        wf_cmd("provider-before").bundles()[0].digest(),
        wf_cmd("provider-after").bundles()[0].digest()
    );
}

#[test]
fn workflow_bundle_prepares_v4_closure_and_retains_model_routes() {
    let prepared = prepare_package(workflow_source(false)).expect("workflow prepares");
    let doc: serde_json::Value = serde_json::from_slice(prepared.bytes()).expect("JSON");
    assert_eq!(doc["format_version"], 4);
    assert!(doc.get("kind").is_none());
    assert_eq!(doc["workflow"]["id"], "demo");
    assert_eq!(doc["agents"][0]["id"], "worker");
    assert_eq!(
        doc["workflow"]["compiler_revision"],
        "2ce247d3aaf2ecf6de48121779abf0056c467b0c1a01775cfdf1011108a76797"
    );

    let closure = prepare_package(workflow_closure_source(true, true)).expect("closure prepares");
    let [bundle] = closure.bundles() else {
        panic!("one bundle");
    };
    assert_eq!(
        bundle
            .agents()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["helper", "verifier", "worker"]
    );
    assert_eq!(bundle.workflow().map(|w| w.id.as_str()), Some("closure"));

    let routed = prepare_package(BundleSource::new(
        "workflow-model-routes",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/workflow-model-routes, version: 1.0.0, publisher: hya }\nworkflow: { id: routed, path: workflows/routed.hya.md }\nagents:\n  - { id: worker, role: subagent, prompt: prompts/worker.md }\n  - { id: verifier, role: subagent, prompt: prompts/verifier.md }\n".as_slice(),
            ),
            SourceFile::new(
                "workflows/routed.hya.md",
                b"---\nkind: Workflow\nname: routed\ndescription: Routed.\nnodes:\n  execute:\n    agent: worker\n    directive: Execute.\n    mode: loop\n    model:\n      id: fake/worker-primary\n      reasoning: high\n      fallback:\n        - id: fake/worker-fallback\n          reasoning: medium\n    verify:\n      agent: verifier\n      until: valid\n      max_iterations: 2\n      model:\n        id: fake/worker-primary\n        reasoning: low\n---\nflowchart TD\n  execute\n".as_slice(),
            ),
            SourceFile::new("prompts/worker.md", b"Work.\n".as_slice()),
            SourceFile::new("prompts/verifier.md", b"Verify.\n".as_slice()),
        ],
    ))
    .expect("routed workflow");
    let rdoc: serde_json::Value = serde_json::from_slice(routed.bytes()).expect("JSON");
    assert_eq!(rdoc["format_version"], 4);
    let src = rdoc["workflow"]["source"].as_str().unwrap_or_default();
    assert!(src.contains("id: fake/worker-primary"));
    assert!(src.contains("id: fake/worker-fallback"));
    assert!(src.contains("reasoning: low"));
    assert_ne!(rdoc["workflow"]["compiler_revision"], "");
}

#[test]
fn workflow_bundle_rejects_unreachable_and_missing_closure_agents() {
    assert!(prepare_package(workflow_source(true)).is_err());

    let stage_source = |stage_agent: &str| {
        BundleSource::new(
            "workflow-missing-agent",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    b"kind: Bundle\nidentity: { id: hya/workflow-missing-agent, version: 1.0.0, publisher: hya }\nworkflow: { id: demo, path: workflows/demo.hya.md }\nagents: [{ id: worker, role: subagent, prompt: prompts/worker.md }]\n".as_slice(),
                ),
                SourceFile::new(
                    "workflows/demo.hya.md",
                    format!(
                        "---\nkind: Workflow\nname: demo\ndescription: Missing.\nnodes:\n  execute:\n    agent: {stage_agent}\n    directive: Work.\n---\nflowchart TD\n  execute\n"
                    )
                    .into_bytes(),
                ),
                SourceFile::new("prompts/worker.md", b"Work.".as_slice()),
            ],
        )
    };
    for (source, bundle_id, agent_id, reference) in [
        (
            stage_source("missing"),
            "hya/workflow-missing-agent",
            "missing",
            "stage:execute",
        ),
        (
            stage_source("general"),
            "hya/workflow-missing-agent",
            "general",
            "stage:execute",
        ),
        (
            workflow_closure_source(false, true),
            "hya/workflow-closure",
            "verifier",
            "verifier:execute",
        ),
        (
            workflow_closure_source(true, false),
            "hya/workflow-closure",
            "helper",
            "agent:worker",
        ),
    ] {
        assert_eq!(
            prepare_package(source).err(),
            Some(BundleError::WorkflowAgentMissing {
                bundle_id: bundle_id.into(),
                agent_id: agent_id.into(),
                reference: reference.into(),
            })
        );
    }
}

#[test]
fn prepared_catalog_round_trips_rejects_tampered_bytes_and_v1_format() {
    let prepared =
        prepare_package(agent_source("alpha", "hya/alpha", "alpha", false)).expect("prep");
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode");
    assert_eq!(decoded.bytes(), prepared.bytes());
    assert_eq!(decoded.bundles(), prepared.bundles());
    assert_eq!(decoded.index(), prepared.index());

    let mut tampered = prepared.bytes().to_vec();
    let needle = b"You are alpha.";
    let offset = tampered
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("needle");
    tampered[offset] = b'X';
    assert!(matches!(
        PreparedCatalog::decode(&tampered, prepared.digest()),
        Err(BundleError::PreparedDigestMismatch { .. })
    ));

    let v1 = br#"{"format_version":1,"bundles":[],"index":[]}"#;
    assert_eq!(
        PreparedCatalog::decode(v1, &sha256_hex(v1)).err(),
        Some(BundleError::UnsupportedPreparedFormat { found: 1 })
    );
}

#[test]
fn prepared_decode_rejects_forged_singular_content_noncanonical_and_unknown_fields() {
    let prepared =
        prepare_package(agent_source("alpha", "hya/alpha", "alpha", false)).expect("prep");
    let base: serde_json::Value = serde_json::from_slice(prepared.bytes()).expect("JSON");

    // 1. Reintroduced singular `agent` field.
    let mut doc = base.clone();
    let agent = doc["agents"][0].clone();
    doc.as_object_mut().unwrap().insert("agent".into(), agent);
    let bytes = rehash_bundle(&mut doc);
    assert!(matches!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)),
        Err(BundleError::PreparedDecode { detail }) if detail.contains("unknown field `agent`")
    ));

    // 2. Content tampering with recomputed outer digest.
    let mut doc = base.clone();
    doc["agents"][0]["prompt"] = serde_json::json!("tampered prompt");
    let bytes = rehash_bundle(&mut doc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::PreparedContentDigestMismatch {
            bundle_id: "hya/alpha".into(),
            source_path: "prompts/lead.md".into(),
        })
    );

    // 3. Unknown bundle collections.
    for field in ["files", "helpers", "dependencies", "imports"] {
        let mut doc = base.clone();
        doc[field] = serde_json::json!([]);
        let bytes = rehash_bundle(&mut doc);
        assert!(matches!(
            PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)),
            Err(BundleError::PreparedDecode { .. })
        ));
    }

    // 4. Noncanonical provenance path and reversed can_spawn vector.
    let skill = prepare_package(skill_source("resources/skills/docs.md")).expect("skill");
    let mut sdoc: serde_json::Value = serde_json::from_slice(skill.bytes()).expect("JSON");
    sdoc["skills"][0]["source_path"] = serde_json::json!("resources/./skills/docs.md");
    let bytes = rehash_bundle(&mut sdoc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::NonCanonicalPreparedCatalog)
    );

    let spawn = prepare_package(BundleSource::new(
        "spawn-order",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: hya/spawn-order, version: 1.0.0, publisher: hya }\nagents: [{ id: lead, role: main, can_spawn: [beta, alpha] }]\n".as_slice(),
        )],
    ))
    .expect("spawn");
    let mut spdoc: serde_json::Value = serde_json::from_slice(spawn.bytes()).expect("JSON");
    spdoc["agents"][0]["can_spawn"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let bytes = rehash_bundle(&mut spdoc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::NonCanonicalPreparedCatalog)
    );
}

#[test]
fn prepared_decode_revalidates_resource_and_hook_references() {
    let prepared =
        prepare_package(agent_source("alpha", "hya/alpha", "alpha", false)).expect("prep");
    let base: serde_json::Value = serde_json::from_slice(prepared.bytes()).expect("JSON");

    // Unknown resource_view allow target.
    let mut doc = base.clone();
    doc["agents"][0]["resource_view"]["allow"] =
        serde_json::json!(["bundle:hya/alpha/skill/missing"]);
    let bytes = rehash_bundle(&mut doc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".into(),
            kind: "resource".into(),
            reference: "bundle:hya/alpha/skill/missing".into(),
        })
    );

    // Harness-prefixed hook refs in hook_refs and resource_view.
    for raw_ref in [
        "harness:hook/event",
        "harness:hook/tool.execute.before",
        "harness:hook/tool.execute.after",
        "harness:hook/unknown",
        "harness:hook/",
        "harness:hook",
    ] {
        let mut doc = base.clone();
        doc["agents"][0]["hook_refs"] = serde_json::json!([raw_ref]);
        let bytes = rehash_bundle(&mut doc);
        assert_eq!(
            PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
            Some(BundleError::UnknownResourceReference {
                bundle_id: "hya/alpha".into(),
                kind: "resource".into(),
                reference: raw_ref.into(),
            })
        );
    }
    let mut doc = base;
    doc["agents"][0]["resource_view"]["allow"] = serde_json::json!(["harness:hook/event"]);
    let bytes = rehash_bundle(&mut doc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".into(),
            kind: "resource".into(),
            reference: "harness:hook/event".into(),
        })
    );

    // Unreferenced unsupported hook local_id rejected on decode.
    let hook_prep = prepare_package(BundleSource::new(
        "prepared-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/prepared-hook, version: 1.0.0, publisher: hya }\nresources:\n  hooks: [{ id: event, path: extensions/runtime.js }]\nextensions:\n  js: [{ id: runtime, path: extensions/runtime.js }]\nagents: [{ id: lead, role: main }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export const runtime = true;\n".as_slice()),
        ],
    ))
    .expect("hook prep");
    let mut hdoc: serde_json::Value = serde_json::from_slice(hook_prep.bytes()).expect("JSON");
    hdoc["hooks"][0]["local_id"] = serde_json::json!("audit");
    hdoc["hooks"][0]["stable_id"] = serde_json::json!("bundle:hya/prepared-hook/hook/audit");
    let bytes = rehash_bundle(&mut hdoc);
    assert_eq!(
        PreparedCatalog::decode(&bytes, &sha256_hex(&bytes)).err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/prepared-hook".into(),
            feature: "hook:audit".into(),
        })
    );
}

#[test]
fn manifest_schemas_prepare_round_trip_and_reject_invalid() {
    let prepared = prepare_package(schema_bundle(Some(
        "schemas:\n  - scheme: kv\n    tool: query\n    writable: true\n  - scheme: db\n    tool: query\n    writable: false\n",
    )))
    .expect("valid schemas");
    let rows = prepared.schemas();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]
            .schemas
            .iter()
            .map(|s| (s.scheme.as_str(), s.tool.as_str(), s.writable))
            .collect::<Vec<_>>(),
        [("db", "query", false), ("kv", "query", true)]
    );
    assert_eq!(prepared.bundle_schemas("hya/schema-demo").len(), 2);
    assert!(prepared.bundle_schemas("hya/other").is_empty());
    assert_eq!(
        PreparedCatalog::decode(prepared.bytes(), prepared.digest())
            .expect("decode")
            .schemas(),
        rows
    );

    let plain = prepare_package(schema_bundle(None)).expect("plain");
    assert!(plain.schemas().is_empty());
    let plain_doc: serde_json::Value = serde_json::from_slice(plain.bytes()).expect("JSON");
    assert!(plain_doc["schemas"].as_array().is_some_and(Vec::is_empty));
    PreparedCatalog::decode(plain.bytes(), plain.digest()).expect("plain decodes");

    // Missing tool, invalid/internal scheme tokens, duplicate scheme.
    assert!(matches!(
        prepare_package(schema_bundle(Some("schemas:\n  - scheme: db\n    tool: nosuch\n"))),
        Err(BundleError::InvalidManifest { source_name, detail })
            if source_name == "hya/schema-demo" && detail.contains("nosuch") && detail.contains("does not declare")
    ));
    for scheme in ["local", "artifact", "d", "has__sep", "no.dot"] {
        let block = format!("schemas:\n  - scheme: {scheme}\n    tool: query\n");
        assert!(matches!(
            prepare_package(schema_bundle(Some(&block))),
            Err(BundleError::InvalidManifest { source_name, detail })
                if source_name == "hya/schema-demo" && detail.contains(scheme)
        ));
    }
    assert!(matches!(
        prepare_package(schema_bundle(Some(
            "schemas:\n  - scheme: db\n    tool: query\n  - scheme: db\n    tool: query\n    writable: true\n"
        ))),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("more than once")
    ));
}

#[test]
fn manifest_extensions_process_prepare_round_trip_and_reject_invalid() {
    let prepared = prepare_package(process_bundle(Some(
        "    kind: bun\n    command: [bun, run, extensions/runtime.ts]\n",
    )))
    .expect("valid process");
    let process = prepared
        .bundle_process("hya/process-demo")
        .expect("process emitted");
    assert_eq!(process.kind.as_str(), "bun");
    assert_eq!(process.command, ["bun", "run", "extensions/runtime.ts"]);
    assert!(prepared.bundle_process("hya/other").is_none());
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode");
    assert_eq!(decoded.bundle_process("hya/process-demo"), Some(process));

    let plain = prepare_package(process_bundle(None)).expect("plain");
    assert!(plain.bundle_process("hya/process-demo").is_none());
    let doc: serde_json::Value = serde_json::from_slice(plain.bytes()).expect("JSON");
    assert!(doc.get("extensions_process").is_none());
    PreparedCatalog::decode(plain.bytes(), plain.digest()).expect("plain decodes");

    for command in ["[]", "[\"\"]", "[\"bun\", \"\"]"] {
        let block = format!("    kind: bun\n    command: {command}\n");
        assert!(matches!(
            prepare_package(process_bundle(Some(&block))),
            Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("command")
        ));
    }
    assert!(matches!(
        prepare_package(process_bundle(Some(
            "    kind: python\n    command: [python3, server.py]\n"
        ))),
        Err(BundleError::InvalidManifest { .. })
    ));
}

#[test]
fn manifest_mcp_resources_prepare_round_trip_and_validate_shape() {
    let prepared = prepare_package(mcp_bundle(
        "    - id: vecdb\n      path: mcp/vecdb.json\n    - id: remote\n      path: mcp/remote.json\n",
        &[
            ("mcp/vecdb.json", r#"{"command": ["python3", "vecdb.py"]}"#),
            ("mcp/remote.json", r#"{"url": "https://mcp.example/sse", "timeout_ms": 250}"#),
        ],
    ))
    .expect("valid mcp");
    let bundle = &prepared.bundles()[0];
    let rows: Vec<_> = bundle
        .mcp()
        .iter()
        .map(|r| {
            (
                r.local_id.as_str(),
                r.stable_id.as_str(),
                r.source_path.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "remote",
                "bundle:hya/mcp-demo/mcp/remote",
                "mcp/remote.json"
            ),
            ("vecdb", "bundle:hya/mcp-demo/mcp/vecdb", "mcp/vecdb.json"),
        ]
    );
    assert_eq!(
        bundle.mcp()[1].content,
        r#"{"command": ["python3", "vecdb.py"]}"#
    );
    assert_eq!(
        PreparedCatalog::decode(prepared.bytes(), prepared.digest())
            .expect("decode")
            .bundles()[0]
            .mcp(),
        bundle.mcp()
    );

    assert_eq!(
        prepare_package(mcp_bundle(
            "    - id: vecdb\n      path: mcp/vecdb.json\n",
            &[]
        ))
        .err(),
        Some(BundleError::MissingReference {
            bundle_id: "hya/mcp-demo".into(),
            path: "mcp/vecdb.json".into(),
        })
    );
    for content in [
        "command: python3",
        r#"{"env": {}}"#,
        r#"{"command": []}"#,
        r#"{"command": ["python3", ""]}"#,
        r#"{"command": ["x"], "args": [1]}"#,
        r#"{"command": "python3"}"#,
    ] {
        assert!(matches!(
            prepare_package(mcp_bundle(
                "    - id: vecdb\n      path: mcp/vecdb.json\n",
                &[("mcp/vecdb.json", content)],
            )),
            Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("mcp/vecdb.json")
        ));
    }
}

#[test]
fn manifest_self_check_prepares_with_default_timeout_and_rejects_invalid() {
    let prepared = prepare_package(BundleSource::new(
        "self-check",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: acme/check, version: 1.0.0, publisher: acme }\ncheck: { command: [echo, ok] }\n".as_slice(),
        )],
    ))
    .expect("valid self-check");
    let check = prepared.bundles()[0].check().expect("check present");
    assert_eq!(check.command, ["echo", "ok"]);
    assert_eq!(check.timeout_secs, 120);

    for check in ["{ command: [] }", "{ command: [echo], timeout_secs: 601 }"] {
        let src = BundleSource::new(
            "invalid-check",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nidentity: {{ id: acme/check, version: 1.0.0, publisher: acme }}\ncheck: {check}\n"
                ),
            )],
        );
        assert!(matches!(
            prepare_package(src),
            Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("check.")
        ));
    }
}

#[test]
fn single_markdown_bundle_filename_markers_and_explicit_prompt() {
    let markers = "kind: Bundle\nidentity:\n  id: hya/markdown\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: markdown-lead\n    role: main";
    let md = |fm: &str, name: &str| {
        prepare_package(BundleSource::new(
            "markdown",
            vec![SourceFile::new(
                name,
                format!("---\n{fm}\n---\nYou are the Markdown lead.\n"),
            )],
        ))
    };

    let valid = md(markers, "bundle.hya.md").expect("valid markdown");
    assert_eq!(
        valid.bundles()[0].agents()[0].prompt.as_deref(),
        Some("You are the Markdown lead.")
    );
    assert!(matches!(
        md(markers, "agent.md"),
        Err(BundleError::UnsupportedSource { .. })
    ));
    assert!(matches!(
        md(&format!("api_version: hya.agent-bundle/v1\n{markers}"), "bundle.hya.md"),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("api_version")
    ));
    assert!(matches!(
        md(&markers.replace("kind: Bundle\n", ""), "bundle.hya.md"),
        Err(BundleError::InvalidManifest { .. })
    ));

    // Empty body alongside explicit `prompt:` file uses the named file.
    let explicit = prepare_package(BundleSource::new(
        "markdown-explicit",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                b"---\nkind: Bundle\nidentity: { id: hya/markdown-explicit-prompt, version: 1.0.0, publisher: hya }\nagents: [{ id: markdown-alpha, role: main, prompt: prompts/alpha.md }]\n---\n\n".as_slice(),
            ),
            SourceFile::new("prompts/alpha.md", b"Alpha prompt.".as_slice()),
        ],
    ))
    .expect("explicit prompt");
    let agent = &explicit.bundles()[0].agents()[0];
    assert_eq!(agent.id.as_str(), "markdown-alpha");
    assert_eq!(agent.prompt.as_deref(), Some("Alpha prompt."));
    assert_eq!(agent.prompt_source.as_deref(), Some("prompts/alpha.md"));
}

#[test]
fn agent_set_determinism_private_flag_and_rejections() {
    let src = |agents: &str, extra: &str| {
        BundleSource::new(
            "agent-set",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nidentity: {{ id: acme/team, version: 1.0.0, publisher: acme }}\nagents: {agents}\n{extra}\n"
                ),
            )],
        )
    };

    let first = src(
        "[{id: beta, role: subagent}, {id: alpha, role: main, can_spawn: [beta, external, beta]}]",
        "",
    );
    let second = src(
        "[{id: alpha, role: main, can_spawn: [external, beta]}, {id: beta, role: subagent}]",
        "",
    );
    let prepared = prepare_package(first.clone()).expect("first");
    assert_eq!(
        prepared.bytes(),
        prepare_package(second).expect("second").bytes()
    );
    let pkg = write_public_package(&first).expect("pkg");
    assert_eq!(pkg, write_public_package(&first).expect("pkg2"));
    let inspected = inspect_public_package(&pkg).expect("inspect");
    assert_eq!(prepared.bytes(), inspected.bytes());
    let decoded = PreparedCatalog::decode(inspected.bytes(), inspected.digest()).expect("decode");
    let catalog = BundleCatalog::from_verified_catalogs(&[&decoded]).expect("catalog");
    for id in ["alpha", "beta"] {
        assert_eq!(
            catalog.resolve_agent(id),
            catalog.resolve_agent(&format!("bundle:acme/team/agent/{id}"))
        );
    }
    assert!(decoded.index()[0].workflow_ids.is_empty());
    assert!(decoded.bundles()[0].workflow().is_none());
    assert!(catalog.semantic_identity_v1().is_some());

    // Private flag defaults to public and round-trips through packages.
    let pub_prep = prepare_package(src("[{id: worker, role: subagent}]", "")).expect("pub");
    assert!(!pub_prep.bundles()[0].agents()[0].private);
    assert_eq!(
        pub_prep.bytes(),
        prepare_package(src("[{id: worker, role: subagent, private: false}]", ""))
            .expect("pub2")
            .bytes()
    );
    let priv_src = src("[{id: worker, role: subagent, private: true}]", "");
    let priv_prep = prepare_package(priv_src.clone()).expect("priv");
    assert_ne!(pub_prep.digest(), priv_prep.digest());
    let priv_ins =
        inspect_public_package(&write_public_package(&priv_src).expect("pkg")).expect("ins");
    assert!(
        PreparedCatalog::decode(priv_ins.bytes(), priv_ins.digest())
            .expect("dec")
            .bundles()[0]
            .agents()[0]
            .private
    );

    for (agents, extra) in [
        ("[{id: same, role: main}, {id: same, role: subagent}]", ""),
        (
            "[{id: member, role: main}]",
            "workflow: {id: demo, path: demo.md}",
        ),
        ("[{id: member, role: main, harness_access: full}]", ""),
        ("[{id: member, role: main, prompt: missing.md}]", ""),
        ("[{id: worker, role: subagent, private: wrong}]", ""),
    ] {
        assert!(
            prepare_package(src(agents, extra)).is_err(),
            "{agents} / {extra}"
        );
    }
}

#[test]
fn agent_channels_prepare_validate_reject_forged_and_first_party_preset() {
    let ch_src = |channels: &str, agents: &str| {
        BundleSource::new(
            "channels",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nidentity: {{ id: hya/agent-channels, version: 1.0.0, publisher: hya }}\nagents: {agents}\nchannels:\n{channels}\n"
                ),
            )],
        )
    };

    let channels = "  - { id: unit-default, kind: unit, participants: [{ kind: role, role: direct_reports }, { kind: role, role: unit_leader }], capabilities: [steer, send, resident_mail, follow_up], scope: unit, retention: team_session }\n  - { id: parent-dm-default, kind: parent_dm, participants: [{ kind: role, role: child }, { kind: role, role: parent }], capabilities: [steer, send, report, resident_mail, follow_up], scope: vertical, retention: team_session }";
    let prepared = prepare_package(ch_src(channels, "[]")).expect("channels");
    let bundle = &prepared.bundles()[0];
    assert!(bundle.agents.is_empty() && bundle.channels.len() == 2);
    assert_eq!(bundle.channels[0].kind, ChannelTemplateKind::ParentDm);
    assert_eq!(bundle.channels[0].scope, ChannelScope::Vertical);
    assert_eq!(bundle.channels[0].capabilities[0], ChannelCapability::Send);
    assert_eq!(
        bundle.channels[0].participants[0],
        PreparedChannelParticipant::Role {
            role: ChannelParticipantRole::Parent
        }
    );
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode");
    let catalog = BundleCatalog::from_prepared(decoded.bundles()).expect("catalog");
    assert_eq!(catalog.channels_for_bundle("hya/agent-channels").len(), 2);

    // Empty bundle without agents or channels prepares.
    assert!(prepare_package(BundleSource::new(
        "empty",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: acme/empty, version: 1.0.0, publisher: acme }\nagents: []\n".as_slice(),
        )],
    ))
    .is_ok());

    // Invalid channel declarations rejected.
    for (ch, agents) in [
        (
            "  - { id: same, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }\n  - { id: same, kind: unit, participants: [{kind: role, role: direct_reports}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: one, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }\n  - { id: two, kind: unit, participants: [{kind: role, role: direct_reports}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: broadcast, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: parent}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: parent_dm, participants: [{kind: role, role: parent}], capabilities: [send, send], scope: vertical, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: parent_dm, participants: [{kind: agent, agent: missing}], capabilities: [], scope: vertical, retention: team_session }",
            "[{id: actual, role: main}]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: vertical, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: forever }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session, surprise: true }",
            "[]",
        ),
    ] {
        assert!(
            prepare_package(ch_src(ch, agents)).is_err(),
            "accepted {ch}"
        );
    }

    // Forged noncanonical capabilities rejected on decode.
    let single = prepare_package(ch_src(
        "  - { id: unit, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [send], scope: unit, retention: team_session }",
        "[]",
    ))
    .expect("single");
    let mut doc: serde_json::Value = serde_json::from_slice(single.bytes()).expect("JSON");
    doc["channels"][0]["capabilities"] = serde_json::json!(["send", "send"]);
    let forged = serde_json::to_vec(&doc).expect("encode");
    assert!(PreparedCatalog::decode(&forged, &sha256_hex(&forged)).is_err());

    // First-party agent-channels preset check.
    let preset_src = BundleSource::read_directory(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundles/builtin/agent-channels"),
    )
    .expect("read preset");
    let preset = prepare_package(preset_src).expect("prepare preset");
    assert_eq!(preset.bundles()[0].identity.id, "hya/agent-channels");
    assert!(preset.bundles()[0].agents.is_empty());
    assert_eq!(preset.bundles()[0].channels.len(), 2);
}

#[test]
fn manifest_fields_advertise_timeout_handoff_and_legacy_bytes_parity() {
    // Legacy bytes & digest regression guard.
    let legacy = prepare_package(legacy_fields_source("", "")).expect("legacy");
    assert_eq!(
        legacy.bytes(),
        include_bytes!("../fixtures/legacy_manifest.json")
    );
    assert_eq!(
        legacy.digest(),
        "c1d2fd35397977b19174e2360fd9895163a68919ed368964a7cd5c452b5e042d"
    );
    let dec = PreparedCatalog::decode(legacy.bytes(), legacy.digest()).expect("legacy dec");
    assert!(dec.bundles()[0].agents()[0].advertise.is_empty());
    assert_eq!(
        dec.bundle_process("acme/legacy").expect("proc").timeout_ms,
        None
    );
    assert_eq!(dec.bundles()[0].agents()[0].handoff, HandoffPolicy::Model);
    assert_eq!(
        legacy.bytes(),
        prepare_package(legacy_fields_source(", advertise: []", ""))
            .expect("empty adv")
            .bytes()
    );
    assert_eq!(
        legacy.bytes(),
        prepare_package(legacy_fields_source(", handoff: model", ""))
            .expect("model handoff")
            .bytes()
    );

    // Valid advertise, timeout_ms bounds, and deterministic handoff round-trip.
    for timeout in [1000, 3_600_000] {
        let prep = prepare_package(legacy_fields_source(
            ", advertise: [read, bash, read], handoff: deterministic",
            &format!(", timeout_ms: {timeout}"),
        ))
        .expect("valid fields");
        let dec = PreparedCatalog::decode(prep.bytes(), prep.digest()).expect("decode");
        assert_eq!(dec.bundles()[0].agents()[0].advertise, ["bash", "read"]);
        assert_eq!(
            dec.bundles()[0].agents()[0].handoff,
            HandoffPolicy::Deterministic
        );
        assert_eq!(
            dec.bundle_process("acme/legacy").expect("proc").timeout_ms,
            Some(timeout)
        );
        assert_eq!(dec.bytes(), prep.bytes());
    }

    // Rejections for invalid timeout_ms, advertise, and handoff.
    for val in ["0", "999", "3600001", "-1", "1.5", "'1000'", "[]"] {
        let err = prepare_package(legacy_fields_source("", &format!(", timeout_ms: {val}")))
            .expect_err("bad timeout");
        assert!(
            matches!(err, BundleError::InvalidManifest { .. })
                && err.to_string().contains("timeout_ms")
        );
    }
    for val in ["bash", "{}", "[{}]", "[[]]", "[1]", "[true]"] {
        let err = prepare_package(legacy_fields_source(&format!(", advertise: {val}"), ""))
            .expect_err("bad advertise");
        assert!(
            matches!(err, BundleError::InvalidManifest { .. })
                && err.to_string().contains("advertise")
        );
    }
    for val in ["summarizer", "Deterministic", "''", "[]", "1"] {
        let err = prepare_package(legacy_fields_source(&format!(", handoff: {val}"), ""))
            .expect_err("bad handoff");
        assert!(
            matches!(err, BundleError::InvalidManifest { .. })
                && err.to_string().contains("handoff")
        );
    }
}

#[test]
fn process_resources_executables_libraries_and_support_files_round_trip() {
    // 1. Process hooks + declared support files.
    let proc_src = BundleSource::new(
        "process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: acme/process, version: 1.0.0, publisher: acme }\nextensions:\n  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\n  files: [{ id: runtime, path: runtime.py }]\nresources:\n  tools: [{ id: echo, path: declarations.json }]\n  hooks:\n    - { id: goal.evaluate, path: declarations.json }\n    - { id: message.user.before, path: declarations.json }\n".as_slice(),
            ),
            SourceFile::new("runtime.py", b"print('fixture')\n".as_slice()),
            SourceFile::new("declarations.json", b"{}".as_slice()),
        ],
    );
    let prep = prepare_package(proc_src.clone()).expect("process closure");
    assert_eq!(prep.bundles()[0].extensions().len(), 1);
    assert_eq!(prep.bundles()[0].hooks().len(), 2);
    PreparedCatalog::decode(prep.bytes(), prep.digest()).expect("decode");
    write_public_package(&proc_src).expect("package");

    // 2. Inert static support files without process.
    let orphan = prepare_package(BundleSource::new(
        "orphan",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: acme/orphan, version: 1.0.0, publisher: acme }\nextensions:\n  files: [{ id: orphan, path: orphan.py }]\n".as_slice(),
            ),
            SourceFile::new("orphan.py", b"print('unused')\n".as_slice()),
        ],
    ))
    .expect("inert support file");
    assert!(orphan.process_extensions().is_empty() && orphan.bundles()[0].tools().is_empty());
    assert_eq!(orphan.bundles()[0].extensions()[0].source_path, "orphan.py");

    // 3. Non-UTF8 native executable & standalone native library round-trip.
    let elf = vec![0x7f, b'E', b'L', b'F', 0, 0xff, 0x80];
    let exec_src = BundleSource::new(
        "native-exec",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: acme/native, version: 1.0.0, publisher: acme }\nextensions:\n  rust: [{ id: provider, path: bin/provider }]\n  process: { kind: rust, command: ['${BUNDLE_ROOT}/bin/provider'] }\nresources:\n  tools: [{ id: echo, path: declarations.json }]\n".as_slice(),
            ),
            SourceFile::new("bin/provider", elf.clone()),
            SourceFile::new("declarations.json", b"{}".as_slice()),
        ],
    );
    let exec_prep = prepare_package(exec_src.clone()).expect("native exec");
    assert_eq!(
        exec_prep.bundles()[0].extensions()[0]
            .source_bytes()
            .expect("bytes"),
        elf
    );
    assert_eq!(
        inspect_public_package(&write_public_package(&exec_src).expect("pkg"))
            .expect("ins")
            .bytes(),
        exec_prep.bytes()
    );

    let dylib = vec![0xcf, 0xfa, 0xed, 0xfe, 0, 0xff, 0x80];
    let lib_src = BundleSource::new(
        "native-lib",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: hya/network-tools, version: 1.0.0, publisher: hya }\nextensions:\n  libraries: [{ id: runtime, path: native/libhya_network_tools.dylib }]\n".as_slice(),
            ),
            SourceFile::new("native/libhya_network_tools.dylib", dylib.clone()),
        ],
    );
    let lib_prep = prepare_package(lib_src.clone()).expect("native lib");
    assert!(lib_prep.process_extensions().is_empty());
    assert!(
        lib_prep.bundles()[0].extensions()[0]
            .stable_id
            .contains("/library/runtime")
    );
    assert_eq!(
        lib_prep.bundles()[0].extensions()[0]
            .source_bytes()
            .expect("bytes"),
        dylib
    );
    assert_eq!(
        inspect_public_package(&write_public_package(&lib_src).expect("pkg"))
            .expect("ins")
            .bytes(),
        lib_prep.bytes()
    );
}

#[test]
fn library_and_process_extensions_sort_by_local_id_and_admit_router_hooks() {
    for (with_proc, kind) in [
        (false, "files"),
        (true, "files"),
        (true, "js"),
        (true, "rust"),
    ] {
        let proc_line = if with_proc {
            "  process: { kind: rust, command: ['${BUNDLE_ROOT}/types.bin'] }\n"
        } else {
            ""
        };
        let src = BundleSource::new(
            "lib-order",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    format!(
                        "kind: Bundle\nidentity: {{ id: hya/network-tools, version: 1.0.0, publisher: hya }}\nextensions:\n{proc_line}  {kind}: [{{ id: types, path: types.bin }}]\n  libraries: [{{ id: runtime, path: native/libhya_network_tools.so }}]\n"
                    ),
                ),
                SourceFile::new("types.bin", b"{}".as_slice()),
                SourceFile::new("native/libhya_network_tools.so", vec![0, 0xff, 0x80]),
            ],
        );
        let prep = prepare_package(src.clone()).expect("prep lib+ext");
        assert_eq!(
            prep.bundles()[0]
                .extensions()
                .iter()
                .map(|r| r.local_id.as_str())
                .collect::<Vec<_>>(),
            ["runtime", "types"],
            "{with_proc}/{kind}"
        );
        assert_eq!(
            inspect_public_package(&write_public_package(&src).expect("pkg"))
                .expect("ins")
                .bytes(),
            prep.bytes()
        );
    }

    // Process-backed plugins may declare chat.params and model.fallback; implicit JS rejects them.
    let router_src = |process: &str| {
        BundleSource::new(
            "router",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    format!(
                        "kind: Bundle\nidentity: {{ id: acme/router, version: 1.0.0, publisher: acme }}\nextensions:\n{process}  js: [{{ id: runtime, path: runtime.js }}]\nresources:\n  hooks:\n    - {{ id: chat.params, path: runtime.js }}\n    - {{ id: model.fallback, path: runtime.js }}\n"
                    ),
                ),
                SourceFile::new("runtime.js", b"export default {}\n".as_slice()),
            ],
        )
    };
    let ok = prepare_package(router_src(
        "  process: { kind: bun, command: [bun, '${BUNDLE_ROOT}/runtime.js'] }\n",
    ))
    .expect("process router");
    assert_eq!(
        ok.bundles()[0]
            .hooks()
            .iter()
            .map(|h| h.local_id.as_str())
            .collect::<Vec<_>>(),
        ["chat.params", "model.fallback"]
    );
    assert!(prepare_package(router_src("")).is_err());
}

#[test]
fn tui_manifest_validation_canonicalization_and_compatibility() {
    let tui_src = |tui: &str, entry_path: &str| {
        let manifest = format!(
            "kind: Bundle\nidentity: {{ id: hya/tui-test, version: 1.0.0, publisher: hya }}\nagents: [{{ id: tui-test, role: main, prompt: prompts/lead.md }}]\nextensions:\n  js: [{{ id: entry, path: {entry_path} }}]\n  process: {{ kind: bun, command: [bun, run, '{entry_path}'] }}\ntui:\n{tui}"
        );
        BundleSource::new(
            "tui-test",
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new("prompts/lead.md", b"TUI agent\n".as_slice()),
                SourceFile::new(entry_path, b"export default {};".as_slice()),
            ],
        )
    };
    let block = |entry: &str, sdk: &str, perms: &str| {
        format!("  api_version: 1\n  entry: {entry}\n  sdk: {sdk}\n  permissions: {perms}\n")
    };

    // Absent tui section remains compatible.
    assert!(
        prepare_package(tui_src("", "entry.ts"))
            .expect("no tui")
            .bundles()[0]
            .tui()
            .is_none()
    );

    // Supported SDK versions + canonicalization of entry path & sorted permissions.
    for sdk in ["1.0", "1.2.3"] {
        let prep = prepare_package(tui_src(
            &block("./entry.ts", sdk, "[tui.render, tui.panel]"),
            "entry.ts",
        ))
        .expect("valid tui");
        let tui = prep.bundles()[0].tui().expect("tui present");
        assert_eq!(tui.entry, "entry.ts");
        assert_eq!(tui.permissions, ["tui.panel", "tui.render"]);
    }

    // Invalid api_version, sdk, entry, and permissions.
    for (tui, file, needle) in [
        (
            block("entry.ts", "1.2.3", "[]").replacen("api_version: 1", "api_version: 2", 1),
            "entry.ts",
            "tui.",
        ),
        (block("entry.ts", "2.0", "[]"), "entry.ts", "tui."),
        (block("missing.ts", "1.0", "[]"), "entry.ts", "tui.entry"),
        (block("entry.txt", "1.0", "[]"), "entry.txt", "tui.entry"),
        (block("entry.ts", "1.0", "[]"), "other.ts", "tui.entry"),
        (
            block("entry.ts", "1.0", "[tui.unknown]"),
            "entry.ts",
            "tui.permissions",
        ),
        (
            block("entry.ts", "1.0", "[tui.panel, tui.panel]"),
            "entry.ts",
            "tui.permissions",
        ),
    ] {
        assert!(matches!(
            prepare_package(tui_src(&tui, file)),
            Err(BundleError::InvalidManifest { detail, .. }) if detail.contains(needle)
        ));
    }
}

#[test]
fn resolves_hya_and_legacy_backend_version_references_in_bundle_source() {
    for reference in ["hya", "backend"] {
        let prepared = prepare_package(BundleSource::new(
            "version-reference",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nversion_ref: {reference}\nidentity:\n  id: hya/version-reference\n  version: 0.0.0\n  publisher: hya\n"
                ),
            )],
        ))
        .expect("version reference resolves");
        assert_eq!(prepared.bundles()[0].identity().version, VERSION);
    }
}
