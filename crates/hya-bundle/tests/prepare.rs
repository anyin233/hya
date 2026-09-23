//! Source preparation, digesting, and rehashing of bundles.

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package,
};
use sha2::{Digest, Sha256};

fn source(root: &str, bundle_id: &str, stable_id: &str, reverse_files: bool) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
agent:
  id: {stable_id}
  role: main
  prompt: prompts/lead.md
"#,
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

#[test]
#[allow(clippy::expect_used)]
fn agent_set_bundle_prepares_multiple_agents_as_one_closed_payload() {
    let manifest = br#"kind: AgentSetBundle
identity:
  id: hya/agent-set
  version: 1.0.0
  publisher: hya
agents:
  - id: alpha
    role: main
    prompt: prompts/alpha.md
  - id: beta
    role: subagent
    prompt: prompts/beta.md
"#;
    let prepared = prepare_package(BundleSource::new(
        "agent-set",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("prompts/alpha.md", b"Alpha agent.\n".as_slice()),
            SourceFile::new("prompts/beta.md", b"Beta agent.\n".as_slice()),
        ],
    ))
    .expect("AgentSetBundle should prepare");

    let document: serde_json::Value =
        serde_json::from_slice(prepared.bytes()).expect("prepared catalog must be JSON");
    assert_eq!(document["bundles"][0]["kind"], "AgentSetBundle");
    assert_eq!(
        document["bundles"][0]["agents"]
            .as_array()
            .expect("AgentSetBundle must carry an agents array")
            .len(),
        2
    );

    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())
        .expect("AgentSetBundle prepared bytes must round-trip");
    let catalog = BundleCatalog::from_prepared(decoded.bundles())
        .expect("AgentSetBundle must publish all agents to the catalog");
    assert_eq!(
        catalog
            .resolve_agent("alpha")
            .map(|agent| agent.id.as_str()),
        Some("alpha")
    );
    assert_eq!(
        catalog.resolve_agent("beta").map(|agent| agent.id.as_str()),
        Some("beta")
    );
}

#[test]
#[allow(clippy::expect_used)]
fn plugin_bundle_prepares_without_agents() {
    let source = BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Plugin
identity: { id: hya/plugins-demo, version: 1.0.0, publisher: hya }
"#,
        )],
    );
    let prepared = prepare_package(source).expect("Plugin should prepare");
    assert_eq!(prepared.bundles()[0].kind().as_str(), "Plugin");
    assert!(prepared.bundles()[0].agents().is_empty());
}

#[test]
#[allow(clippy::expect_used)]
fn plugin_bundle_round_trips_static_skill_and_rejects_agent_fields() {
    let manifest = br#"kind: Plugin
identity: { id: hya/plugins-demo, version: 1.0.0, publisher: hya }
resources:
  skills:
    - id: guide
      path: skills/guide.md
"#;
    let prepared = prepare_package(BundleSource::new(
        "plugin-skill",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("skills/guide.md", b"Use the guide.\n".as_slice()),
        ],
    ))
    .expect("Plugin skill should prepare");
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())
        .expect("Plugin prepared bytes should decode");
    let plugin = decoded.bundles()[0]
        .plugin_bundle()
        .expect("decoded payload should be Plugin");
    assert_eq!(plugin.skills.len(), 1);
    assert_eq!(plugin.skills[0].local_id, "guide");

    for key in ["agent", "agents", "workflow", "channels"] {
        let text = format!(
            "kind: Plugin\nidentity: {{ id: hya/plugins-demo, version: 1.0.0, publisher: hya }}\n{key}: {{}}\n"
        );
        let result = prepare_package(BundleSource::new(
            "plugin-invalid",
            vec![SourceFile::new("bundle.yaml", text.into_bytes())],
        ));
        assert!(result.is_err(), "Plugin must reject `{key}`");
    }
}

#[test]
#[allow(clippy::expect_used)]
fn plugin_and_agent_bundles_coexist_in_one_catalog() {
    let plugin = prepare_package(BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Plugin\nidentity: { id: hya/plugin-pack, version: 1.0.0, publisher: hya }\n",
        )],
    ))
    .expect("Plugin should prepare");
    let agent = prepare_package(BundleSource::new(
        "agent",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: AgentBundle\nidentity: { id: hya/agent-pack, version: 1.0.0, publisher: hya }\nagent: { id: lead, role: main }\n",
        )],
    ))
    .expect("Agent should prepare");
    let mut bundles = plugin.bundles().to_vec();
    bundles.extend(agent.bundles().iter().cloned());
    let catalog = BundleCatalog::from_prepared(&bundles).expect("mixed catalog should prepare");
    assert_eq!(catalog.bundles().len(), 2);
}

#[test]
fn plugin_rejects_static_rust_extensions() {
    let source = BundleSource::new(
        "plugin-rust",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Plugin
identity: { id: hya/plugins-rust, version: 1.0.0, publisher: hya }
extensions:
  rust:
    - id: runtime
      path: runtime.rs
"#,
        )],
    );
    assert!(matches!(
        prepare_package(source),
        Err(BundleError::UnsupportedBundleFeature { feature, .. }) if feature == "extensions.rust"
    ));
}

