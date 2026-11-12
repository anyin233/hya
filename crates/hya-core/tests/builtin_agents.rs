#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Built-in agents are the trusted `hya/core-agents` preset, not AgentBundles.
//!
//! These tests pin the roster that used to live in `bundles/builtin/*/bundle.yaml`
//! so the move out of the bundle system cannot silently drop or rename an agent.

use hya_bundle::AgentRole;
use hya_core::builtin_agents::{builtin_agent_ids, core_agents_preset, preset_agent};

/// Every built-in id, in the order the roster must expose them.
const EXPECTED_IDS: &[&str] = &[
    "hya-compaction",
    "hya-main",
    "hya-plan",
    "hya-reviewer",
    "hya-scout",
    "hya-summary",
    "hya-task",
    "hya-title",
];

/// Ids that ordinary agents must never be able to spawn.
const RESERVED_IDS: &[&str] = &["hya-compaction", "hya-summary", "hya-title"];

/// Ids the selector must present as primary agents.
const MAIN_IDS: &[&str] = &["hya-main", "hya-plan"];

fn preset() -> hya_core::CoreAgentsPreset {
    core_agents_preset().expect("embedded core-agents preset")
}

#[test]
fn roster_holds_every_builtin_id_in_sorted_order() {
    assert_eq!(builtin_agent_ids(), EXPECTED_IDS);
}

#[test]
fn roster_is_strictly_sorted_so_lookup_and_digest_stay_deterministic() {
    assert!(
        builtin_agent_ids().windows(2).all(|pair| pair[0] < pair[1]),
        "the preset roster must be strictly sorted by id"
    );
}

#[test]
fn every_builtin_resolves_by_id() {
    for id in EXPECTED_IDS {
        assert!(preset_agent(id).is_some(), "builtin `{id}` must resolve");
    }
    assert!(preset_agent("no-such-agent").is_none());
}

#[test]
fn reserved_system_agents_come_from_the_preset_policy() {
    let preset = preset();
    for agent in preset.agents() {
        let reserved = RESERVED_IDS.contains(&agent.id.as_str());
        assert_eq!(
            preset.is_reserved(agent.id.as_str()),
            reserved,
            "`{}` reserved flag is wrong",
            agent.id
        );
    }
}

#[test]
fn roles_match_the_retired_builtin_bundles() {
    for agent in preset().agents() {
        let expected = if MAIN_IDS.contains(&agent.id.as_str()) {
            AgentRole::Main
        } else {
            AgentRole::Subagent
        };
        assert_eq!(agent.role, expected, "`{}` role is wrong", agent.id);
    }
}

#[test]
fn every_builtin_carries_a_non_empty_prompt() {
    for agent in preset().agents() {
        let prompt = agent
            .prompt
            .as_deref()
            .unwrap_or_else(|| panic!("`{}` needs a prompt body", agent.id));
        assert!(
            !prompt.trim().is_empty(),
            "`{}` prompt body must not be empty",
            agent.id
        );
    }
}

#[test]
fn every_builtin_id_carries_the_hya_prefix() {
    for agent in preset().agents() {
        assert!(
            agent.id.as_str().starts_with("hya-"),
            "`{}` lacks the hya- prefix",
            agent.id
        );
    }
}

#[test]
fn ordinary_agents_are_the_five_non_reserved_ids() {
    let preset = preset();
    let ordinary = preset
        .agents()
        .iter()
        .filter(|agent| !preset.is_reserved(agent.id.as_str()))
        .map(|agent| agent.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 5);
    for reserved in RESERVED_IDS {
        assert!(!ordinary.contains(reserved));
    }
}

#[test]
fn descriptions_exist_for_every_selector_visible_agent() {
    // Reserved system agents are never shown in a selector, so they need no
    // description. Everything else is pickable and must describe itself.
    let preset = preset();
    for agent in preset.agents() {
        if preset.is_reserved(agent.id.as_str()) {
            continue;
        }
        assert!(
            agent
                .description
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
            "`{}` needs a description",
            agent.id
        );
    }
}
