//! Immutable per-Agent model preference bindings and precedence.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use hya_bundle::{AgentRole, ModelPolicy, SpawnLifecycle};
use hya_core::{
    AgentDefinition, AgentOrigin, AgentSpec, CategoryEntry, CategoryRegistry, RuntimeRegistry,
    apply_agent_model_preference, resolve_configured_agent_model,
};
use hya_proto::ModelRef;
use hya_tool::ToolRegistry;

use support::{TestDir, builtin_only_catalog};

fn agent(policy: ModelPolicy) -> AgentDefinition<'static> {
    AgentDefinition {
        stable_id: "general",
        description: None,
        role: AgentRole::Subagent,
        color: None,
        prompt: None,
        model_policy: Cow::Owned(policy),
        workdir: None,
        spawn_lifecycle: SpawnLifecycle::Transient,
        origin: AgentOrigin::Builtin,
    }
}

fn base() -> AgentSpec {
    AgentSpec {
        name: "general".into(),
        model: "openai/base".into(),
        system_prompt: "base".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    }
}

#[test]
fn turn_bindings_pin_the_published_preference_snapshot() {
    let workdir = TestDir::new("agent-model-preferences");
    let registry = RuntimeRegistry::new(ToolRegistry::builtins(), builtin_only_catalog());
    let before = registry.bind_turn(workdir.path()).unwrap();

    let mut first = BTreeMap::new();
    first.insert("general".to_string(), ModelRef::new("openai/first"));
    registry.publish_agent_model_preferences(first);
    let bound_first = registry.bind_turn(workdir.path()).unwrap();

    let mut second = BTreeMap::new();
    second.insert("general".to_string(), ModelRef::new("openai/second"));
    registry.publish_agent_model_preferences(second);
    let bound_second = registry.bind_turn(workdir.path()).unwrap();

    assert_eq!(before.agent_model_preference("general"), None);
    assert_eq!(
        bound_first.agent_model_preference("general"),
        Some(&ModelRef::new("openai/first"))
    );
    assert_eq!(
        bound_second.agent_model_preference("general"),
        Some(&ModelRef::new("openai/second"))
    );
}

#[test]
fn remembered_model_is_only_an_unconfigured_servable_default() {
    let remembered = ModelRef::new("openai/remembered");
    let servable = |model: &ModelRef| model == &remembered;

    let selected = apply_agent_model_preference(
        base(),
        &agent(ModelPolicy::default()),
        Some(&remembered),
        &servable,
    );
    assert_eq!(selected.model, remembered);

    let stale = apply_agent_model_preference(
        base(),
        &agent(ModelPolicy::default()),
        Some(&ModelRef::new("missing/model")),
        &servable,
    );
    assert_eq!(stale.model, ModelRef::new("openai/base"));

    for policy in [
        ModelPolicy {
            model: Some("openai/configured".to_string()),
            category: None,
            reasoning: None,
        },
        ModelPolicy {
            model: None,
            category: Some("fast".to_string()),
            reasoning: None,
        },
    ] {
        let configured =
            apply_agent_model_preference(base(), &agent(policy), Some(&remembered), &servable);
        assert_eq!(configured.model, ModelRef::new("openai/base"));
    }

    let reasoning_only = apply_agent_model_preference(
        base(),
        &agent(ModelPolicy {
            model: None,
            category: None,
            reasoning: Some("high".to_string()),
        }),
        Some(&remembered),
        &servable,
    );
    assert_eq!(reasoning_only.model, remembered);
}

#[test]
fn configured_model_resolution_prefers_direct_then_first_servable_category_candidate() {
    let categories = CategoryRegistry::from_entries(HashMap::from([(
        "deep".to_string(),
        CategoryEntry {
            model: ModelRef::new("missing/primary"),
            fallback: vec![ModelRef::new("openai/fallback")],
            prompt_append: String::new(),
            token_budget: None,
        },
    )]));
    let is_servable = |model: &ModelRef| model.as_str() == "openai/fallback";

    let category = resolve_configured_agent_model(
        &ModelPolicy {
            category: Some("deep".to_string()),
            ..ModelPolicy::default()
        },
        &categories,
        &is_servable,
    );
    assert_eq!(category, Some(ModelRef::new("openai/fallback")));

    let direct = resolve_configured_agent_model(
        &ModelPolicy {
            model: Some("openai/direct".to_string()),
            category: Some("deep".to_string()),
            reasoning: None,
        },
        &categories,
        &is_servable,
    );
    assert_eq!(direct, Some(ModelRef::new("openai/direct")));
}
