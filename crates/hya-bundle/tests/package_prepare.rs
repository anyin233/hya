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
  spawn_lifecycle: transient
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
    spawn_lifecycle: transient
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