#[test]
#[allow(clippy::expect_used)]
fn agent_set_prepared_decode_rejects_an_empty_roster_with_valid_digests() {
    let source = BundleSource::new(
        "empty-roster",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: AgentSetBundle
identity: { id: hya/set, version: 1.0.0, publisher: hya }
agents: [{ id: member, role: main }]
"#,
        )],
    );
    let prepared = prepare_package(source).expect("prepare agent set");
    let mut document: serde_json::Value = serde_json::from_slice(prepared.bytes()).expect("JSON");
    document["bundles"][0]["agents"] = serde_json::json!([]);
    document["index"][0]["agent_ids"] = serde_json::json!([]);
    let bytes = rehash_first_bundle(&mut document);
    assert!(matches!(
        PreparedCatalog::decode(&bytes, &digest(&bytes)),
        Err(BundleError::NonCanonicalPreparedCatalog)
    ));
}

/// Build one minimal WorkflowBundle source with an optional unreachable Agent.
fn workflow_source(include_orphan: bool) -> BundleSource {
    let manifest = format!(
        r#"kind: WorkflowBundle
identity:
  id: hya/workflow-demo
  version: 1.0.0
  publisher: hya
workflow:
  id: demo
  path: workflows/demo.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
{}
"#,
        if include_orphan {
            "  - id: orphan\n    role: subagent\n    prompt: prompts/orphan.md"
        } else {
            ""
        }
    );
    let workflow = br#"---
kind: Workflow
name: demo
description: One packaged Workflow.
nodes:
  execute:
    agent: worker
    directive: Execute the request.
---
flowchart TD
  execute
"#;
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new("workflows/demo.hya.md", workflow.to_vec()),
        SourceFile::new("prompts/worker.md", b"Execute carefully.\n".to_vec()),
    ];
    if include_orphan {
        files.push(SourceFile::new(
            "prompts/orphan.md",
            b"This Agent is unreachable.\n".to_vec(),
        ));
    }
    BundleSource::new("workflow-source", files)
}
/// Build a WorkflowBundle whose worker and verifier declare model routes.
fn workflow_source_with_model_routes() -> BundleSource {
    let manifest = br#"kind: WorkflowBundle
identity:
  id: hya/workflow-model-routes
  version: 1.0.0
  publisher: hya
workflow:
  id: routed
  path: workflows/routed.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
  - id: verifier
    role: subagent
    prompt: prompts/verifier.md
"#;
    let workflow = br#"---
kind: Workflow
name: routed
description: Model-routed packaged Workflow.
nodes:
  execute:
    agent: worker
    directive: Execute the request.
    mode: loop
    model:
      id: fake/worker-primary
      reasoning: high
      fallback:
        - id: fake/worker-fallback
          reasoning: medium
    verify:
      agent: verifier
      until: the result is valid
      max_iterations: 2
      model:
        id: fake/worker-primary
        reasoning: low
---
flowchart TD
  execute
"#;
    BundleSource::new(
        "workflow-model-routes",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("workflows/routed.hya.md", workflow.as_slice()),
            SourceFile::new("prompts/worker.md", b"Execute carefully.\n".as_slice()),
            SourceFile::new("prompts/verifier.md", b"Verify carefully.\n".as_slice()),
        ],
    )
}

/// Build a WorkflowBundle whose compiled stage points at the requested Agent.
fn workflow_source_with_stage_agent(stage_agent: &str) -> BundleSource {
    let manifest = br#"kind: WorkflowBundle
identity:
  id: hya/workflow-missing-agent
  version: 1.0.0
  publisher: hya
workflow:
  id: demo
  path: workflows/demo.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
"#;
    let workflow = format!(
        r#"---
kind: Workflow
name: demo
description: Missing Agent Workflow.
nodes:
  execute:
    agent: {stage_agent}
    directive: Execute the request.
---
flowchart TD
  execute
"#
    );
    BundleSource::new(
        "workflow-missing-agent",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("workflows/demo.hya.md", workflow.into_bytes()),
            SourceFile::new("prompts/worker.md", b"Execute carefully.".as_slice()),
        ],
    )
}

/// Build a WorkflowBundle with a verifier and a transitive Agent spawn edge.
fn workflow_source_with_verifier_closure(
    include_verifier: bool,
    include_helper: bool,
) -> BundleSource {
    let verifier = include_verifier
        .then_some("  - id: verifier\n    role: subagent\n    prompt: prompts/verifier.md\n");
    let helper = include_helper
        .then_some("  - id: helper\n    role: subagent\n    prompt: prompts/helper.md\n");
    let manifest = format!(
        r#"kind: WorkflowBundle
identity:
  id: hya/workflow-closure
  version: 1.0.0
  publisher: hya
workflow:
  id: closure
  path: workflows/closure.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
    can_spawn:
      - helper
{}{}"#,
        verifier.unwrap_or_default(),
        helper.unwrap_or_default(),
    );
    let workflow = br#"---
kind: Workflow
name: closure
description: Verifier and helper Workflow.
nodes:
  execute:
    agent: worker
    directive: Execute the request.
    mode: loop
    verify:
      agent: verifier
      until: the result is valid
      max_iterations: 2
---
flowchart TD
  execute
"#;
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new("workflows/closure.hya.md", workflow.as_slice()),
        SourceFile::new("prompts/worker.md", b"Execute carefully.".as_slice()),
    ];
    if include_verifier {
        files.push(SourceFile::new(
            "prompts/verifier.md",
            b"Verify the result.".as_slice(),
        ));
    }
    if include_helper {
        files.push(SourceFile::new(
            "prompts/helper.md",
            b"Help the worker.".as_slice(),
        ));
    }
    BundleSource::new("workflow-closure", files)
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

