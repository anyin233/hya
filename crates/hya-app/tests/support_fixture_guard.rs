//! Guards for `support::test_runtime`'s agent-id validation.
//!
//! The shared fixture helper builds its `bundle.yaml` by string concatenation,
//! so an id can parse into a *different* value than the caller wrote. These
//! tests pin both halves of that contract: the ids that must be rejected, and
//! the ids that must keep working. Without them the guard itself can rot the
//! same way the duplicated fixture helper did.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_bundle::AgentRole;
use hya_tool::ToolRegistry;

fn build(agents: &[(&str, AgentRole, &[&str])]) {
    support::test_runtime(Arc::new(ToolRegistry::builtins()), agents);
}

#[test]
#[should_panic(expected = "splits the can_spawn flow sequence")]
fn rejects_comma_that_splits_can_spawn() {
    // `can_spawn: [alpha,beta]` would silently grant two spawn edges.
    build(&[("caller", AgentRole::Main, &["alpha,beta"])]);
}

#[test]
#[should_panic(expected = "YAML parses as an anchor")]
fn rejects_leading_ampersand_anchor() {
    // A leading `&` is a YAML anchor; the id would silently become "".
    build(&[("&anchored", AgentRole::Main, &[])]);
}

#[test]
#[should_panic(expected = "surrounding whitespace")]
fn rejects_surrounding_whitespace() {
    build(&[(" padded", AgentRole::Main, &[])]);
}

#[test]
#[should_panic(expected = "must not be empty")]
fn rejects_empty_id() {
    build(&[("", AgentRole::Main, &[])]);
}

/// YAML 1.1 bareword booleans are **not** a hazard for this manifest, and this
/// test exists so nobody "hardens" the guard against them: `serde_norway`
/// resolves only `true|True|TRUE|false|False|FALSE` as booleans, and these
/// fields deserialize as raw `String`s regardless. Verified end-to-end through
/// `hya_bundle::prepare_package`, not against a standalone parser probe.
#[test]
fn accepts_yaml_1_1_bareword_ids() {
    build(&[
        ("no", AgentRole::Main, &["on", "y"]),
        ("on", AgentRole::Subagent, &[]),
        ("y", AgentRole::Subagent, &[]),
        ("null", AgentRole::Subagent, &[]),
        ("123", AgentRole::Subagent, &[]),
    ]);
}
