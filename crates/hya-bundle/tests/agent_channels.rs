//! Declarative AgentSet channel-template preparation and validation.
#![allow(clippy::expect_used)]

use hya_bundle::{
    BundleCatalog, BundleSource, ChannelCapability, ChannelParticipantRole, ChannelScope,
    ChannelTemplateKind, PreparedCatalog, PreparedChannelParticipant, SourceFile, prepare_package,
};
use sha2::{Digest, Sha256};

fn source(channels: &str, agents: &str) -> BundleSource {
    BundleSource::new(
        "channels",
        vec![SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: AgentSetBundle\nidentity: {{ id: hya/agent-channels, version: 1.0.0, publisher: hya }}\nagents: {agents}\nchannels:\n{channels}\n"
            ),
        )],
    )
}

#[test]
fn channel_only_agent_set_prepares_and_round_trips_canonically() {
    let channels = r#"
  - id: unit-default
    kind: unit
    participants:
      - { kind: role, role: direct_reports }
      - { kind: role, role: unit_leader }
    capabilities: [steer, send, resident_mail, follow_up]
    scope: unit
    retention: team_session
  - id: parent-dm-default
    kind: parent_dm
    participants:
      - { kind: role, role: child }
      - { kind: role, role: parent }
    capabilities: [steer, send, report, resident_mail, follow_up]
    scope: vertical
    retention: team_session"#;
    let prepared = prepare_package(source(channels, "[]")).expect("prepare channel preset");
    let bundle = prepared.bundles()[0]
        .agent_set_bundle()
        .expect("agent set payload");
    assert!(bundle.agents.is_empty());
    assert_eq!(bundle.channels.len(), 2);
    assert_eq!(bundle.channels[0].kind, ChannelTemplateKind::ParentDm);
    assert_eq!(bundle.channels[0].scope, ChannelScope::Vertical);
    assert_eq!(bundle.channels[0].capabilities[0], ChannelCapability::Send);
    assert_eq!(
        bundle.channels[0].participants[0],
        PreparedChannelParticipant::Role {
            role: ChannelParticipantRole::Parent
        }
    );

    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())
        .expect("prepared channel policy round-trip");
    let catalog = BundleCatalog::from_prepared(decoded.bundles()).expect("catalog");
    assert_eq!(catalog.channels_for_bundle("hya/agent-channels").len(), 2);
    assert_eq!(
        catalog
            .resolve_channel_template("hya/agent-channels", "unit-default")
            .map(|row| row.kind),
        Some(ChannelTemplateKind::Unit)
    );
}

#[test]
fn channel_contract_rejects_unknown_duplicates_types_and_references() {
    let cases = [
        ("", "[]"),
        (
            "  - { id: same, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }\n  - { id: same, kind: unit, participants: [{kind: role, role: direct_reports}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: one, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }\n  - { id: two, kind: unit, participants: [{kind: role, role: direct_reports}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: broadcast, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: parent}], capabilities: [], scope: unit, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: parent_dm, participants: [{kind: role, role: parent}], capabilities: [send, send], scope: vertical, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: parent_dm, participants: [{kind: agent, agent: missing}], capabilities: [], scope: vertical, retention: team_session }",
            "[{id: actual, role: main}]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: vertical, retention: team_session }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: forever }",
            "[]",
        ),
        (
            "  - { id: bad, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [], scope: unit, retention: team_session, surprise: true }",
            "[]",
        ),
    ];
    for (channels, agents) in cases {
        assert!(
            prepare_package(source(channels, agents)).is_err(),
            "accepted {channels}"
        );
    }
}

#[test]
fn agent_set_without_agents_or_channels_still_rejects() {
    let source = BundleSource::new(
        "empty",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: AgentSetBundle\nidentity: { id: acme/empty, version: 1.0.0, publisher: acme }\nagents: []\n",
        )],
    );
    assert!(prepare_package(source).is_err());
}

#[test]
fn prepared_decode_rejects_noncanonical_channel_policy_with_valid_outer_digest() {
    let prepared = prepare_package(source(
        "  - { id: unit, kind: unit, participants: [{kind: role, role: unit_leader}], capabilities: [send], scope: unit, retention: team_session }",
        "[]",
    ))
    .expect("prepare source");
    let mut document: serde_json::Value =
        serde_json::from_slice(prepared.bytes()).expect("prepared JSON");
    document["bundles"][0]["channels"][0]["capabilities"] = serde_json::json!(["send", "send"]);
    let forged = serde_json::to_vec(&document).expect("encode forged catalog");
    let digest = format!("{:x}", Sha256::digest(&forged));
    assert!(PreparedCatalog::decode(&forged, &digest).is_err());
}

#[test]
fn first_party_agent_channel_preset_is_a_channel_only_agent_set() {
    let source = BundleSource::read_directory(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundles/presets/agent-channels"),
    )
    .expect("read first-party preset");
    let prepared = prepare_package(source).expect("prepare first-party preset");
    let bundle = prepared.bundles()[0]
        .agent_set_bundle()
        .expect("agent channel preset kind");
    assert_eq!(bundle.identity.id, "hya/agent-channels");
    assert!(bundle.agents.is_empty());
    assert_eq!(bundle.channels.len(), 2);
}
