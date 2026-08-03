use hya_bundle::{BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_builtins};
use sha2::{Digest, Sha256};

fn source(root: &str, bundle_id: &str, stable_id: &str, reverse_files: bool) -> BundleSource {
    let manifest = format!(
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: {stable_id}
    role: main
    prompt: prompts/lead.md
    spawn_lifecycle: transient
    harness_access: full
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
fn deterministic_prepare_is_independent_of_source_and_file_order() {
    let forward = prepare_builtins(vec![
        source("z-source", "hya/zeta", "zeta", false),
        source("a-source", "hya/alpha", "alpha", false),
    ]);
    let Ok(forward) = forward else {
        panic!("forward preparation failed: {forward:?}");
    };

    let reverse = prepare_builtins(vec![
        source("a-source", "hya/alpha", "alpha", true),
        source("z-source", "hya/zeta", "zeta", true),
    ]);
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
        ["hya/alpha", "hya/zeta"]
    );
}

#[test]
fn prepared_catalog_round_trips_and_rejects_tampered_bytes() {
    let prepared = prepare_builtins(vec![source("alpha-source", "hya/alpha", "alpha", false)]);
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
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
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
    let Some(agents) = bundle["agents"].as_array_mut() else {
        panic!("prepared agents missing");
    };
    let mut second = agents[0].clone();
    second["local_id"] = serde_json::Value::String("aardvark".to_string());
    second["stable_id"] = serde_json::Value::String("aardvark".to_string());
    agents.push(second);

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
    document["index"][0]["stable_agent_ids"] = serde_json::json!(["alpha", "aardvark"]);

    let bytes = serde_json::to_vec(&document);
    let Ok(bytes) = bytes else {
        panic!("prepared document encode failed: {bytes:?}");
    };
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(result.err(), Some(BundleError::NonCanonicalPreparedCatalog));
}

#[test]
fn prepared_decode_rejects_content_with_recomputed_outer_digests() {
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["agents"][0]["prompt"] =
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
    let prepared = prepare_builtins(vec![skill_source("resources/skills/docs.md")]);
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
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/spawn-order
  version: 1.0.0
  publisher: hya
agents:
  - local_id: alpha
    stable_id: alpha
    role: subagent
    spawn_lifecycle: transient
    harness_access: full
  - local_id: beta
    stable_id: beta
    role: subagent
    spawn_lifecycle: transient
    harness_access: full
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
    can_spawn:
      - beta
      - alpha
"#;
    let prepared = prepare_builtins(vec![BundleSource::new(
        "spawn-order",
        vec![SourceFile::new("bundle.yaml", manifest.as_slice())],
    )]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    let Some(can_spawn) = document["bundles"][0]["agents"][2]["can_spawn"].as_array_mut() else {
        panic!("prepared can_spawn missing");
    };
    can_spawn.reverse();

    let bytes = rehash_first_bundle(&mut document);
    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(result.err(), Some(BundleError::NonCanonicalPreparedCatalog));
}

#[test]
fn prepared_decode_revalidates_full_catalog_references() {
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["agents"][0]["can_spawn"] = serde_json::json!(["missing"]);
    let bytes = rehash_first_bundle(&mut document);

    let result = PreparedCatalog::decode(&bytes, &digest(&bytes));
    assert_eq!(
        result.err(),
        Some(BundleError::UnknownAgentReference {
            bundle_id: "hya/alpha".to_string(),
            agent_id: "alpha".to_string(),
            reference: "missing".to_string(),
        })
    );
}

#[test]
fn prepared_decode_rejects_unreferenced_unsupported_hook_local_id() {
    let manifest = br#"api_version: hya.agent-bundle/v1
kind: AgentBundle
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
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
"#;
    let prepared = prepare_builtins(vec![BundleSource::new(
        "prepared-hook",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new(
                "extensions/runtime.js",
                b"export const runtime = true;\n".as_slice(),
            ),
        ],
    )]);
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
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
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
        document["bundles"][0]["agents"][0]["hook_refs"] = serde_json::json!([raw_ref]);
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
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
    let Ok(prepared) = prepared else {
        panic!("preparation failed: {prepared:?}");
    };
    let document = serde_json::from_slice::<serde_json::Value>(prepared.bytes());
    let Ok(mut document) = document else {
        panic!("prepared document was not JSON: {document:?}");
    };
    document["bundles"][0]["agents"][0]["resource_view"]["allow"] =
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
    let prepared = prepare_builtins(vec![source("alpha", "hya/alpha", "alpha", false)]);
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
        r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/skills
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: docs
      path: {resource_path}
agents:
  - local_id: lead
    stable_id: lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
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
    let canonical = prepare_builtins(vec![skill_source("resources/skills/docs.md")]);
    let Ok(canonical) = canonical else {
        panic!("canonical source failed: {canonical:?}");
    };
    let dotted = prepare_builtins(vec![skill_source("resources/./skills/docs.md")]);
    let Ok(dotted) = dotted else {
        panic!("dotted source failed: {dotted:?}");
    };

    assert_eq!(canonical.bytes(), dotted.bytes());
    assert_eq!(canonical.digest(), dotted.digest());
    assert_eq!(
        canonical.bundles()[0].skills[0].source_path,
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
    let prepared = prepare_builtins(vec![source]);
    let Ok(prepared) = prepared else {
        panic!("directory preparation failed: {prepared:?}");
    };
    assert_eq!(prepared.index()[0].bundle_id, "hya/directory-fixture");
    assert_eq!(
        prepared.bundles()[0].agents[0].prompt.as_deref(),
        Some("You are the directory fixture.")
    );
}
