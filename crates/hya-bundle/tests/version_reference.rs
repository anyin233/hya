//! Bundle source version-reference regression tests.

use hya_bundle::{BundleSource, SourceFile, prepare_package};
use hya_version::VERSION;

#[test]
fn resolves_hya_and_legacy_backend_version_references_in_bundle_source() {
    for reference in ["hya", "backend"] {
        let source = BundleSource::new(
            "version-reference",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nversion_ref: {reference}\nidentity:\n  id: hya/version-reference\n  version: 0.0.0\n  publisher: hya\n"
                ),
            )],
        );
        let Ok(prepared) = prepare_package(source) else {
            panic!("{reference} version reference should resolve");
        };
        assert_eq!(prepared.bundles()[0].identity().version, VERSION);
    }
}
