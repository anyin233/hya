//! Catalog assembly, workflow/resource resolution, manifest `apis:`, and `permission_modes:`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_bundle::{
    ApiMethod, ApiScope, BundleCatalog, BundleError, BundleSource, ExportKind, PreparedCatalog,
    PreparedResource, SourceFile, prepare_package,
};
use sha2::{Digest, Sha256};

const SCHEMA: &str = r#"{"type":"object","properties":{"n":{"type":"integer"}}}"#;

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn rehash(doc: &mut serde_json::Value) -> (Vec<u8>, String) {
    let mut content = doc.clone();
    content.as_object_mut().expect("object").remove("digest");
    doc["digest"] = serde_json::json!(sha256_hex(&serde_json::to_vec(&content).expect("encode")));
    let bytes = serde_json::to_vec(doc).expect("encode doc");
    let digest = sha256_hex(&bytes);
    (bytes, digest)
}

fn skill_bundle(root: &str, bundle_id: &str, agent_id: &str, content: &str) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nresources:\n  skills:\n    - id: docs\n      path: resources/skills/docs.md\nagents:\n  - id: {agent_id}\n    role: main\n"
    );
    BundleSource::new(
        root,
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("resources/skills/docs.md", content.as_bytes()),
        ],
    )
}

fn workflow_bundle(
    bundle_id: &str,
    workflow_id: &str,
    agent_id: &str,
    extra_node_yaml: &str,
) -> BundleSource {
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nworkflow:\n  id: {workflow_id}\n  path: workflows/{workflow_id}.hya.md\nagents:\n  - id: {agent_id}\n    role: subagent\n    prompt: prompts/{agent_id}.md\n"
    );
    let workflow = format!(
        "---\nkind: Workflow\nname: {workflow_id}\ndescription: Catalog Workflow.\nnodes:\n  run:\n    agent: {agent_id}\n    directive: Run the stage.\n{extra_node_yaml}---\nflowchart TD\n  run\n"
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

fn api_bundle(with_agents: bool, process: bool, apis: Option<&str>) -> BundleSource {
    let process = if process {
        "runtime:\n  kind: js\n  mode: process\n  entry: api.ts\n  command: [bun, run, '${BUNDLE_ROOT}/api.ts']\nassets:\n  - id: in-schema\n    path: schemas/in.json\n  - id: bad-schema\n    path: schemas/bad.json\n"
    } else {
        ""
    };
    let apis = apis.map_or_else(String::new, |block| format!("apis:\n{block}"));
    let body = if with_agents {
        "agents:\n  - id: api-lead\n    role: main\n"
    } else {
        ""
    };
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity:\n  id: hya/api-demo\n  version: 1.0.0\n  publisher: hya\n{process}{apis}{body}"
    );
    BundleSource::new(
        "api-demo",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("api.ts", b"// api process\n".as_slice()),
            SourceFile::new("schemas/in.json", SCHEMA.as_bytes()),
            SourceFile::new("schemas/bad.json", b"not json".as_slice()),
        ],
    )
}

fn api_row(id: &str, method: &str, scope: &str, path: &str) -> String {
    format!("  - {{ id: {id}, method: {method}, scope: {scope}, path: '{path}' }}\n")
}

fn mode_bundle(with_agents: bool, process: bool, hook: bool, modes: Option<&str>) -> BundleSource {
    let process = if process {
        "runtime:\n  kind: js\n  mode: process\n  entry: approver.ts\n  command: [bun, run, '${BUNDLE_ROOT}/approver.ts']\n"
    } else {
        ""
    };
    let hook = if hook {
        "resources:\n  hooks:\n    - id: permission.approve\n      path: hooks/permission-approve.json\n"
    } else {
        ""
    };
    let modes = modes.map_or_else(String::new, |block| format!("permission_modes:\n{block}"));
    let body = if with_agents {
        "agents:\n  - id: mode-lead\n    role: main\n"
    } else {
        ""
    };
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity:\n  id: hya/approver\n  version: 1.0.0\n  publisher: hya\n{process}{hook}{modes}{body}"
    );
    BundleSource::new(
        "approver",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("approver.ts", b"// approver process\n".as_slice()),
            SourceFile::new("hooks/permission-approve.json", b"{}".as_slice()),
        ],
    )
}

fn mode_row(id: &str, title: &str) -> String {
    format!("  - {{ id: {id}, title: '{title}' }}\n")
}

