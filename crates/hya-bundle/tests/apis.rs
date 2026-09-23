//! Manifest `apis:` declarations: HTTP endpoints served by a bundle's
//! explicit `extensions.process`.

use hya_bundle::{
    ApiMethod, ApiScope, BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package,
};
use sha2::{Digest, Sha256};

const SCHEMA: &str = r#"{"type":"object","properties":{"n":{"type":"integer"}}}"#;

/// One bundle source of `kind` with optional `extensions.process` and `apis`
/// blocks (YAML fragments indented for their parent key). The process
/// declares `schemas/in.json` (valid) and `schemas/bad.json` (not JSON) as
/// support files.
fn api_bundle(kind: &str, process: bool, apis: Option<&str>) -> BundleSource {
    let process = if process {
        "extensions:\n  process:\n    kind: bun\n    command: [bun, run, '${BUNDLE_ROOT}/api.ts']\n  files:\n    - id: api\n      path: api.ts\n    - id: in-schema\n      path: schemas/in.json\n    - id: bad-schema\n      path: schemas/bad.json\n"
    } else {
        ""
    };
    let apis = apis.map_or_else(String::new, |block| format!("apis:\n{block}"));
    let body = match kind {
        "AgentSetBundle" => "agents:\n  - id: api-lead\n    role: main\n",
        _ => "",
    };
    let manifest = format!(
        "kind: {kind}\nidentity:\n  id: hya/api-demo\n  version: 1.0.0\n  publisher: hya\n{process}{apis}{body}"
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

fn api(id: &str, method: &str, scope: &str, path: &str) -> String {
    format!("  - {{ id: {id}, method: {method}, scope: {scope}, path: '{path}' }}\n")
}

fn prepared(source: BundleSource) -> PreparedCatalog {
    match prepare_package(source) {
        Ok(prepared) => prepared,
        Err(error) => panic!("bundle must prepare: {error:?}"),
    }
}

fn invalid_detail(source: BundleSource) -> String {
    match prepare_package(source) {
        Err(BundleError::InvalidManifest { detail, .. }) => detail,
        Err(error) => panic!("expected an invalid-manifest error: {error:?}"),
        Ok(_) => panic!("the manifest must be rejected"),
    }
}

#[test]
fn plugin_apis_prepare_sorted_and_round_trip() {
    let block = format!(
        "  - id: usage\n    method: GET\n    scope: session\n    path: /usage\n    description: Token usage\n    response_schema: schemas/in.json\n{}{}",
        api("create", "POST", "global", "/items/{id}"),
        api("remove", "DELETE", "global", "/items/{id}"),
    );
    let catalog = prepared(api_bundle("Plugin", true, Some(&block)));
    let apis = catalog.bundle_apis("hya/api-demo");
    let ids = apis.iter().map(|api| api.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["create", "remove", "usage"], "apis are sorted by id");
    assert_eq!(apis[0].method, ApiMethod::Post);
    assert_eq!(apis[0].scope, ApiScope::Global);
    assert_eq!(apis[0].path, "/items/{id}");
    assert_eq!(apis[0].description, "", "description defaults to empty");
    assert_eq!(apis[2].description, "Token usage");
    assert_eq!(apis[2].response_schema.as_deref(), Some("schemas/in.json"));
    assert_eq!(apis[2].request_schema, None);
    assert!(catalog.bundle_apis("hya/other").is_empty());

    let decoded = match PreparedCatalog::decode(catalog.bytes(), catalog.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("apis must survive decode: {error:?}"),
    };
    assert_eq!(decoded.bundle_apis("hya/api-demo"), apis);

    let document: serde_json::Value = match serde_json::from_slice(catalog.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert_eq!(
        document["apis"][0]["bundle_id"], "hya/api-demo",
        "apis are a document-level section keyed by bundle id"
    );
    assert_eq!(document["apis"][0]["apis"][0]["method"], "POST");
    assert_eq!(document["apis"][0]["apis"][0]["scope"], "global");
}

#[test]
fn apis_change_the_catalog_digest_and_are_skipped_when_empty() {
    let plain = prepared(api_bundle("Plugin", true, None));
    let with_apis = prepared(api_bundle(
        "Plugin",
        true,
        Some(&api("usage", "GET", "session", "/usage")),
    ));
    assert_ne!(plain.digest(), with_apis.digest());
    let document: serde_json::Value = match serde_json::from_slice(plain.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert!(
        document.get("apis").is_none(),
        "the apis section is skipped when no bundle declares any"
    );
    let a = api("a", "GET", "session", "/a");
    let b = api("b", "GET", "session", "/b");
    let reordered = prepared(api_bundle("Plugin", true, Some(&format!("{b}{a}"))));
    let sorted = prepared(api_bundle("Plugin", true, Some(&format!("{a}{b}"))));
    assert_eq!(reordered.digest(), sorted.digest());
}

#[test]
fn every_process_backed_kind_may_declare_apis() {
    let catalog = prepared(api_bundle(
        "AgentSetBundle",
        true,
        Some(&api("usage", "GET", "session", "/usage")),
    ));
    assert_eq!(catalog.bundle_apis("hya/api-demo").len(), 1);
}

#[test]
fn apis_without_an_explicit_process_are_rejected() {
    let detail = invalid_detail(api_bundle(
        "Plugin",
        false,
        Some(&api("usage", "GET", "session", "/usage")),
    ));
    assert!(
        detail.contains("apis") && detail.contains("extensions.process"),
        "the diagnostic must name apis and the missing process: {detail}"
    );
}

#[test]
fn invalid_ids_templates_and_duplicates_are_rejected() {
    for id in ["''", "a/b", "-lead", "'has space'", "per%cent"] {
        let detail = invalid_detail(api_bundle(
            "Plugin",
            true,
            Some(&api(id, "GET", "session", "/x")),
        ));
        assert!(detail.contains("apis"), "{id}: {detail}");
    }
    for path in ["usage", "/", "/a/", "/a/{x}/{x}", "/a/*", "/a{b}", "/a/.."] {
        let detail = invalid_detail(api_bundle(
            "Plugin",
            true,
            Some(&api("x", "GET", "session", path)),
        ));
        assert!(detail.contains("path"), "{path}: {detail}");
    }
    let detail = invalid_detail(api_bundle(
        "Plugin",
        true,
        Some(&format!(
            "{}{}",
            api("usage", "GET", "session", "/a"),
            api("usage", "POST", "session", "/b")
        )),
    ));
    assert!(detail.contains("more than once"), "{detail}");
}

#[test]
fn unknown_methods_and_scopes_are_rejected() {
    for (method, scope) in [("get", "session"), ("HEAD", "session"), ("GET", "tree")] {
        assert!(
            prepare_package(api_bundle(
                "Plugin",
                true,
                Some(&api("x", method, scope, "/x")),
            ))
            .is_err(),
            "{method} {scope} must be rejected"
        );
    }
}

#[test]
fn overlapping_templates_under_one_method_and_scope_are_rejected() {
    let detail = invalid_detail(api_bundle(
        "Plugin",
        true,
        Some(&format!(
            "{}{}",
            api("one", "GET", "global", "/items/{id}"),
            api("two", "GET", "global", "/items/latest")
        )),
    ));
    assert!(
        detail.contains("overlaps") && detail.contains("one") && detail.contains("two"),
        "{detail}"
    );
    let detail = invalid_detail(api_bundle(
        "Plugin",
        true,
        Some(&format!(
            "{}{}",
            api("one", "PUT", "session", "/items/{id}"),
            api("two", "PUT", "session", "/items/{key}")
        )),
    ));
    assert!(detail.contains("overlaps"), "{detail}");
    // The same template under another method or scope is a distinct endpoint.
    let catalog = prepared(api_bundle(
        "Plugin",
        true,
        Some(&format!(
            "{}{}{}",
            api("get", "GET", "global", "/items/{id}"),
            api("put", "PUT", "global", "/items/{id}"),
            api("session", "GET", "session", "/items/{id}")
        )),
    ));
    assert_eq!(catalog.bundle_apis("hya/api-demo").len(), 3);
}

#[test]
fn schema_files_must_be_declared_json_files() {
    let with = |field: &str, path: &str| {
        format!(
            "  - id: x\n    method: POST\n    scope: global\n    path: /x\n    {field}: {path}\n"
        )
    };
    let catalog = prepared(api_bundle(
        "Plugin",
        true,
        Some(&with("request_schema", "./schemas/in.json")),
    ));
    assert_eq!(
        catalog.bundle_apis("hya/api-demo")[0]
            .request_schema
            .as_deref(),
        Some("schemas/in.json"),
        "schema paths are normalized"
    );
    let detail = invalid_detail(api_bundle(
        "Plugin",
        true,
        Some(&with("request_schema", "schemas/missing.json")),
    ));
    assert!(
        detail.contains("request_schema") && detail.contains("extensions"),
        "{detail}"
    );
    let detail = invalid_detail(api_bundle(
        "Plugin",
        true,
        Some(&with("response_schema", "schemas/bad.json")),
    ));
    assert!(
        detail.contains("response_schema") && detail.contains("JSON"),
        "{detail}"
    );
}

#[test]
fn non_canonical_api_rows_are_rejected_on_decode() {
    let catalog = prepared(api_bundle(
        "Plugin",
        true,
        Some(&format!(
            "{}{}",
            api("alpha", "GET", "session", "/a"),
            api("usage", "GET", "session", "/usage")
        )),
    ));
    let mut document: serde_json::Value = match serde_json::from_slice(catalog.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    let Some(apis) = document["apis"][0]["apis"].as_array_mut() else {
        panic!("apis row must be an array");
    };
    apis.reverse();
    let bytes = match serde_json::to_vec(&document) {
        Ok(bytes) => bytes,
        Err(error) => panic!("encode tampered document: {error}"),
    };
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert!(matches!(
        PreparedCatalog::decode(&bytes, &digest),
        Err(BundleError::NonCanonicalPreparedCatalog)
    ));
}

#[test]
fn agent_bundle_with_explicit_process_may_declare_apis() {
    let manifest = r#"kind: AgentBundle
identity:
  id: hya/decl-demo
  version: 1.0.0
  publisher: hya
apis:
  - { id: usage, method: GET, scope: session, path: /usage, description: Token usage }
  - { id: health, method: GET, scope: global, path: /health }
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
  process:
    kind: bun
    command: [bun, run, extensions/runtime.ts]
agent:
  id: decl-lead
  role: main
"#;
    let catalog = prepared(BundleSource::new(
        "decl-demo",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec()),
            SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
        ],
    ));
    let ids = catalog
        .bundle_apis("hya/decl-demo")
        .iter()
        .map(|api| api.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["health", "usage"]);
}
