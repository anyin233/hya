//! Package preparation and canonical round-tripping of the prepared catalog.

use hya_bundle::{
    BundleSource, PreparedCatalog, SourceFile, inspect_public_package, prepare_package,
    write_public_package,
};

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
---
You are the public package lead.
"#,
        )],
    )
}

fn workflow_package_source() -> BundleSource {
    BundleSource::new(
        "workflow-public-package",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: WorkflowBundle
identity:
  id: hya/workflow-public
  version: 1.0.0
  publisher: hya
workflow:
  id: demo
  path: workflows/demo.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
"#,
            ),
            SourceFile::new(
                "workflows/demo.hya.md",
                br#"---
kind: Workflow
name: demo
description: Public Workflow.
nodes:
  run:
    agent: worker
    directive: Run the stage.
---
flowchart TD
  run
"#,
            ),
            SourceFile::new("prompts/worker.md", b"Run the stage.\n"),
        ],
    )
}

#[test]
fn workflow_public_writer_round_trips_compiled_source_and_agents() {
    let source = workflow_package_source();
    let bytes = write_public_package(&source);
    let Ok(bytes) = bytes else {
        panic!("Workflow public package writer failed: {bytes:?}");
    };
    let inspected = inspect_public_package(&bytes);
    let Ok(inspected) = inspected else {
        panic!("Workflow public package inspection failed: {inspected:?}");
    };
    let [bundle] = inspected.bundles() else {
        panic!("Workflow package must contain one payload");
    };
    assert_eq!(bundle.kind().as_str(), "WorkflowBundle");
    assert_eq!(
        bundle.workflow().map(|workflow| workflow.id.as_str()),
        Some("demo")
    );
    assert_eq!(bundle.agents().len(), 1);
    assert_eq!(bundle.agents()[0].id.as_str(), "worker");
}

#[test]
fn public_package_source_prepares_as_an_installed_mutable_origin() {
    let source = public_package_source();

    let prepared = prepare_package(source);
    let Ok(prepared) = prepared else {
        panic!("public package preparation failed: {prepared:?}");
    };
    assert_eq!(prepared.bundles().len(), 1);
}

#[test]
fn deterministic_public_writer_round_trips_declared_closure() {
    let source = public_package_source();
    let first = write_public_package(&source);
    let Ok(first) = first else {
        panic!("public package writer failed: {first:?}");
    };
    let second = write_public_package(&source);
    let Ok(second) = second else {
        panic!("second public package writer call failed: {second:?}");
    };
    assert_eq!(first, second);
    let inspected = inspect_public_package(&first);
    let Ok(inspected) = inspected else {
        panic!("written public package failed inspection: {inspected:?}");
    };
    let prepared = prepare_package(source);
    let Ok(prepared) = prepared else {
        panic!("source preparation failed: {prepared:?}");
    };
    assert_eq!(inspected.bytes(), prepared.bytes());
    assert_eq!(inspected.digest(), prepared.digest());
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
}

/// The checked-in Track P fixture package (`crates/hya-e2e/tests/fixtures/
/// schema_demo.hyabundle`) must be byte-identical to what the production
/// preparer emits from the same source, keeping the e2e scenario reproducible.
#[test]
fn e2e_schema_demo_fixture_matches_the_production_package_bytes() {
    fn schema_demo_source() -> BundleSource {
        BundleSource::new(
            "schema-demo",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    br#"kind: AgentBundle
identity:
  id: hya/schema-demo
  version: 1.0.0
  publisher: hya
schemas:
  - scheme: db
    tool: query
    writable: false
resources:
  tools:
    - id: query
      path: extensions/runtime.js
  mcp:
    - id: vecdb
      path: mcp/vecdb.json
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
  process:
    kind: bun
    command: [bun, schema-demo-worker]
agent:
  id: schema-lead
  role: main
  resource_view:
    allow:
      - query
      - runtime
"#,
                ),
                SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
                SourceFile::new(
                    "mcp/vecdb.json",
                    br#"{"command": ["python3", "vecdb.py"]}"#.to_vec(),
                ),
            ],
        )
    }

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hya-e2e/tests/fixtures/schema_demo.hyabundle");
    let bytes = write_public_package(&schema_demo_source())
        .unwrap_or_else(|error| panic!("schema demo package must build: {error:?}"));
    if !fixture.exists() {
        if let Some(parent) = fixture.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create fixture dir: {error}"));
        }
        std::fs::write(&fixture, &bytes)
            .unwrap_or_else(|error| panic!("write fixture {}: {error}", fixture.display()));
    }
    let checked_in = std::fs::read(&fixture)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", fixture.display()));
    assert_eq!(
        checked_in,
        bytes,
        "fixture {} drifted from the production package bytes; regenerate it",
        fixture.display()
    );
}