fn invalid_detail(source: BundleSource) -> String {
    match prepare_package(source) {
        Err(BundleError::InvalidManifest { detail, .. }) => detail,
        other => panic!("expected InvalidManifest, got {other:?}"),
    }
}

#[test]
fn empty_and_verified_catalogs_share_assets_and_semantic_identity() {
    let empty = BundleCatalog::from_prepared(&[]).expect("empty catalog valid");
    assert!(empty.bundles().is_empty());
    assert!(empty.resolve_agent("anything").is_none());

    let prepared = prepare_package(skill_bundle(
        "shared",
        "hya/shared",
        "shared",
        "shared asset",
    ))
    .expect("prepare");
    let first = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("verified 1");
    let second = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("verified 2");
    let unverified = BundleCatalog::from_prepared(prepared.bundles()).expect("unverified");

    let id1 = first
        .semantic_identity_v1()
        .expect("verified semantic identity");
    assert!(!id1.is_empty());
    assert_eq!(second.semantic_identity_v1(), Some(id1));
    assert_eq!(unverified.semantic_identity_v1(), None);
    assert!(std::sync::Arc::ptr_eq(
        &first.bundles()[0],
        &second.bundles()[0]
    ));
    assert!(std::ptr::eq(
        &prepared.bundles()[0],
        first.bundles()[0].as_ref()
    ));
    assert_eq!(first.bundles()[0].skills()[0].content, "shared asset");
}

#[test]
fn catalog_indexes_mcp_and_rejects_unsupported_hook_or_duplicate_bundle() {
    let prepared = prepare_package(skill_bundle(
        "mcp",
        "hya/catalog-mcp",
        "catalog-mcp",
        "docs",
    ))
    .expect("prepare");
    let mut bundle = prepared.bundles()[0].clone();
    let content = r#"{"command": ["python3", "docs.py"]}"#;
    bundle.mcp.push(PreparedResource {
        local_id: "docs".into(),
        stable_id: "bundle:hya/catalog-mcp/mcp/docs".into(),
        source_path: "resources/mcp/docs.json".into(),
        digest: sha256_hex(content.as_bytes()),
        content: content.into(),
        binary_base64: None,
        aliases: Vec::new(),
        overrides: None,
    });
    let catalog = BundleCatalog::from_prepared(&[bundle]).expect("mcp indexes");
    assert_eq!(
        catalog
            .bundle_resources("hya/catalog-mcp", ExportKind::Mcp)
            .map(<[PreparedResource]>::len),
        Some(1)
    );

    // Unsupported hook local_id rejected from prepared data.
    let hook_prepared = prepare_package(BundleSource::new(
        "catalog-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: hya/catalog-hook, version: 1.0.0, publisher: hya }\nruntime: { kind: js, mode: managed, entry: extensions/runtime.js }\nresources:\n  hooks: [{ id: event, path: extensions/runtime.js }]\nagents: [{ id: catalog-hook, role: main, hook_refs: [bundle:hya/catalog-hook/hook/event] }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {};\n".as_slice()),
        ],
    ))
    .expect("prepare hook");
    let mut forged = hook_prepared.bundles()[0].clone();
    forged.hooks[0].local_id = "audit".into();
    forged.hooks[0].stable_id = "bundle:hya/catalog-hook/hook/audit".into();
    assert_eq!(
        BundleCatalog::from_prepared(&[forged]).err(),
        Some(BundleError::UnsupportedBundleFeature {
            bundle_id: "hya/catalog-hook".into(),
            feature: "hook:audit".into(),
        })
    );

    // Duplicate bundle identity rejected even with disjoint agent exports.
    let make_dup = |agent: &str| {
        prepare_package(BundleSource::new(
            agent,
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nschema_version: 2\nidentity: {{ id: hya/duplicate-package, version: 1.0.0, publisher: hya }}\nagents: [{{ id: {agent}, role: main }}]\n"
                ),
            )],
        ))
        .expect("prepare dup")
    };
    let a = make_dup("dup-a");
    let b = make_dup("dup-b");
    assert!(matches!(
        BundleCatalog::from_prepared(&[a.bundles()[0].clone(), b.bundles()[0].clone()]),
        Err(BundleError::DuplicateBundleId { bundle_id }) if bundle_id == "hya/duplicate-package"
    ));
}

