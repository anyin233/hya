//! Manifest `views:` declarations: read-only session views served by a
//! bundle's explicit `extensions.process`.

use hya_bundle::{BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package};
use sha2::{Digest, Sha256};

/// One bundle source of `kind` with optional `extensions.process` and `views`
/// blocks (YAML fragments indented for their parent key).
fn view_bundle(kind: &str, process: bool, views: Option<&str>) -> BundleSource {
    let process = if process {
        "extensions:\n  process:\n    kind: bun\n    command: [bun, run, '${BUNDLE_ROOT}/views.ts']\n  files:\n    - id: views\n      path: views.ts\n"
    } else {
        ""
    };
    let views = views.map_or_else(String::new, |block| format!("views:\n{block}"));
    let body = match kind {
        "AgentSetBundle" => {
            "agents:\n  - id: view-lead\n    role: main\n    spawn_lifecycle: transient\n"
        }
        _ => "",
    };
    let manifest = format!(
        "kind: {kind}\nidentity:\n  id: hya/view-demo\n  version: 1.0.0\n  publisher: hya\n{process}{views}{body}"
    );
    BundleSource::new(
        "view-demo",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("views.ts", b"// view process\n".as_slice()),
        ],
    )
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
fn plugin_views_prepare_sorted_and_round_trip() {
    let catalog = prepared(view_bundle(
        "Plugin",
        true,
        Some("  - id: usage\n    description: Token usage\n  - id: alpha\n"),
    ));
    let views = catalog.bundle_views("hya/view-demo");
    let ids = views
        .iter()
        .map(|view| view.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["alpha", "usage"], "views are sorted by id");
    assert_eq!(views[1].description, "Token usage");
    assert_eq!(views[0].description, "", "description defaults to empty");
    assert!(catalog.bundle_views("hya/other").is_empty());

    let decoded = match PreparedCatalog::decode(catalog.bytes(), catalog.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("views must survive decode: {error:?}"),
    };
    assert_eq!(decoded.bundle_views("hya/view-demo"), views);

    let document: serde_json::Value = match serde_json::from_slice(catalog.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert_eq!(
        document["views"][0]["bundle_id"], "hya/view-demo",
        "views are a document-level section keyed by bundle id"
    );
}

#[test]
fn views_change_the_catalog_digest_and_are_skipped_when_empty() {
    let plain = prepared(view_bundle("Plugin", true, None));
    let with_views = prepared(view_bundle("Plugin", true, Some("  - id: usage\n")));
    assert_ne!(plain.digest(), with_views.digest());
    let document: serde_json::Value = match serde_json::from_slice(plain.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    assert!(
        document.get("views").is_none(),
        "the views section is skipped when no bundle declares any"
    );
    // Declaration order does not change the canonical bytes.
    let reordered = prepared(view_bundle(
        "Plugin",
        true,
        Some("  - id: usage\n  - id: alpha\n"),
    ));
    let sorted = prepared(view_bundle(
        "Plugin",
        true,
        Some("  - id: alpha\n  - id: usage\n"),
    ));
    assert_eq!(reordered.digest(), sorted.digest());
}

#[test]
fn agent_set_bundle_with_explicit_process_may_declare_views() {
    let catalog = prepared(view_bundle("AgentSetBundle", true, Some("  - id: usage\n")));
    assert_eq!(catalog.bundle_views("hya/view-demo").len(), 1);
}

#[test]
fn views_without_an_explicit_process_are_rejected() {
    let detail = invalid_detail(view_bundle("Plugin", false, Some("  - id: usage\n")));
    assert!(
        detail.contains("views") && detail.contains("extensions.process"),
        "the diagnostic must name views and the missing process: {detail}"
    );
}

#[test]
fn invalid_or_duplicate_view_ids_are_rejected() {
    for id in ["''", "a/b", "-lead", "has space", "per%cent"] {
        let detail = invalid_detail(view_bundle(
            "Plugin",
            true,
            Some(&format!("  - id: {id}\n")),
        ));
        assert!(detail.contains("views"), "{id}: {detail}");
    }
    let detail = invalid_detail(view_bundle(
        "Plugin",
        true,
        Some("  - id: usage\n  - id: usage\n"),
    ));
    assert!(detail.contains("more than once"), "{detail}");
}

#[test]
fn non_canonical_view_rows_are_rejected_on_decode() {
    let catalog = prepared(view_bundle(
        "Plugin",
        true,
        Some("  - id: alpha\n  - id: usage\n"),
    ));
    let mut document: serde_json::Value = match serde_json::from_slice(catalog.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    };
    let Some(views) = document["views"][0]["views"].as_array_mut() else {
        panic!("views row must be an array");
    };
    views.reverse();
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
fn agent_bundle_with_explicit_process_may_declare_views() {
    let manifest = r#"kind: AgentBundle
identity:
  id: hya/decl-demo
  version: 1.0.0
  publisher: hya
views:
  - id: usage
    description: Token usage
  - id: health
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
  spawn_lifecycle: transient
"#;
    let catalog = prepared(BundleSource::new(
        "decl-demo",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec()),
            SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
        ],
    ));
    let ids = catalog
        .bundle_views("hya/decl-demo")
        .iter()
        .map(|view| view.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["health", "usage"]);
}
