//! Bundle source version-reference regression tests.

use hya_bundle::{BundleSource, SourceFile, prepare_package};

#[test]
fn resolves_backend_version_reference_in_bundle_source() {
    let source = BundleSource::new(
        "version-reference",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Plugin\nversion_ref: backend\nidentity:\n  id: hya/version-reference\n  version: 0.0.0\n  publisher: hya\n",
        )],
    );
    let prepared = prepare_package(source).expect("backend version reference should resolve");
    assert_eq!(prepared.bundles()[0].identity().version, "0.43.40");
}