#[test]
fn catalog_resource_resolution_prefers_local_and_parses_nested_kind_segments() {
    let alpha =
        prepare_package(skill_bundle("alpha", "hya/alpha", "alpha", "alpha docs")).expect("alpha");
    let beta =
        prepare_package(skill_bundle("beta", "hya/beta", "beta", "beta docs")).expect("beta");
    let nested_id = "hya/tool/skill/mcp-nest";
    let nested = prepare_package(skill_bundle(
        "nested",
        nested_id,
        "nested-lead",
        "nested docs",
    ))
    .expect("nested");
    let bundles = [alpha.bundles(), beta.bundles(), nested.bundles()].concat();
    let catalog = BundleCatalog::from_prepared(&bundles).expect("catalog");

    let local = catalog
        .resolve_resource("hya/alpha", ExportKind::Skill, "docs")
        .expect("local");
    assert_eq!(local.stable_id, "bundle:hya/alpha/skill/docs");
    assert_eq!(local.content, "alpha docs");

    let qualified = catalog
        .resolve_resource("hya/alpha", ExportKind::Skill, "bundle:hya/beta/skill/docs")
        .expect("qualified");
    assert_eq!(qualified.stable_id, "bundle:hya/beta/skill/docs");
    assert_eq!(qualified.content, "beta docs");

    let nested_local = catalog
        .resolve_resource(nested_id, ExportKind::Skill, "docs")
        .expect("nested local");
    assert_eq!(
        nested_local.stable_id,
        format!("bundle:{nested_id}/skill/docs")
    );
    let nested_qual = catalog
        .resolve_resource(
            nested_id,
            ExportKind::Skill,
            &format!("bundle:{nested_id}/skill/docs"),
        )
        .expect("nested qualified");
    assert_eq!(nested_qual.content, "nested docs");
}

#[test]
fn workflow_catalog_indexes_defers_model_availability_and_disambiguates_bare_names() {
    let single = prepare_package(workflow_bundle(
        "hya/workflow-catalog",
        "demo",
        "worker",
        "",
    ))
    .expect("workflow");
    let catalog = BundleCatalog::from_prepared(single.bundles()).expect("catalog");
    let (owner, agent) = catalog
        .resolve_agent_entry("worker")
        .expect("worker indexed");
    assert_eq!(owner, "hya/workflow-catalog");
    assert_eq!(agent.id.as_str(), "worker");
    let (wf_owner, wf) = catalog
        .resolve_workflow_entry("bundle:hya/workflow-catalog/workflow/demo")
        .expect("qualified workflow");
    assert_eq!(wf_owner, "hya/workflow-catalog");
    assert_eq!(wf.id, "demo");
    assert_eq!(catalog.resolve_workflow("demo"), Some(wf));

    // Model route availability is deferred to runtime.
    let routed = prepare_package(workflow_bundle(
        "hya/workflow-routed-catalog",
        "routed",
        "worker",
        "    model:\n      id: unavailable-during-package/model\n      reasoning: medium\n",
    ))
    .expect("routed");
    let routed_cat = BundleCatalog::from_verified_catalogs(&[&routed]).expect("verified routed");
    let (_, routed_wf) = routed_cat
        .resolve_workflow_entry("bundle:hya/workflow-routed-catalog/workflow/routed")
        .expect("routed workflow");
    assert!(
        routed_wf
            .source
            .contains("unavailable-during-package/model")
    );

    // Ambiguous bare workflow names return None while qualified ids stay resolvable.
    let one = prepare_package(workflow_bundle(
        "hya/workflow-one",
        "demo",
        "worker-one",
        "",
    ))
    .expect("one");
    let two = prepare_package(workflow_bundle(
        "hya/workflow-two",
        "demo",
        "worker-two",
        "",
    ))
    .expect("two");
    let merged =
        BundleCatalog::from_prepared(&[one.bundles(), two.bundles()].concat()).expect("merged");
    assert!(merged.resolve_workflow("demo").is_none());
    assert!(
        merged
            .resolve_workflow("bundle:hya/workflow-one/workflow/demo")
            .is_some()
    );
    assert!(merged.resolve_agent("worker-one").is_some());
    assert!(merged.resolve_agent("worker-two").is_some());
}