fn rehash_first_bundle(document: &mut serde_json::Value) -> Vec<u8> {
    let Some(bundle) = document["bundles"].get_mut(0) else {
        panic!("prepared bundle missing");
    };
    let mut digest_input = bundle.clone();
    let Some(fields) = digest_input.as_object_mut() else {
        panic!("prepared bundle is not an object");
    };
    fields.remove("digest");
    let digest_input = serde_json::to_vec(&digest_input);
    let Ok(digest_input) = digest_input else {
        panic!("bundle digest input failed: {digest_input:?}");
    };
    let bundle_digest = digest(&digest_input);
    bundle["digest"] = serde_json::Value::String(bundle_digest.clone());
    document["index"][0]["digest"] = serde_json::Value::String(bundle_digest);
    let bytes = serde_json::to_vec(document);
    let Ok(bytes) = bytes else {
        panic!("prepared document encode failed: {bytes:?}");
    };
    bytes
}

#[test]
fn deterministic_prepare_is_independent_of_source_file_order() {
    // One package prepares one bundle, so only file order within the source can
    // vary. The prepared bytes must not depend on it.
    let forward = prepare_package(source("a-source", "hya/alpha", "alpha", false));
    let Ok(forward) = forward else {
        panic!("forward preparation failed: {forward:?}");
    };

    let reverse = prepare_package(source("a-source", "hya/alpha", "alpha", true));
    let Ok(reverse) = reverse else {
        panic!("reverse preparation failed: {reverse:?}");
    };

    assert_eq!(forward.bytes(), reverse.bytes());
    assert_eq!(forward.digest(), reverse.digest());
    assert_eq!(
        forward
            .index()
            .iter()
            .map(|entry| entry.bundle_id.as_str())
            .collect::<Vec<_>>(),
        ["hya/alpha"]
    );
}

