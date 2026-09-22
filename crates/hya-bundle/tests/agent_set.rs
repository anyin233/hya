//! Agent-set packages use the shared package, canonicalization, and catalog seams.

use hya_bundle::{
    BundleCatalog, BundleSource, PreparedCatalog, SourceFile, inspect_public_package,
    prepare_package, write_public_package,
};

fn source(agents: &str, extra: &str) -> BundleSource {
    BundleSource::new(
        "agent-set",
        vec![SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: AgentSetBundle\nidentity: {{ id: acme/team, version: 1.0.0, publisher: acme }}\nagents: {agents}\n{extra}\n"
            ),
        )],
    )
}

#[test]
fn agent_set_package_is_deterministic_and_resolves_every_member()
-> Result<(), Box<dyn std::error::Error>> {
    let first = source(
        "[{id: beta, role: subagent}, {id: alpha, role: main, can_spawn: [beta, external, beta]}]",
        "",
    );
    let second = source(
        "[{id: alpha, role: main, can_spawn: [external, beta]}, {id: beta, role: subagent}]",
        "",
    );
    let prepared = prepare_package(first.clone())?;
    assert_eq!(prepared.bytes(), prepare_package(second)?.bytes());
    let bytes = write_public_package(&first)?;
    assert_eq!(bytes, write_public_package(&first)?);
    let inspected = inspect_public_package(&bytes)?;
    assert_eq!(prepared.bytes(), inspected.bytes());
    let decoded = PreparedCatalog::decode(inspected.bytes(), inspected.digest())?;
    let catalog = BundleCatalog::from_verified_catalogs(&[&decoded])?;
    for id in ["alpha", "beta"] {
        let bare = catalog.resolve_agent(id).ok_or("missing bare member")?;
        let qualified = catalog
            .resolve_agent(&format!("bundle:acme/team/agent/{id}"))
            .ok_or("missing qualified member")?;
        assert_eq!(bare, qualified);
    }
    assert!(decoded.index()[0].workflow_ids.is_empty());
    assert!(decoded.bundles()[0].workflow().is_none());
    assert!(catalog.semantic_identity_v1().is_some());
    Ok(())
}

#[test]
fn agent_set_rejects_empty_duplicate_and_unknown_source_contracts() {
    for (agents, extra) in [
        ("[]", ""),
        ("[{id: same, role: main}, {id: same, role: subagent}]", ""),
        (
            "[{id: member, role: main}]",
            "workflow: {id: demo, path: demo.md}",
        ),
        ("[{id: member, role: main}]", "channels: []"),
        ("[{id: member, role: main, harness_access: full}]", ""),
        ("[{id: member, role: main, prompt: missing.md}]", ""),
    ] {
        assert!(
            prepare_package(source(agents, extra)).is_err(),
            "accepted {agents} / {extra}"
        );
    }
}