#[test]
fn apis_prepare_sorted_round_trip_and_digest_behavior() {
    let block = format!(
        "  - id: usage\n    method: GET\n    scope: session\n    path: /usage\n    description: Token usage\n    response_schema: schemas/in.json\n{}{}",
        api_row("create", "POST", "global", "/items/{id}"),
        api_row("remove", "DELETE", "global", "/items/{id}"),
    );
    let catalog = prepare_package(api_bundle(false, true, Some(&block))).expect("prepare apis");
    let apis = catalog.bundle_apis("hya/api-demo");
    assert_eq!(
        apis.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["create", "remove", "usage"]
    );
    assert_eq!(apis[0].method, ApiMethod::Post);
    assert_eq!(apis[0].scope, ApiScope::Global);
    assert_eq!(apis[0].path, "/items/{id}");
    assert_eq!(apis[0].description, "");
    assert_eq!(apis[2].description, "Token usage");
    assert_eq!(apis[2].response_schema.as_deref(), Some("schemas/in.json"));
    assert_eq!(apis[2].request_schema, None);
    assert!(catalog.bundle_apis("hya/other").is_empty());

    let decoded = PreparedCatalog::decode(catalog.bytes(), catalog.digest()).expect("decode");
    assert_eq!(decoded.bundle_apis("hya/api-demo"), apis);
    let doc: serde_json::Value = serde_json::from_slice(catalog.bytes()).expect("JSON");
    assert_eq!(doc["apis"][0]["method"], "POST");
    assert_eq!(doc["apis"][0]["scope"], "global");

    // Absent apis serialize as empty list, presence changes digest, order is normalized.
    let plain = prepare_package(api_bundle(false, true, None)).expect("plain");
    let with_one = prepare_package(api_bundle(
        false,
        true,
        Some(&api_row("usage", "GET", "session", "/usage")),
    ))
    .expect("with one");
    assert_ne!(plain.digest(), with_one.digest());
    let plain_doc: serde_json::Value = serde_json::from_slice(plain.bytes()).expect("JSON");
    assert!(plain_doc["apis"].as_array().is_some_and(Vec::is_empty));
    let a = api_row("a", "GET", "session", "/a");
    let b = api_row("b", "GET", "session", "/b");
    assert_eq!(
        prepare_package(api_bundle(false, true, Some(&format!("{b}{a}"))))
            .expect("ba")
            .digest(),
        prepare_package(api_bundle(false, true, Some(&format!("{a}{b}"))))
            .expect("ab")
            .digest()
    );
}

#[test]
fn agent_bearing_bundles_with_explicit_process_may_declare_apis() {
    let with_files = prepare_package(api_bundle(
        true,
        true,
        Some(&api_row("usage", "GET", "session", "/usage")),
    ))
    .expect("agent-bearing apis");
    assert_eq!(with_files.bundle_apis("hya/api-demo").len(), 1);

    let with_js = prepare_package(BundleSource::new(
        "decl-demo",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: hya/decl-demo, version: 1.0.0, publisher: hya }\nruntime: { kind: js, mode: process, entry: extensions/runtime.js, command: [bun, run, extensions/runtime.ts] }\napis:\n  - { id: usage, method: GET, scope: session, path: /usage, description: Token usage }\n  - { id: health, method: GET, scope: global, path: /health }\nagents: [{ id: decl-lead, role: main }]\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {}".as_slice()),
        ],
    ))
    .expect("agent + js + process apis");
    assert_eq!(
        with_js
            .bundle_apis("hya/decl-demo")
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["health", "usage"]
    );
}

#[test]
fn apis_reject_missing_process_invalid_ids_paths_methods_scopes_and_duplicates() {
    let missing_proc = invalid_detail(api_bundle(
        false,
        false,
        Some(&api_row("usage", "GET", "session", "/usage")),
    ));
    assert!(missing_proc.contains("apis") && missing_proc.contains("mode: process"));

    for id in ["''", "a/b", "-lead", "'has space'", "per%cent"] {
        let detail = invalid_detail(api_bundle(
            false,
            true,
            Some(&api_row(id, "GET", "session", "/x")),
        ));
        assert!(detail.contains("apis"), "{id}: {detail}");
    }
    for path in ["usage", "/", "/a/", "/a/{x}/{x}", "/a/*", "/a{b}", "/a/.."] {
        let detail = invalid_detail(api_bundle(
            false,
            true,
            Some(&api_row("x", "GET", "session", path)),
        ));
        assert!(detail.contains("path"), "{path}: {detail}");
    }
    let dup = invalid_detail(api_bundle(
        false,
        true,
        Some(&format!(
            "{}{}",
            api_row("usage", "GET", "session", "/a"),
            api_row("usage", "POST", "session", "/b")
        )),
    ));
    assert!(dup.contains("more than once"), "{dup}");

    for (method, scope) in [("get", "session"), ("HEAD", "session"), ("GET", "tree")] {
        assert!(
            prepare_package(api_bundle(
                false,
                true,
                Some(&api_row("x", method, scope, "/x"))
            ))
            .is_err(),
            "{method} {scope}"
        );
    }
}