/// WorkflowBundle preparation emits canonical v2 bytes with one closed payload kind.
#[test]
fn workflow_bundle_prepares_compiled_agent_closure_as_v2() {
    let prepared = prepare_package(workflow_source(false));
    let Ok(prepared) = prepared else {
        panic!("WorkflowBundle preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(document) = document else {
        panic!("prepared WorkflowBundle was not JSON: {document:?}");
    };

    assert_eq!(document["format_version"], 2);
    assert_eq!(document["bundles"][0]["kind"], "WorkflowBundle");
    assert_eq!(document["bundles"][0]["workflow"]["id"], "demo");
    assert_eq!(document["bundles"][0]["agents"][0]["id"], "worker");
    assert_eq!(
        document["bundles"][0]["workflow"]["compiler_revision"],
        "2ce247d3aaf2ecf6de48121779abf0056c467b0c1a01775cfdf1011108a76797"
    );
}

/// Model-routed WorkflowBundles retain their source contract in prepared v2.
#[test]
fn workflow_bundle_prepares_model_routes_without_format_bump() {
    let prepared = prepare_package(workflow_source_with_model_routes());
    let Ok(prepared) = prepared else {
        panic!("model-routed WorkflowBundle preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(document) = document else {
        panic!("prepared model-routed WorkflowBundle was not JSON: {document:?}");
    };

    assert_eq!(document["format_version"], 2);
    assert_eq!(document["bundles"][0]["format_version"], 2);
    let source = document["bundles"][0]["workflow"]["source"]
        .as_str()
        .unwrap_or_default();
    assert!(source.contains("id: fake/worker-primary"));
    assert!(source.contains("id: fake/worker-fallback"));
    assert!(source.contains("reasoning: low"));
    assert_ne!(
        document["bundles"][0]["workflow"]["compiler_revision"], "",
        "route-bearing source must retain the common compiler revision"
    );
}

/// WorkflowBundle preparation rejects Agents outside the compiled reachable closure.
#[test]
fn workflow_bundle_rejects_unreachable_extra_agent() {
    let rejected = prepare_package(workflow_source(true));
    assert!(rejected.is_err(), "unreachable Agent must fail preparation");
}

#[test]
fn prepared_catalog_round_trips_and_rejects_tampered_bytes() {
    let prepared = prepare_package(source("alpha-source", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest());
    let Ok(decoded) = decoded else {
        panic!("prepared decode failed: {decoded:?}");
    };
    assert_eq!(decoded.bytes(), prepared.bytes());
    assert_eq!(decoded.bundles(), prepared.bundles());
    assert_eq!(decoded.index(), prepared.index());

    let mut tampered = prepared.bytes().to_vec();
    let needle = b"You are alpha.";
    let Some(offset) = tampered
        .windows(needle.len())
        .position(|window| window == needle)
    else {
        panic!("prepared prompt bytes missing");
    };
    tampered[offset] = b'X';
    let rejected = PreparedCatalog::decode(&tampered, prepared.digest());
    assert!(matches!(
        rejected,
        Err(BundleError::PreparedDigestMismatch { .. })
    ));
}

#[test]
fn prepared_decode_rejects_internally_consistent_noncanonical_vectors() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    let Some(bundle) = document["bundles"].get_mut(0) else {
        panic!("prepared bundle missing");
    };
    // Reintroduce a plural `agents` array: a decoder that still honoured it
    // would silently accept a multi-agent bundle.
    let agent = bundle["agent"].clone();
    let Some(fields) = bundle.as_object_mut() else {
        panic!("prepared bundle is not an object");
    };
    fields.insert("agents".to_string(), serde_json::json!([agent]));

    let mut digest_input = bundle.clone();
    let Some(fields) = digest_input.as_object_mut() else {
        panic!("prepared bundle is not an object");
    };
    fields.remove("digest");
    let digest_input = serde_json::to_vec(&digest_input);
    let Ok(digest_input) = digest_input else {
        panic!("bundle digest input failed: {digest_input:?}");
    };
    let bundle_digest = digest(&digest_input);
    bundle["digest"] = serde_json::Value::String(bundle_digest.clone());
    document["index"][0]["digest"] = serde_json::Value::String(bundle_digest);
    document["index"][0]["agent_ids"] = serde_json::json!(["alpha"]);

    let bytes = serde_json::to_vec(&document);
    let Ok(bytes) = bytes else {
        panic!("prepared document encode failed: {bytes:?}");
    };
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    let Some(BundleError::PreparedDecode { detail }) = result.err() else {
        panic!("a reintroduced `agents` array must be rejected at decode");
    };
    assert!(
        detail.contains("unknown field `agents`"),
        "decode must name the plural field it refuses: {detail}"
    );
}

#[test]
fn prepared_decode_rejects_content_with_recomputed_outer_digests() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["agent"]["prompt"] =
        serde_json::Value::String("tampered prompt".to_string());

    let Some(bundle) = document["bundles"].get_mut(0) else {
        panic!("prepared bundle missing");
    };
    let mut digest_input = bundle.clone();
    let Some(fields) = digest_input.as_object_mut() else {
        panic!("prepared bundle is not an object");
    };
    fields.remove("digest");
    let digest_input = serde_json::to_vec(&digest_input);
    let Ok(digest_input) = digest_input else {
        panic!("bundle digest input failed: {digest_input:?}");
    };
    let bundle_digest = digest(&digest_input);
    bundle["digest"] = serde_json::Value::String(bundle_digest.clone());
    document["index"][0]["digest"] = serde_json::Value::String(bundle_digest);

    let bytes = serde_json::to_vec(&document);
    let Ok(bytes) = bytes else {
        panic!("prepared document encode failed: {bytes:?}");
    };
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(
        result.err(),
        Some(BundleError::PreparedContentDigestMismatch {
            bundle_id: "hya/alpha".to_string(),
            source_path: "prompts/lead.md".to_string(),
        })
    );
}

#[test]
fn prepared_decode_rejects_noncanonical_provenance_paths() {
    let prepared = prepare_package(skill_source("resources/skills/docs.md"));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["skills"][0]["source_path"] =
        serde_json::Value::String("resources/./skills/docs.md".to_string());

    let Some(bundle) = document["bundles"].get_mut(0) else {
        panic!("prepared bundle missing");
    };
    let mut digest_input = bundle.clone();
    let Some(fields) = digest_input.as_object_mut() else {
        panic!("prepared bundle is not an object");
    };
    fields.remove("digest");
    let digest_input = serde_json::to_vec(&digest_input);
    let Ok(digest_input) = digest_input else {
        panic!("bundle digest input failed: {digest_input:?}");
    };
    let bundle_digest = digest(&digest_input);
    bundle["digest"] = serde_json::Value::String(bundle_digest.clone());
    document["index"][0]["digest"] = serde_json::Value::String(bundle_digest);

    let bytes = serde_json::to_vec(&document);
    let Ok(bytes) = bytes else {
        panic!("prepared document encode failed: {bytes:?}");
    };
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(result.err(), Some(BundleError::NonCanonicalPreparedCatalog));
}

#[test]
fn prepared_decode_rejects_noncanonical_resolved_reference_vectors() {
    let manifest = br#"kind: AgentBundle
identity:
  id: hya/spawn-order
  version: 1.0.0
  publisher: hya
agent:
  id: lead
  role: main
  can_spawn:
    - beta
    - alpha
"#;
    let prepared = prepare_package(BundleSource::new(
        "spawn-order",
        vec![SourceFile::new("bundle.yaml", manifest.as_slice())],
    ));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    let Some(can_spawn) = document["bundles"][0]["agent"]["can_spawn"].as_array_mut() else {
        panic!("prepared can_spawn missing");
    };
    can_spawn.reverse();

    let bytes = rehash_first_bundle(&mut document);
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(result.err(), Some(BundleError::NonCanonicalPreparedCatalog));
}

#[test]
fn prepared_decode_revalidates_full_catalog_references() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    // Resource references are still revalidated on decode; `can_spawn` is not,
    // because a cross-bundle target may legitimately not be installed.
    document["bundles"][0]["agent"]["resource_view"]["allow"] =
        serde_json::json!(["bundle:hya/alpha/skill/missing"]);
    let bytes = rehash_first_bundle(&mut document);

    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(
        result.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".to_string(),
            kind: "resource".to_string(),
            reference: "bundle:hya/alpha/skill/missing".to_string(),
        })
    );
}

