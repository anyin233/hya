//! Built-in agents are Rust-native definitions, not AgentBundles.
//!
//! These tests pin the roster that used to live in `bundles/builtin/*/bundle.yaml`
//! so the move out of the bundle system cannot silently drop or rename an agent.

use hya_bundle::{AgentRole, SpawnLifecycle};
use hya_core::builtin_agents::{BUILTIN_AGENTS, BuiltinAgent, SpawnScope, builtin_agent};

/// Every built-in id, in the order the roster must expose them.
const EXPECTED_IDS: &[&str] = &[
    "build",
    "compaction",
    "explore",
    "general",
    "hya-docs",
    "hya-explorer",
    "hya-implementer",
    "hya-main",
    "hya-planner",
    "hya-release",
    "hya-reviewer",
    "hya-tester",
    "plan",
    "summary",
    "title",
];

/// Ids that ordinary agents must never be able to spawn.
const RESERVED_IDS: &[&str] = &["compaction", "summary", "title"];

/// Ids that carry a compiled-in prompt body.
const PROMPTED_IDS: &[&str] = &[
    "compaction",
    "explore",
    "hya-docs",
    "hya-explorer",
    "hya-implementer",
    "hya-main",
    "hya-planner",
    "hya-release",
    "hya-reviewer",
    "hya-tester",
    "summary",
    "title",
];

/// Ids the selector must present as primary agents.
const MAIN_IDS: &[&str] = &["build", "hya-main", "plan"];

#[test]
fn roster_holds_every_builtin_id_in_sorted_order() {
    let ids = BUILTIN_AGENTS
        .iter()
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, EXPECTED_IDS);
}

#[test]
fn roster_is_strictly_sorted_so_lookup_and_digest_stay_deterministic() {
    assert!(
        BUILTIN_AGENTS.windows(2).all(|pair| pair[0].id < pair[1].id),
        "BUILTIN_AGENTS must be strictly sorted by id"
    );
}

#[test]
fn every_builtin_resolves_by_id() {
    for id in EXPECTED_IDS {
        assert!(builtin_agent(id).is_some(), "builtin `{id}` must resolve");
    }
    assert!(builtin_agent("no-such-agent").is_none());
}

#[test]
fn reserved_system_agents_are_marked_and_spawn_nothing() {
    for agent in BUILTIN_AGENTS {
        let reserved = RESERVED_IDS.contains(&agent.id);
        assert_eq!(
            agent.system_reserved, reserved,
            "`{}` system_reserved flag is wrong",
            agent.id
        );
        let expected_scope = if reserved {
            SpawnScope::None
        } else {
            SpawnScope::AllOrdinary
        };
        assert_eq!(
            agent.spawn_scope, expected_scope,
            "`{}` spawn scope is wrong",
            agent.id
        );
    }
}

#[test]
fn roles_match_the_retired_builtin_bundles() {
    for agent in BUILTIN_AGENTS {
        let expected = if MAIN_IDS.contains(&agent.id) {
            AgentRole::Main
        } else {
            AgentRole::Subagent
        };
        assert_eq!(agent.role, expected, "`{}` role is wrong", agent.id);
    }
}

#[test]
fn prompted_agents_carry_non_empty_compiled_in_bodies() {
    for agent in BUILTIN_AGENTS {
        let has_prompt = PROMPTED_IDS.contains(&agent.id);
        assert_eq!(
            agent.prompt.is_some(),
            has_prompt,
            "`{}` prompt presence is wrong",
            agent.id
        );
        if let Some(prompt) = agent.prompt {
            assert!(
                !prompt.trim().is_empty(),
                "`{}` prompt body must not be empty",
                agent.id
            );
        }
    }
}

#[test]
fn every_builtin_is_transient_and_selector_visible() {
    for agent in BUILTIN_AGENTS {
        assert_eq!(
            agent.spawn_lifecycle,
            SpawnLifecycle::Transient,
            "`{}` lifecycle is wrong",
            agent.id
        );
    }
}

#[test]
fn ordinary_agents_are_the_twelve_non_reserved_ids() {
    let ordinary = BUILTIN_AGENTS
        .iter()
        .filter(|agent| !agent.system_reserved)
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 12);
    for reserved in RESERVED_IDS {
        assert!(!ordinary.contains(reserved));
    }
}

#[test]
fn descriptions_exist_for_every_selector_visible_agent() {
    // Reserved system agents are never shown in a selector, so they need no
    // description. Everything else is pickable and must describe itself.
    for agent in BUILTIN_AGENTS {
        if agent.system_reserved {
            continue;
        }
        assert!(
            agent.description.is_some_and(|text| !text.trim().is_empty()),
            "`{}` needs a description",
            agent.id
        );
    }
}

#[test]
fn definition_view_reports_builtin_origin() {
    let agent: &BuiltinAgent = builtin_agent("build").expect("build");
    let definition = agent.definition();
    assert_eq!(definition.stable_id, "build");
    assert_eq!(definition.role, AgentRole::Main);
    assert!(definition.origin.is_builtin());
}