#[test]
fn apis_reject_overlapping_templates_and_validate_schema_files() {
    let overlap_lit = invalid_detail(api_bundle(
        false,
        true,
        Some(&format!(
            "{}{}",
            api_row("one", "GET", "global", "/items/{id}"),
            api_row("two", "GET", "global", "/items/latest")
        )),
    ));
    assert!(
        overlap_lit.contains("overlaps")
            && overlap_lit.contains("one")
            && overlap_lit.contains("two")
    );

    let overlap_param = invalid_detail(api_bundle(
        false,
        true,
        Some(&format!(
            "{}{}",
            api_row("one", "PUT", "session", "/items/{id}"),
            api_row("two", "PUT", "session", "/items/{key}")
        )),
    ));
    assert!(overlap_param.contains("overlaps"));

    // Distinct methods or scopes may share the same template.
    let distinct = prepare_package(api_bundle(
        false,
        true,
        Some(&format!(
            "{}{}{}",
            api_row("get", "GET", "global", "/items/{id}"),
            api_row("put", "PUT", "global", "/items/{id}"),
            api_row("session", "GET", "session", "/items/{id}")
        )),
    ))
    .expect("distinct methods/scopes allowed");
    assert_eq!(distinct.bundle_apis("hya/api-demo").len(), 3);

    // Schema files: normalized when valid, rejected when missing or non-JSON.
    let with_schema = |field: &str, path: &str| {
        format!(
            "  - id: x\n    method: POST\n    scope: global\n    path: /x\n    {field}: {path}\n"
        )
    };
    let ok = prepare_package(api_bundle(
        false,
        true,
        Some(&with_schema("request_schema", "./schemas/in.json")),
    ))
    .expect("normalized schema");
    assert_eq!(
        ok.bundle_apis("hya/api-demo")[0].request_schema.as_deref(),
        Some("schemas/in.json")
    );
    let missing = invalid_detail(api_bundle(
        false,
        true,
        Some(&with_schema("request_schema", "schemas/missing.json")),
    ));
    assert!(missing.contains("request_schema") && missing.contains("extensions"));
    let bad_json = invalid_detail(api_bundle(
        false,
        true,
        Some(&with_schema("response_schema", "schemas/bad.json")),
    ));
    assert!(bad_json.contains("response_schema") && bad_json.contains("JSON"));
}

#[test]
fn non_canonical_api_rows_are_rejected_on_decode() {
    let catalog = prepare_package(api_bundle(
        false,
        true,
        Some(&format!(
            "{}{}",
            api_row("alpha", "GET", "session", "/a"),
            api_row("usage", "GET", "session", "/usage")
        )),
    ))
    .expect("prepare");
    let mut doc: serde_json::Value = serde_json::from_slice(catalog.bytes()).expect("JSON");
    doc["apis"].as_array_mut().expect("apis array").reverse();
    let (bytes, digest) = rehash(&mut doc);
    assert!(matches!(
        PreparedCatalog::decode(&bytes, &digest),
        Err(BundleError::NonCanonicalPreparedCatalog)
    ));
}