#[test]
fn prepared_decode_rejects_unreferenced_unsupported_hook_local_id() {
    let manifest = br#"kind: AgentBundle
identity:
  id: hya/prepared-hook
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
  id: lead
  role: main
"#;
    let prepared = prepare_package(BundleSource::new(
        "prepared-hook",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    ));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["hooks"][0]["local_id"] = serde_json::Value::String("audit".to_string());
    document["bundles"][0]["hooks"][0]["stable_id"] =
        serde_json::Value::String("bundle:hya/prepared-hook/hook/audit".to_string());

    let bytes = rehash_first_bundle(&mut document);
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(
        result.err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/prepared-hook".to_string(),
            feature: "hook:audit".to_string(),
        })
    );
}

#[test]
fn prepared_decode_rejects_harness_prefixed_hook_refs_before_catalog_publication() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };

    for (raw_ref, kind) in [
        ("harness:hook/event", "resource"),
        ("harness:hook/tool.execute.before", "resource"),
        ("harness:hook/tool.execute.after", "resource"),
        ("harness:hook/unknown", "resource"),
        ("harness:hook/", "resource"),
        ("harness:hook", "resource"),
    ] {
        let mut document = document.clone();
        document["bundles"][0]["agent"]["hook_refs"] = serde_json::json!([raw_ref]);
        let bytes = rehash_first_bundle(&mut document);
        let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
        assert_eq!(
            result.err(),
            Some(BundleError::UnknownResourceReference {
                bundle_id: "hya/alpha".to_string(),
                kind: kind.to_string(),
                reference: raw_ref.to_string(),
            })
        );
    }
}

#[test]
fn prepared_decode_rejects_harness_prefixed_hook_resource_view_reference() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["agent"]["resource_view"]["allow"] =
        serde_json::json!(["harness:hook/event"]);

    let bytes = rehash_first_bundle(&mut document);
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(
        result.err(),
        Some(BundleError::UnknownResourceReference {
            bundle_id: "hya/alpha".to_string(),
            kind: "resource".to_string(),
            reference: "harness:hook/event".to_string(),
        })
    );
}

#[test]
fn prepared_decode_rejects_unknown_bundle_collections() {
    let prepared = prepare_package(source("alpha", "hya/alpha", "alpha", false));
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };

    for field in ["files", "helpers", "dependencies", "imports"] {
        let mut document = document.clone();
        document["bundles"][0][field] = serde_json::json!([]);
        let bytes = rehash_first_bundle(&mut document);
        let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
        assert!(matches!(result, Err(BundleError::PreparedDecode { .. })));
    }
}

fn skill_source(resource_path: &str) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: hya/skills
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: {resource_path}
agent:
  id: lead
  role: main
"#,
    );
    BundleSource::new(
        "skill-source",
        vec![
            SourceFile::new("./bundle.yaml", manifest.into_bytes()),
            SourceFile::new(resource_path, b"# Docs\n".as_slice()),
        ],
    )
}

#[test]
fn normalized_source_paths_produce_identical_prepared_content() {
    let canonical = prepare_package(skill_source("resources/skills/docs.md"));
    let Ok(canonical) = canonical else {
        panic!("canonical source failed: {canonical:?}");
    };
    let dotted = prepare_package(skill_source("resources/./skills/docs.md"));
    let Ok(dotted) = dotted else {
        panic!("dotted source failed: {dotted:?}");
    };

    assert_eq!(canonical.bytes(), dotted.bytes());
    assert_eq!(canonical.digest(), dotted.digest());
    assert_eq!(
        canonical.bundles()[0].skills()[0].source_path,
        "resources/skills/docs.md"
    );
}

#[test]
fn build_time_directory_reader_feeds_the_same_preparer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/directory");
    let source = BundleSource::read_directory(&root);
    let Ok(source) = source else {
        panic!("directory read failed: {source:?}");
    };
    let prepared = prepare_package(source);
    let Ok(prepared) = prepared else {
        panic!("directory preparation failed: {prepared:?}");
    };
    assert_eq!(prepared.index()[0].bundle_id, "hya/directory-fixture");
    assert_eq!(
        prepared.bundles()[0].agents()[0].prompt.as_deref(),
        Some("You are the directory fixture."),
    );
}

