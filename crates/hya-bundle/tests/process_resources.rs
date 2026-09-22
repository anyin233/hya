//! Process-backed resource closure and hook contracts.
#![allow(clippy::expect_used)]
use hya_bundle::{
    BundleSource, PreparedCatalog, SourceFile, prepare_package, write_public_package,
};

#[test]
fn process_resources_accept_native_hooks_and_declared_support_files() {
    let source = BundleSource::new(
        "process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
resources:
  tools:
    - { id: echo, path: declarations.json }
  hooks:
    - { id: goal.evaluate, path: declarations.json }
    - { id: message.user.before, path: declarations.json }
"#,
            ),
            SourceFile::new("runtime.py", b"print('fixture')\n"),
            SourceFile::new("declarations.json", b"{}"),
        ],
    );
    let prepared = prepare_package(source.clone()).expect("process closure");
    assert_eq!(prepared.bundles()[0].extensions().len(), 1);
    assert_eq!(prepared.bundles()[0].hooks().len(), 2);
    PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("canonical decode");
    write_public_package(&source).expect("package process closure");
}

#[test]
fn static_support_files_remain_inert_declared_assets() {
    let source = BundleSource::new(
        "orphan",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/orphan, version: 1.0.0, publisher: acme }
extensions:
  files: [{ id: orphan, path: orphan.py }]
"#,
            ),
            SourceFile::new("orphan.py", b"print('unused')\n"),
        ],
    );
    let prepared = prepare_package(source).expect("static file closure");
    assert!(prepared.process_extensions().is_empty());
    assert!(prepared.bundles()[0].tools().is_empty());
    assert_eq!(
        prepared.bundles()[0].extensions()[0].source_path,
        "orphan.py"
    );
}
