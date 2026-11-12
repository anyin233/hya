//! Manifest `permission_modes:` declarations: session permission modes whose
//! asks a bundle's explicit `extensions.process` approves through the
//! `permission.approve` hook.

use hya_bundle::{BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package};
use sha2::{Digest, Sha256};

/// One bundle source of `kind` with an optional `extensions.process`, an
/// optional `permission.approve` hook resource, and an optional
/// `permission_modes` block (a YAML fragment indented for its parent key).
fn mode_bundle(with_agents: bool, process: bool, hook: bool, modes: Option<&str>) -> BundleSource {
    let process = if process {
        "extensions:\n  process:\n    kind: bun\n    command: [bun, run, '${BUNDLE_ROOT}/approver.ts']\n  files:\n    - id: approver\n      path: approver.ts\n"
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
        "kind: Bundle\nidentity:\n  id: hya/approver\n  version: 1.0.0\n  publisher: hya\n{process}{hook}{modes}{body}"
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

fn mode(id: &str, title: &str) -> String {
    format!("  - {{ id: {id}, title: '{title}' }}\n")
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

fn document(catalog: &PreparedCatalog) -> serde_json::Value {
    match serde_json::from_slice(catalog.bytes()) {
        Ok(document) => document,
        Err(error) => panic!("prepared bytes must be JSON: {error}"),
    }
}

#[test]
fn plugin_permission_modes_prepare_sorted_and_round_trip() {
    let block = format!(
        "  - id: careful\n    title: Careful\n    description: Approves read-only commands\n{}",
        mode("ci", "CI")
    );
    let catalog = prepared(mode_bundle(false, true, true, Some(&block)));
    let modes = catalog.bundle_permission_modes("hya/approver");
    let ids = modes
        .iter()
        .map(|mode| mode.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["careful", "ci"], "modes are sorted by id");
    assert_eq!(modes[0].title, "Careful");
    assert_eq!(modes[0].description, "Approves read-only commands");
    assert_eq!(modes[1].description, "", "description defaults to empty");
    assert!(catalog.bundle_permission_modes("hya/other").is_empty());

    let decoded = match PreparedCatalog::decode(catalog.bytes(), catalog.digest()) {
        Ok(decoded) => decoded,
        Err(error) => panic!("permission modes must survive decode: {error:?}"),
    };
    assert_eq!(decoded.bundle_permission_modes("hya/approver"), modes);

    let document = document(&catalog);
    assert_eq!(document["permission_modes"][1]["id"], "ci");
}

#[test]
fn permission_modes_change_the_digest_and_are_skipped_when_empty() {
    let plain = prepared(mode_bundle(false, true, true, None));
    let with_modes = prepared(mode_bundle(
        false,
        true,
        true,
        Some(&mode("careful", "Careful")),
    ));
    assert_ne!(plain.digest(), with_modes.digest());
    assert!(
        document(&plain)["permission_modes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "the section is skipped when no bundle declares a mode"
    );
    let a = mode("a", "A");
    let b = mode("b", "B");
    let reordered = prepared(mode_bundle(false, true, true, Some(&format!("{b}{a}"))));
    let sorted = prepared(mode_bundle(false, true, true, Some(&format!("{a}{b}"))));
    assert_eq!(reordered.digest(), sorted.digest());
}

#[test]
fn agent_bearing_bundle_may_declare_permission_modes() {
    let catalog = prepared(mode_bundle(
        true,
        true,
        true,
        Some(&mode("careful", "Careful")),
    ));
    assert_eq!(catalog.bundle_permission_modes("hya/approver").len(), 1);
}

#[test]
fn permission_modes_require_a_process_and_the_approve_hook() {
    let detail = invalid_detail(mode_bundle(
        false,
        false,
        false,
        Some(&mode("careful", "Careful")),
    ));
    assert!(
        detail.contains("permission_modes") && detail.contains("extensions.process"),
        "the diagnostic must name the missing process: {detail}"
    );
    let detail = invalid_detail(mode_bundle(
        false,
        true,
        false,
        Some(&mode("careful", "Careful")),
    ));
    assert!(
        detail.contains("permission_modes") && detail.contains("permission.approve"),
        "the diagnostic must name the missing hook: {detail}"
    );
}

#[test]
fn invalid_ids_titles_duplicates_and_counts_are_rejected() {
    for (block, needle) in [
        (mode("-bad", "Bad"), "id `-bad`"),
        (mode("a/b", "Slash"), "id `a/b`"),
        (mode(&"x".repeat(65), "Long"), "at most 64"),
        (mode("manual", "Reserved"), "reserved"),
        (mode("yolo", "Reserved"), "reserved"),
        (
            format!("{}{}", mode("dup", "One"), mode("dup", "Two")),
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
                .map(|i| mode(&format!("m{i}"), "M"))
                .collect::<String>(),
            "at most 16",
        ),
    ] {
        let detail = invalid_detail(mode_bundle(false, true, true, Some(&block)));
        assert!(
            detail.contains("permission_modes") && detail.contains(needle),
            "`{needle}` expected in: {detail}"
        );
    }
}

#[test]
fn unknown_mode_fields_are_rejected() {
    let source = mode_bundle(
        false,
        true,
        true,
        Some("  - { id: careful, title: Careful, allow: all }\n"),
    );
    assert!(prepare_package(source).is_err());
}

#[test]
fn non_canonical_permission_mode_rows_are_rejected_on_decode() {
    let catalog = prepared(mode_bundle(
        false,
        true,
        true,
        Some(&format!("{}{}", mode("alpha", "A"), mode("beta", "B"))),
    ));
    let mut document = document(&catalog);
    let Some(modes) = document["permission_modes"].as_array_mut() else {
        panic!("modes row must be an array");
    };
    modes.reverse();
    let mut content = document.clone();
    content
        .as_object_mut()
        .unwrap_or_else(|| panic!("prepared bundle must be an object"))
        .remove("digest");
    document["digest"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&content).unwrap_or_else(|error| panic!("encode bundle: {error}"))
        )
    ));
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
fn permission_approve_is_an_allowed_hook_id() {
    // A process bundle may declare the hook without declaring a mode.
    let catalog = prepared(mode_bundle(false, true, true, None));
    assert_eq!(
        catalog.bundles()[0].hooks()[0].local_id,
        "permission.approve"
    );
}
