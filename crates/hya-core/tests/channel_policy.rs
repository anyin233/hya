//! Runtime channel policy derives trusted defaults and intersects bundle restrictions.
#![allow(clippy::expect_used)]

use hya_bundle::{
    BundleCatalog, BundleSource, ChannelCapability, ChannelParticipantRole, SourceFile,
    prepare_package,
};
use hya_core::ChannelPolicy;

fn catalog(channels: &str) -> BundleCatalog {
    catalog_with_id("acme/restrict", channels)
}

fn catalog_with_id(identity: &str, channels: &str) -> BundleCatalog {
    let source = BundleSource::new(
        "restriction",
        vec![SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: AgentSetBundle\nidentity: {{ id: {identity}, version: 1.0.0, publisher: acme }}\nagents: [{{id: reviewer, role: subagent}}]\nchannels:\n{channels}\n"
            ),
        )],
    );
    let prepared = prepare_package(source).expect("prepare restriction");
    BundleCatalog::from_prepared(prepared.bundles()).expect("catalog")
}

#[test]
fn installed_bundle_cannot_gain_trust_by_reusing_preset_identity() {
    let policy = ChannelPolicy::from_bundle_catalog(&catalog_with_id(
        "hya/agent-channels",
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .expect("policy");
    assert!(!policy.allows_parent_dm(
        ChannelCapability::Report,
        "reviewer",
        ChannelParticipantRole::Child
    ));
}

#[test]
fn captured_snapshot_is_unchanged_when_a_new_catalog_is_loaded() {
    let admitted =
        ChannelPolicy::from_bundle_catalog(&BundleCatalog::from_prepared(&[]).expect("empty"))
            .expect("admitted policy")
            .snapshot_for("reviewer");
    let refreshed = ChannelPolicy::from_bundle_catalog(&catalog(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .expect("refreshed policy")
    .snapshot_for("reviewer");

    assert_ne!(admitted.dm_child & 0b1_1111, 0);
    assert_eq!(refreshed.dm_child, 0);
    assert_ne!(
        admitted.dm_child & 0b1_1111,
        0,
        "admitted snapshot stays pinned"
    );
}

#[test]
fn absent_declaration_uses_trusted_preset_defaults() {
    let catalog = BundleCatalog::from_prepared(&[]).expect("empty catalog");
    let policy = ChannelPolicy::from_bundle_catalog(&catalog).expect("trusted preset");
    assert!(policy.allows_unit(
        ChannelCapability::Send,
        "anything",
        ChannelParticipantRole::UnitLeader
    ));
    assert!(policy.allows_parent_dm(
        ChannelCapability::Report,
        "anything",
        ChannelParticipantRole::Child
    ));
}

#[test]
fn applicable_bundle_policy_only_restricts_and_empty_denies() {
    let policy = ChannelPolicy::from_bundle_catalog(&catalog(
        "  - { id: unit, kind: unit, participants: [{kind: agent, agent: reviewer}], capabilities: [send], scope: unit, retention: team_session }\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .expect("policy");
    assert!(policy.allows_unit(
        ChannelCapability::Send,
        "reviewer",
        ChannelParticipantRole::DirectReports
    ));
    assert!(!policy.allows_unit(
        ChannelCapability::Steer,
        "reviewer",
        ChannelParticipantRole::DirectReports
    ));
    assert!(policy.allows_unit(
        ChannelCapability::Steer,
        "other",
        ChannelParticipantRole::DirectReports
    ));
    assert!(!policy.allows_parent_dm(
        ChannelCapability::Report,
        "other",
        ChannelParticipantRole::Child
    ));
    assert!(policy.allows_parent_dm(
        ChannelCapability::Report,
        "other",
        ChannelParticipantRole::Parent
    ));
}
