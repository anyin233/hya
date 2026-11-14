//! Model-facing tool selection, handoff policy, and bundle process request timeout contracts.
#![allow(clippy::expect_used)]

use hya_bundle::{
    BundleError, BundleSource, HandoffPolicy, PreparedCatalog, SourceFile, prepare_package,
};

fn source(agent_fields: &str, process_fields: &str) -> BundleSource {
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
fn manifest_fields_prepare_and_round_trip() {
    for timeout in [1000, 3_600_000] {
        let prepared = prepare_package(source(
            ", advertise: [read, bash, read]",
            &format!(", timeout_ms: {timeout}"),
        ))
        .expect("new manifest fields accepted");
        let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())
            .expect("new fields decode canonically");
        assert_eq!(decoded.bundles()[0].agents()[0].advertise, ["bash", "read"]);
        assert_eq!(
            decoded
                .bundle_process("acme/legacy")
                .expect("process")
                .timeout_ms,
            Some(timeout)
        );
        assert_eq!(decoded.bytes(), prepared.bytes());
    }
}

#[test]
fn out_of_range_and_non_integer_timeouts_are_rejected() {
    for value in ["0", "999", "3600001", "-1", "1.5", "'1000'", "[]"] {
        let error = prepare_package(source("", &format!(", timeout_ms: {value}")))
            .expect_err("invalid timeout rejected");
        assert!(
            matches!(error, BundleError::InvalidManifest { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("timeout_ms"), "{error}");
    }
}

#[test]
fn malformed_advertise_is_rejected() {
    for value in ["bash", "{}", "[{}]", "[[]]", "[1]", "[true]"] {
        let error = prepare_package(source(&format!(", advertise: {value}"), ""))
            .expect_err("advertise must be a list of tool name strings");
        assert!(
            matches!(error, BundleError::InvalidManifest { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("advertise"), "{error}");
    }
}

#[test]
fn legacy_manifest_prepares_to_identical_bytes() {
    let prepared = prepare_package(source("", "")).expect("legacy manifest");
    assert_eq!(
        prepared.bytes(),
        include_bytes!("fixtures/legacy_manifest.json")
    );
    assert_eq!(
        prepared.digest(),
        "c1d2fd35397977b19174e2360fd9895163a68919ed368964a7cd5c452b5e042d"
    );
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())
        .expect("pre-change prepared catalog decodes");
    assert!(decoded.bundles()[0].agents()[0].advertise.is_empty());
    assert_eq!(
        decoded
            .bundle_process("acme/legacy")
            .expect("process")
            .timeout_ms,
        None
    );
    let empty = prepare_package(source(", advertise: []", "")).expect("empty means unrestricted");
    assert_eq!(prepared.bytes(), empty.bytes());
}

#[test]
fn handoff_policy_prepares_and_round_trips() {
    let legacy = prepare_package(source("", "")).expect("legacy manifest");
    let model = prepare_package(source(", handoff: model", "")).expect("handoff: model accepted");
    assert_eq!(
        model.bytes(),
        legacy.bytes(),
        "the default is not serialized"
    );
    let decoded = PreparedCatalog::decode(legacy.bytes(), legacy.digest()).expect("legacy decodes");
    assert_eq!(
        decoded.bundles()[0].agents()[0].handoff,
        HandoffPolicy::Model
    );

    let deterministic = prepare_package(source(", handoff: deterministic", ""))
        .expect("handoff: deterministic accepted");
    let decoded = PreparedCatalog::decode(deterministic.bytes(), deterministic.digest())
        .expect("deterministic policy decodes canonically");
    assert_eq!(
        decoded.bundles()[0].agents()[0].handoff,
        HandoffPolicy::Deterministic
    );
    assert_eq!(decoded.bytes(), deterministic.bytes());
}

#[test]
fn unknown_handoff_policy_is_rejected() {
    for value in ["summarizer", "Deterministic", "''", "[]", "1"] {
        let error = prepare_package(source(&format!(", handoff: {value}"), ""))
            .expect_err("unknown handoff policy rejected");
        assert!(
            matches!(error, BundleError::InvalidManifest { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("handoff"), "{error}");
    }
}