#[test]
fn workflow_bundle_rejects_missing_stage_agent_with_typed_error() {
    let result = prepare_package(workflow_source_with_stage_agent("missing"));
    assert_eq!(
        result.err(),
        Some(BundleError::WorkflowAgentMissing {
            bundle_id: "hya/workflow-missing-agent".to_string(),
            agent_id: "missing".to_string(),
            reference: "stage:execute".to_string(),
        })
    );
}

/// Reserved built-in ids cannot stand in for packaged Workflow closure members.
#[test]
fn workflow_bundle_rejects_reserved_builtin_stage_agent_as_missing_closure() {
    let result = prepare_package(workflow_source_with_stage_agent("general"));
    assert_eq!(
        result.err(),
        Some(BundleError::WorkflowAgentMissing {
            bundle_id: "hya/workflow-missing-agent".to_string(),
            agent_id: "general".to_string(),
            reference: "stage:execute".to_string(),
        })
    );
}

/// A loop verifier is part of the exact Workflow Agent closure.
#[test]
fn workflow_bundle_rejects_missing_verifier_agent_with_typed_error() {
    let result = prepare_package(workflow_source_with_verifier_closure(false, true));
    assert_eq!(
        result.err(),
        Some(BundleError::WorkflowAgentMissing {
            bundle_id: "hya/workflow-closure".to_string(),
            agent_id: "verifier".to_string(),
            reference: "verifier:execute".to_string(),
        })
    );
}

/// Recursive `can_spawn` targets are part of the exact Workflow Agent closure.
#[test]
fn workflow_bundle_rejects_missing_transitive_spawn_agent_with_typed_error() {
    let result = prepare_package(workflow_source_with_verifier_closure(true, false));
    assert_eq!(
        result.err(),
        Some(BundleError::WorkflowAgentMissing {
            bundle_id: "hya/workflow-closure".to_string(),
            agent_id: "helper".to_string(),
            reference: "agent:worker".to_string(),
        })
    );
}

#[test]
fn prepared_catalog_rejects_v1_bytes_without_upgrading_them() {
    let bytes = br#"{"format_version":1,"bundles":[],"index":[]}"#;
    let result = PreparedCatalog::decode(bytes, &digest(bytes));
    assert_eq!(result.err(), Some(BundleError::NonCanonicalPreparedCatalog));
}

#[test]
fn workflow_bundle_packages_stage_verifier_and_transitive_helper_agents() {
    let prepared = prepare_package(workflow_source_with_verifier_closure(true, true));
    let Ok(prepared) = prepared else {
        panic!("Workflow closure preparation failed: {prepared:?}");
    };
    let [bundle] = prepared.bundles() else {
        panic!("Workflow closure must prepare one bundle");
    };
    assert_eq!(
        bundle
            .agents()
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>(),
        ["helper", "verifier", "worker"]
    );
    assert_eq!(
        bundle.workflow().map(|workflow| workflow.id.as_str()),
        Some("closure")
    );
}

/// Build one AgentBundle source declaring one JS tool and optional schemas.
fn schema_bundle_source(schemas_block: Option<&str>, tools: bool) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: hya/schema-demo
  version: 1.0.0
  publisher: hya
{schemas_block}resources:
  tools:
    - id: query
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: lead
  role: main
  resource_view:
    allow:
      - query
      - runtime
"#,
        schemas_block = schemas_block.unwrap_or_default(),
    );
    let mut files = vec![
        SourceFile::new("bundle.yaml", manifest.into_bytes()),
        SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
    ];
    if !tools {
        files.pop();
    }
    BundleSource::new("schema-demo", files)
}

fn panic_on_prepare_error(error: BundleError) -> ! {
    panic!("valid schemas must prepare: {error:?}");
}

#[test]
fn manifest_schemas_prepare_emit_and_round_trip() {
    let prepared = match prepare_package(schema_bundle_source(
        Some(
            "schemas:\n  - scheme: db\n    tool: query\n    writable: false\n  - scheme: kv\n    tool: query\n    writable: true\n",
        ),
        true,
    )) {
        Ok(prepared) => prepared,
        Err(error) => panic_on_prepare_error(error),
    };

    let [bundle] = prepared.bundles() else {
        panic!("one bundle expected");
    };
    assert_eq!(bundle.identity().id, "hya/schema-demo");
    let rows = prepared.schemas();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bundle_id, "hya/schema-demo");
    assert_eq!(
        rows[0]
            .schemas
            .iter()
            .map(|schema| (
                schema.scheme.as_str(),
                schema.tool.as_str(),
                schema.writable
            ))
            .collect::<Vec<_>>(),
        [("db", "query", false), ("kv", "query", true)],
        "declared schemas are emitted sorted by scheme with their writable flags"
    );
    assert_eq!(prepared.bundle_schemas("hya/schema-demo").len(), 2);
    assert!(prepared.bundle_schemas("hya/other").is_empty());

    // The section round-trips through the canonical document bytes.
    let decoded = match PreparedCatalog::decode(prepared.bytes(), prepared.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("schema declarations must survive decode: {error:?}"),
    };
    assert_eq!(decoded.schemas(), rows);

    // A bundle without schemas keeps the document section absent entirely.
    let plain = match prepare_package(schema_bundle_source(None, true)) {
        Ok(plain) => plain,
        Err(error) => panic_on_prepare_error(error),
    };
    assert!(plain.schemas().is_empty());
    let document = match serde_json::from_slice::<serde_json::Value>(plain.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert!(
        document.get("schemas").is_none(),
        "the schemas section must be skipped when empty (v2-compatible bytes)"
    );
    match PreparedCatalog::decode(plain.bytes(), plain.digest()) {
        Ok(_) => {}
        Err(error) => panic!("plain doc stays decodable: {error:?}"),
    }
}

