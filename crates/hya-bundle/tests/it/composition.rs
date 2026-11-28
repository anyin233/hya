//! Bundle-level composition contract: `requires`, `default_agent`, `can_spawn: ["*"]`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use hya_bundle::{
    Bundle, BundleCatalog, BundleError, BundleSource, PreparedCatalog, SourceFile,
    missing_prerequisites, prepare_package,
};

fn prepare(id: &str, body: &str) -> Result<PreparedCatalog, BundleError> {
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity: {{ id: {id}, version: 1.0.0, publisher: {} }}\n{body}",
        id.split('/').next().unwrap()
    );
    prepare_package(BundleSource::new(
        id,
        vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
    ))
}

fn bundle(id: &str, body: &str) -> Bundle {
    prepare(id, body)
        .unwrap_or_else(|error| panic!("prepare {id}: {error}"))
        .bundles()[0]
        .clone()
}

fn invalid_detail(result: Result<PreparedCatalog, BundleError>) -> String {
    match result {
        Err(BundleError::InvalidManifest { detail, .. }) => detail,
        other => panic!("expected InvalidManifest, got {other:?}"),
    }
}

#[test]
fn requires_is_sorted_deduped_and_rejects_self_and_malformed_ids() {
    let prepared = bundle(
        "acme/app",
        "requires: [hya/multi-agent, acme/base, hya/multi-agent]\n",
    );
    assert_eq!(prepared.requires(), ["acme/base", "hya/multi-agent"]);

    for requires in ["[acme/app]", "[no-slash]", "[acme/a/b]", "['acme/ bad']"] {
        let detail = invalid_detail(prepare("acme/app", &format!("requires: {requires}\n")));
        assert!(detail.contains("requires entry"), "{requires}: {detail}");
    }
}

#[test]
fn default_agent_must_name_a_public_main_agent_of_the_bundle() {
    let agents = "agents:\n  - { id: lead, role: main }\n  - { id: helper, role: subagent }\n  - { id: hidden, role: main, private: true }\n";
    let prepared = bundle("acme/app", &format!("default_agent: lead\n{agents}"));
    assert_eq!(prepared.default_agent().map(|id| id.as_str()), Some("lead"));
    assert_eq!(bundle("acme/app", agents).default_agent(), None);

    for name in ["helper", "hidden", "absent"] {
        let detail = invalid_detail(prepare(
            "acme/app",
            &format!("default_agent: {name}\n{agents}"),
        ));
        assert!(detail.contains("default_agent"), "{name}: {detail}");
    }
}

#[test]
fn can_spawn_wildcard_is_accepted_alone_only_outside_workflow_bundles() {
    let prepared = bundle(
        "acme/app",
        "agents:\n  - { id: lead, role: main, can_spawn: ['*'] }\n",
    );
    assert_eq!(prepared.agents()[0].can_spawn[0].as_str(), "*");

    let detail = invalid_detail(prepare(
        "acme/app",
        "agents:\n  - { id: lead, role: main, can_spawn: ['*', helper] }\n  - { id: helper, role: subagent }\n",
    ));
    assert!(detail.contains("cannot be combined"), "{detail}");

    let workflow = prepare_package(BundleSource::new(
        "acme/flow",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/flow, version: 1.0.0, publisher: acme }\nworkflow: { id: flow, path: workflows/flow.hya.md }\nagents:\n  - { id: lead, role: subagent, prompt: prompts/lead.md, can_spawn: ['*'] }\n".as_slice(),
            ),
            SourceFile::new("prompts/lead.md", b"Lead.\n".as_slice()),
            SourceFile::new(
                "workflows/flow.hya.md",
                b"---\nkind: Workflow\nname: flow\ndescription: flow\nnodes:\n  execute:\n    agent: lead\n    directive: Work.\n---\nflowchart TD\n  execute\n".as_slice(),
            ),
        ],
    ));
    let detail = invalid_detail(workflow);
    assert!(detail.contains("Workflow"), "{detail}");
}

#[test]
fn missing_prerequisites_counts_ids_overrides_and_present_set() {
    let channels = bundle("hya/agent-channels", "");
    let agents = bundle("hya/multi-agent", "requires: [hya/agent-channels]\n");
    let flow = bundle("hya/workflow", "requires: [hya/multi-agent, hya/plan]\n");
    let replacement = bundle("acme/channels", "overrides: hya/agent-channels\n");

    assert_eq!(
        missing_prerequisites(&[&agents, &flow], &BTreeSet::new()),
        vec![
            ("hya/multi-agent", "hya/agent-channels"),
            ("hya/workflow", "hya/plan"),
        ]
    );
    assert!(missing_prerequisites(&[&channels, &agents], &BTreeSet::new()).is_empty());
    assert!(missing_prerequisites(&[&replacement, &agents], &BTreeSet::new()).is_empty());
    assert!(
        missing_prerequisites(
            &[&agents, &flow],
            &BTreeSet::from(["hya/agent-channels", "hya/plan"])
        )
        .is_empty()
    );
}

#[test]
fn a_catalog_rejects_two_default_agent_declarers() {
    let one = bundle(
        "acme/one",
        "default_agent: a\nagents:\n  - { id: a, role: main }\n",
    );
    let two = bundle(
        "acme/two",
        "default_agent: b\nagents:\n  - { id: b, role: main }\n",
    );
    let plain = bundle("acme/plain", "agents:\n  - { id: c, role: main }\n");
    assert!(BundleCatalog::from_prepared(&[one.clone(), plain]).is_ok());
    assert_eq!(
        BundleCatalog::from_prepared(&[one, two]).unwrap_err(),
        BundleError::DuplicateDefaultAgent {
            first: "acme/one".to_string(),
            second: "acme/two".to_string(),
        }
    );
}

#[test]
fn undeclared_contract_fields_stay_out_of_the_prepared_document() {
    // Absent fields are not serialized, so digests of existing bundles do not change.
    let plain = prepare("acme/plain", "agents:\n  - { id: c, role: main }\n").unwrap();
    let text = String::from_utf8(plain.bytes().to_vec()).unwrap();
    assert!(!text.contains("\"requires\""), "{text}");
    assert!(!text.contains("\"default_agent\""), "{text}");

    let declared = prepare(
        "acme/app",
        "requires: [acme/base]\ndefault_agent: c\nagents:\n  - { id: c, role: main }\n",
    )
    .unwrap();
    let reprepared = PreparedCatalog::decode(declared.bytes(), declared.digest()).unwrap();
    assert_eq!(reprepared.bundles()[0].requires(), ["acme/base"]);
}
