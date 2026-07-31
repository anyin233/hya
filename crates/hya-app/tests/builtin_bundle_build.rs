use sha2::{Digest, Sha256};

const PREPARED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/builtin-bundles.json"));
const EXPECTED_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/builtin-bundles.sha256"));

fn digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        assert!(write!(encoded, "{byte:02x}").is_ok());
    }
    encoded
}

#[test]
fn app_build_prepares_the_canonical_builtin_catalog_without_runtime_loading() {
    assert_eq!(digest(PREPARED), EXPECTED_DIGEST);
    let document = serde_json::from_slice::<serde_json::Value>(PREPARED);
    let Ok(document) = document else {
        panic!("prepared builtin catalog is not JSON: {document:?}");
    };
    assert_eq!(
        document["bundles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|bundle| bundle["identity"]["id"].as_str())
            .collect::<Vec<_>>(),
        ["hya/core-agents", "hya/development"]
    );
}