#[test]
fn manifest_schemas_missing_tool_reference_is_rejected() {
    let error = match prepare_package(schema_bundle_source(
        Some("schemas:\n  - scheme: db\n    tool: nosuch\n    writable: false\n"),
        true,
    )) {
        Err(error) => error,
        Ok(_) => panic!("schemas must reference declared tools"),
    };
    let BundleError::InvalidManifest {
        source_name,
        detail,
    } = error
    else {
        panic!("expected an invalid-manifest error: {error:?}");
    };
    assert_eq!(source_name, "hya/schema-demo");
    assert!(
        detail.contains("nosuch") && detail.contains("does not declare"),
        "the diagnostic must name the missing tool: {detail}"
    );
}

#[test]
fn manifest_schemas_invalid_and_internal_scheme_tokens_are_rejected() {
    for scheme in ["local", "artifact", "d", "has__sep", "no.dot"] {
        let block = format!("schemas:\n  - scheme: {scheme}\n    tool: query\n");
        let error = match prepare_package(schema_bundle_source(Some(&block), true)) {
            Err(error) => error,
            Ok(_) => panic!("invalid scheme tokens must be rejected: {scheme}"),
        };
        let BundleError::InvalidManifest {
            source_name,
            detail,
        } = error
        else {
            panic!("expected an invalid-manifest error for {scheme}: {error:?}");
        };
        assert_eq!(source_name, "hya/schema-demo");
        assert!(
            detail.contains(scheme),
            "the diagnostic must name the scheme: {detail}"
        );
    }
}

#[test]
fn manifest_schemas_duplicate_scheme_is_rejected() {
    let error = match prepare_package(schema_bundle_source(
        Some(
            "schemas:\n  - scheme: db\n    tool: query\n  - scheme: db\n    tool: query\n    writable: true\n",
        ),
        true,
    )) {
        Err(error) => error,
        Ok(_) => panic!("one scheme may be declared once per bundle"),
    };
    let BundleError::InvalidManifest { detail, .. } = error else {
        panic!("expected an invalid-manifest error: {error:?}");
    };
    assert!(
        detail.contains("more than once"),
        "the diagnostic must name the duplicate: {detail}"
    );
}

/// Build one AgentBundle source with an optional `extensions.process` block.
fn process_bundle_source(process_block: Option<&str>) -> BundleSource {
    let process = process_block.map_or_else(String::new, |block| format!("  process:\n{block}"));
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: hya/process-demo
  version: 1.0.0
  publisher: hya
extensions:
{process}agent:
  id: process-lead
  role: main
"#,
    );
    BundleSource::new(
        "process-demo",
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    )
}