#[test]
fn permission_modes_prepare_sorted_round_trip_and_digest_behavior() {
    let block = format!(
        "  - id: careful\n    title: Careful\n    description: Approves read-only commands\n{}",
        mode_row("ci", "CI")
    );
    let catalog = prepare_package(mode_bundle(false, true, true, Some(&block))).expect("modes");
    let modes = catalog.bundle_permission_modes("hya/approver");
    assert_eq!(
        modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["careful", "ci"]
    );
    assert_eq!(modes[0].title, "Careful");
    assert_eq!(modes[0].description, "Approves read-only commands");
    assert_eq!(modes[1].description, "");
    assert!(catalog.bundle_permission_modes("hya/other").is_empty());

    let decoded = PreparedCatalog::decode(catalog.bytes(), catalog.digest()).expect("decode");
    assert_eq!(decoded.bundle_permission_modes("hya/approver"), modes);
    let doc: serde_json::Value = serde_json::from_slice(catalog.bytes()).expect("JSON");
    assert_eq!(doc["permission_modes"][1]["id"], "ci");

    // Absent modes serialize empty (hook allowed standalone), presence changes digest, order normalized.
    let plain = prepare_package(mode_bundle(false, true, true, None)).expect("plain");
    assert_eq!(plain.bundles()[0].hooks()[0].local_id, "permission.approve");
    let plain_doc: serde_json::Value = serde_json::from_slice(plain.bytes()).expect("JSON");
    assert!(
        plain_doc["permission_modes"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert_ne!(plain.digest(), catalog.digest());
    let a = mode_row("a", "A");
    let b = mode_row("b", "B");
    assert_eq!(
        prepare_package(mode_bundle(false, true, true, Some(&format!("{b}{a}"))))
            .expect("ba")
            .digest(),
        prepare_package(mode_bundle(false, true, true, Some(&format!("{a}{b}"))))
            .expect("ab")
            .digest()
    );

    // Agent-bearing bundle may also declare permission_modes.
    let with_agent = prepare_package(mode_bundle(
        true,
        true,
        true,
        Some(&mode_row("careful", "Careful")),
    ))
    .expect("agent bundle modes");
    assert_eq!(with_agent.bundle_permission_modes("hya/approver").len(), 1);
}

#[test]
fn permission_modes_require_process_hook_and_reject_invalid_declarations() {
    let no_proc = invalid_detail(mode_bundle(
        false,
        false,
        false,
        Some(&mode_row("careful", "Careful")),
    ));
    assert!(no_proc.contains("permission_modes") && no_proc.contains("mode: process"));
    let no_hook = invalid_detail(mode_bundle(
        false,
        true,
        false,
        Some(&mode_row("careful", "Careful")),
    ));
    assert!(no_hook.contains("permission_modes") && no_hook.contains("permission.approve"));

    for (block, needle) in [
        (mode_row("-bad", "Bad"), "id `-bad`"),
        (mode_row("a/b", "Slash"), "id `a/b`"),
        (mode_row(&"x".repeat(65), "Long"), "at most 64"),
        (mode_row("manual", "Reserved"), "reserved"),
        (mode_row("yolo", "Reserved"), "reserved"),
        (
            format!("{}{}", mode_row("dup", "One"), mode_row("dup", "Two")),
            "more than once",
        ),
        ("  - { id: empty, title: '' }\n".to_string(), "title"),
        (
            format!("  - {{ id: long, title: '{}' }}\n", "t".repeat(129)),
            "title",
        ),
        (
            format!(
                "  - {{ id: long, title: T, description: '{}' }}\n",
                "d".repeat(1025)
            ),
            "description",
        ),
        (
            (0..17)
                .map(|i| mode_row(&format!("m{i}"), "M"))
                .collect::<String>(),
            "at most 16",
        ),
    ] {
        let detail = invalid_detail(mode_bundle(false, true, true, Some(&block)));
        assert!(
            detail.contains("permission_modes") && detail.contains(needle),
            "expected `{needle}` in `{detail}`"
        );
    }

    assert!(
        prepare_package(mode_bundle(
            false,
            true,
            true,
            Some("  - { id: careful, title: Careful, allow: all }\n"),
        ))
        .is_err()
    );
}

#[test]
fn non_canonical_permission_mode_rows_are_rejected_on_decode() {
    let catalog = prepare_package(mode_bundle(
        false,
        true,
        true,
        Some(&format!(
            "{}{}",
            mode_row("alpha", "A"),
            mode_row("beta", "B")
        )),
    ))
    .expect("prepare");
    let mut doc: serde_json::Value = serde_json::from_slice(catalog.bytes()).expect("JSON");
    doc["permission_modes"]
        .as_array_mut()
        .expect("modes")
        .reverse();
    let (bytes, digest) = rehash(&mut doc);
    assert!(matches!(
        PreparedCatalog::decode(&bytes, &digest),
        Err(BundleError::NonCanonicalPreparedCatalog)
    ));
}
