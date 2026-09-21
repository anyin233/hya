//! Namespace declaration and validation for prepared bundles.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_bundle::{
    BundleError, BundleSource, PreparedInstallableBundle, SourceFile, prepare_package,
};

fn agent_source(root: &str, manifest: &str) -> BundleSource {
    let mut files = vec![SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec())];
    if manifest.contains("prompt:") {
        files.push(SourceFile::new(
            "prompts/lead.md",
            b"You are the lead.\n".to_vec(),
        ));
    }
    BundleSource::new(root, files)
}

fn manifest_with_namespace(namespace: Option<&str>, id: &str) -> String {
    let namespace_line = namespace
        .map(|value| format!("namespace: {value}\n"))
        .unwrap_or_default();
    format!(
        r#"kind: AgentBundle
identity:
  id: {id}
  version: 1.0.0
  publisher: hya
{namespace_line}agent:
  id: lead
  role: main
  prompt: prompts/lead.md
  spawn_lifecycle: transient
"#
    )
}

fn first_namespace(catalog: &hya_bundle::PreparedCatalog) -> Option<&str> {
    match &catalog.bundles()[0] {
        PreparedInstallableBundle::Agent(bundle) => bundle.namespace.as_deref(),
        PreparedInstallableBundle::Workflow(bundle) => bundle.namespace.as_deref(),
    }
}

#[test]
fn namespace_defaults_to_identity_name_segment() {
    let catalog = prepare_package(agent_source(
        "ns-default",
        &manifest_with_namespace(None, "hya/acme-tools"),
    ))
    .expect("prepare without namespace");
    assert_eq!(first_namespace(&catalog), Some("acme-tools"));
}

#[test]
fn declared_namespace_is_respected() {
    let catalog = prepare_package(agent_source(
        "ns-declared",
        &manifest_with_namespace(Some("acme"), "hya/acme-tools"),
    ))
    .expect("prepare with namespace");
    assert_eq!(first_namespace(&catalog), Some("acme"));
}

#[test]
fn empty_namespace_declares_the_identity_default() {
    // Declaring an empty namespace is the explicit spelling of "use the
    // default"; it resolves to the identity name segment like omission.
    let catalog = prepare_package(agent_source(
        "ns-empty",
        &manifest_with_namespace(Some(""), "hya/acme-tools"),
    ))
    .expect("prepare with empty namespace");
    assert_eq!(first_namespace(&catalog), Some("acme-tools"));
}

#[test]
fn invalid_namespace_token_is_rejected() {
    for bad in ["bad__ns", "has.dot", "has/slash", "has space", "__lead"] {
        let result = prepare_package(agent_source(
            "ns-invalid",
            &manifest_with_namespace(Some(bad), "hya/acme-tools"),
        ));
        let Some(BundleError::InvalidNamespace { namespace, .. }) = result.err() else {
            panic!("namespace `{bad}` must be rejected");
        };
        assert_eq!(namespace, bad);
    }
}

#[test]
fn reserved_namespace_is_rejected() {
    for reserved in ["mcp", "harness", "builtin", "plugin"] {
        let result = prepare_package(agent_source(
            "ns-reserved",
            &manifest_with_namespace(Some(reserved), "hya/acme-tools"),
        ));
        assert!(
            matches!(result.err(), Some(BundleError::InvalidNamespace { .. })),
            "reserved namespace `{reserved}` must be rejected"
        );
    }
}

#[test]
fn invalid_default_name_segment_requires_explicit_namespace() {
    // `.` is legal in an identity id but not in a namespace token; the
    // default resolution must fail with guidance instead of silently
    // producing an unusable namespace.
    let result = prepare_package(agent_source(
        "ns-default-invalid",
        &manifest_with_namespace(None, "hya/my.tool"),
    ));
    assert!(matches!(
        result.err(),
        Some(BundleError::InvalidNamespace { .. })
    ));
}

#[test]
fn workflow_bundle_namespace_resolves_like_agent_bundle() {
    let manifest = r#"kind: WorkflowBundle
identity:
  id: hya/flow-pack
  version: 1.0.0
  publisher: hya
namespace: flow
workflow:
  id: demo
  path: workflows/demo.hya.md
agents:
  - id: worker
    role: subagent
    prompt: prompts/worker.md
    spawn_lifecycle: transient
"#;
    let files = vec![
        SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec()),
        SourceFile::new(
            "workflows/demo.hya.md",
            b"---\nkind: Workflow\nname: demo\ndescription: demo\nnodes:\n  execute:\n    agent: worker\n    directive: Execute the request.\n---\nflowchart TD\n  execute\n".to_vec(),
        ),
        SourceFile::new("prompts/worker.md", b"Work.\n".to_vec()),
    ];
    let catalog = prepare_package(BundleSource::new("ns-flow", files)).expect("prepare");
    assert_eq!(first_namespace(&catalog), Some("flow"));
}