#[test]
fn manifest_extensions_process_declares_and_round_trips() {
    let prepared = match prepare_package(process_bundle_source(Some(
        "    kind: bun\n    command: [bun, run, extensions/runtime.ts]\n",
    ))) {
        Ok(prepared) => prepared,
        Err(error) => panic!("valid extensions.process must prepare: {error:?}"),
    };

    let Some(process) = prepared.bundle_process("hya/process-demo") else {
        panic!("the declared process extension must be emitted");
    };
    assert_eq!(process.kind.as_str(), "bun");
    assert_eq!(
        process.command,
        ["bun", "run", "extensions/runtime.ts"],
        "the declared argv is preserved verbatim"
    );
    assert!(prepared.bundle_process("hya/other").is_none());

    // The declaration round-trips through the canonical document bytes.
    let decoded = match PreparedCatalog::decode(prepared.bytes(), prepared.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("process declarations must survive decode: {error:?}"),
    };
    assert_eq!(decoded.bundle_process("hya/process-demo"), Some(process));

    // A bundle without `extensions.process` keeps the document section absent.
    let plain = match prepare_package(process_bundle_source(None)) {
        Ok(plain) => plain,
        Err(error) => panic!("plain bundle must prepare: {error:?}"),
    };
    assert!(plain.bundle_process("hya/process-demo").is_none());
    let document = match serde_json::from_slice::<serde_json::Value>(plain.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert!(
        document.get("extensions_process").is_none(),
        "the extensions_process section must be skipped when empty (v2-compatible bytes)"
    );
    match PreparedCatalog::decode(plain.bytes(), plain.digest()) {
        Ok(_) => {}
        Err(error) => panic!("plain doc stays decodable: {error:?}"),
    }
}

#[test]
fn manifest_extensions_process_empty_command_is_rejected() {
    for command in ["[]", "[\"\"]", "[\"bun\", \"\"]"] {
        let manifest = format!("    kind: bun\n    command: {command}\n");
        let error = match prepare_package(process_bundle_source(Some(&manifest))) {
            Err(error) => error,
            Ok(_) => panic!("empty process command must be rejected: {command}"),
        };
        let BundleError::InvalidManifest { detail, .. } = error else {
            panic!("expected an invalid-manifest error for {command}: {error:?}");
        };
        assert!(
            detail.contains("command"),
            "the diagnostic must name the command: {detail}"
        );
    }
}

#[test]
fn manifest_extensions_process_unknown_kind_is_rejected() {
    let error = match prepare_package(process_bundle_source(Some(
        "    kind: python\n    command: [python3, server.py]\n",
    ))) {
        Err(error) => error,
        Ok(_) => panic!("unknown process kinds must be rejected"),
    };
    assert!(
        matches!(error, BundleError::InvalidManifest { .. }),
        "expected an invalid-manifest error: {error:?}"
    );
}

/// Build one AgentBundle source declaring `resources.mcp` plus MCP config files.
fn mcp_bundle_source(mcp_list: &str, files: &[(&str, &str)]) -> BundleSource {
    let manifest = format!(
        r#"kind: AgentBundle
identity:
  id: hya/mcp-demo
  version: 1.0.0
  publisher: hya
resources:
  mcp:
{mcp_list}agent:
  id: mcp-lead
  role: main
"#,
    );
    let mut sources = vec![SourceFile::new("bundle.yaml", manifest.into_bytes())];
    for (path, content) in files {
        sources.push(SourceFile::new(*path, content.as_bytes().to_vec()));
    }
    BundleSource::new("mcp-demo", sources)
}

#[test]
fn manifest_mcp_resources_prepare_emit_and_round_trip() {
    let prepared = match prepare_package(mcp_bundle_source(
        "    - id: vecdb\n      path: mcp/vecdb.json\n    - id: remote\n      path: mcp/remote.json\n",
        &[
            ("mcp/vecdb.json", r#"{"command": ["python3", "vecdb.py"]}"#),
            (
                "mcp/remote.json",
                r#"{"url": "https://mcp.example/sse", "timeout_ms": 250}"#,
            ),
        ],
    )) {
        Ok(prepared) => prepared,
        Err(error) => panic!("valid resources.mcp must prepare: {error:?}"),
    };

    let [bundle] = prepared.bundles() else {
        panic!("one bundle expected");
    };
    let rows = bundle
        .mcp()
        .iter()
        .map(|resource| {
            (
                resource.local_id.as_str(),
                resource.stable_id.as_str(),
                resource.source_path.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                "remote",
                "bundle:hya/mcp-demo/mcp/remote",
                "mcp/remote.json"
            ),
            ("vecdb", "bundle:hya/mcp-demo/mcp/vecdb", "mcp/vecdb.json"),
        ],
        "mcp declarations emit as stable prepared resources sorted by local id"
    );
    let vecdb = &bundle.mcp()[1];
    assert_eq!(
        vecdb.content, r#"{"command": ["python3", "vecdb.py"]}"#,
        "the declaration file bytes are retained verbatim"
    );

    // The declarations round-trip through the canonical document bytes.
    let decoded = match PreparedCatalog::decode(prepared.bytes(), prepared.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("mcp declarations must survive decode: {error:?}"),
    };
    assert_eq!(decoded.bundles()[0].mcp(), bundle.mcp());
}

#[test]
fn manifest_mcp_resource_files_are_shape_validated() {
    let missing = prepare_package(mcp_bundle_source(
        "    - id: vecdb\n      path: mcp/vecdb.json\n",
        &[],
    ));
    assert_eq!(
        missing.err(),
        Some(BundleError::MissingReference {
            bundle_id: "hya/mcp-demo".to_string(),
            path: "mcp/vecdb.json".to_string(),
        }),
        "the declaration file must exist"
    );

    for (label, content) in [
        ("not-json", "command: python3"),
        ("neither-command-nor-url", r#"{"env": {}}"#),
        ("empty-command", r#"{"command": []}"#),
        ("blank-command-arg", r#"{"command": ["python3", ""]}"#),
        ("unknown-field", r#"{"command": ["x"], "args": [1]}"#),
        ("wrong-type", r#"{"command": "python3"}"#),
    ] {
        let error = match prepare_package(mcp_bundle_source(
            "    - id: vecdb\n      path: mcp/vecdb.json\n",
            &[("mcp/vecdb.json", content)],
        )) {
            Err(error) => error,
            Ok(_) => panic!("invalid mcp file must be rejected: {label}"),
        };
        let BundleError::InvalidManifest { detail, .. } = error else {
            panic!("expected an invalid-manifest error for {label}: {error:?}");
        };
        assert!(
            detail.contains("mcp/vecdb.json"),
            "the diagnostic must name the file: {detail}"
        );
    }
}
