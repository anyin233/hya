//! Package preparation and canonical round-tripping of the prepared catalog.

use hya_bundle::{BundleSource, PreparedCatalog, SourceFile, prepare_package};

fn public_package_source() -> BundleSource {
    BundleSource::new(
        "public-package",
        vec![SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/public-package
  version: 1.0.0
  publisher: hya
agent:
  id: public-package-lead
  role: main
  spawn_lifecycle: transient
---
You are the public package lead.
"#,
        )],
    )
}

#[test]
fn public_package_source_reuses_v1_preparer_as_installed_mutable_origin() {
    let source = public_package_source();

    let prepared = prepare_package(source);
    let Ok(prepared) = prepared else {
        panic!("public package preparation failed: {prepared:?}");
    };
    assert_eq!(prepared.bundles().len(), 1);
    let bundle = &prepared.bundles()[0];
}

#[test]
fn installed_prepared_catalog_round_trips_canonically() {
    let prepared = prepare_package(public_package_source());
    let Ok(prepared) = prepared else {
        panic!("public package preparation failed: {prepared:?}");
    };
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest());
    let Ok(decoded) = decoded else {
        panic!("installed prepared catalog decode failed: {decoded:?}");
    };
    assert_eq!(decoded.bundles().len(), 1);
    let bundle = &decoded.bundles()[0];
}
