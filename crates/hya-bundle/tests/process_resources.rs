//! Process-backed resource closure and hook contracts.
#![allow(clippy::expect_used)]
use hya_bundle::{
    BundleSource, PreparedCatalog, SourceFile, inspect_public_package, prepare_package,
    write_public_package,
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

#[test]
fn native_process_bundle_packages_non_utf8_executable_bytes() {
    let executable = vec![0x7f, b'E', b'L', b'F', 0, 0xff, 0x80];
    let source = BundleSource::new(
        "native-executable",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/native, version: 1.0.0, publisher: acme }
extensions:
  rust: [{ id: provider, path: bin/provider }]
  process: { kind: rust, command: ['${BUNDLE_ROOT}/bin/provider'] }
resources:
  tools: [{ id: echo, path: declarations.json }]
"#,
            ),
            SourceFile::new("bin/provider", executable.clone()),
            SourceFile::new("declarations.json", b"{}"),
        ],
    );

    let prepared = prepare_package(source.clone()).expect("prepare native executable");
    assert_eq!(prepared.bundles()[0].extensions().len(), 1);
    assert_eq!(
        prepared.bundles()[0].extensions()[0]
            .source_bytes()
            .expect("decode executable"),
        executable
    );
    PreparedCatalog::decode(prepared.bytes(), prepared.digest()).expect("decode native executable");
    let package = write_public_package(&source).expect("package native executable");
    let inspected = inspect_public_package(&package).expect("inspect native executable package");
    assert_eq!(inspected.bytes(), prepared.bytes());
}

#[test]
fn native_library_bundle_packages_raw_bytes_without_a_process() {
    let library = vec![0xcf, 0xfa, 0xed, 0xfe, 0, 0xff, 0x80];
    let source = BundleSource::new(
        "native-library",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: hya/todo-tools, version: 1.0.0, publisher: hya }
extensions:
  libraries: [{ id: runtime, path: native/libhya_todo_tools.dylib }]
"#,
            ),
            SourceFile::new("native/libhya_todo_tools.dylib", library.clone()),
        ],
    );
    let prepared = prepare_package(source.clone()).expect("prepare native library");
    assert!(prepared.process_extensions().is_empty());
    let resource = &prepared.bundles()[0].extensions()[0];
    assert!(resource.stable_id.contains("/library/runtime"));
    assert_eq!(resource.source_bytes().expect("decode library"), library);
    let package = write_public_package(&source).expect("package native library");
    let inspected = inspect_public_package(&package).expect("inspect native library");
    assert_eq!(inspected.bytes(), prepared.bytes());
}
