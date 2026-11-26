//! Guards for `support::test_runtime`'s agent-id validation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use hya_bundle::AgentRole;

use crate::support;

#[test]
fn test_runtime_rejects_yaml_hazards_and_accepts_bareword_ids() {
    type AgentRow<'a> = (&'a str, AgentRole, &'a [&'a str]);
    let tools = Arc::new(hya_tool_loader::builtins());
    let cases: &[(&[AgentRow<'_>], &str)] = &[
        (
            &[("caller", AgentRole::Main, &["alpha,beta"])],
            "splits the can_spawn flow sequence",
        ),
        (
            &[("&anchored", AgentRole::Main, &[])],
            "YAML parses as an anchor",
        ),
        (
            &[(" padded", AgentRole::Main, &[])],
            "surrounding whitespace",
        ),
        (&[("", AgentRole::Main, &[])], "must not be empty"),
    ];
    for (agents, expected) in cases {
        let err = std::panic::catch_unwind(|| {
            support::test_runtime(Arc::clone(&tools), agents);
        })
        .expect_err("hazard id must panic");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(msg.contains(expected), "expected {expected:?} in {msg:?}");
    }

    // YAML 1.1 barewords round-trip verbatim through serde_norway as Strings.
    support::test_runtime(
        tools,
        &[
            ("no", AgentRole::Main, &["on", "y"]),
            ("on", AgentRole::Subagent, &[]),
            ("y", AgentRole::Subagent, &[]),
            ("null", AgentRole::Subagent, &[]),
            ("123", AgentRole::Subagent, &[]),
        ],
    );
}
