#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::AgentSpec;
use hya_proto::{AgentName, ModelRef};
use hya_provider::ReasoningEffort;
use serde_json::{Value, json};

use super::agent_catalog::AgentEntry;
use super::reference::apply_agent_entry;

fn agent() -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("12th-anth/claude-opus-4-8"),
        system_prompt: "system".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    }
}

fn entry(variant: Option<&str>, options: BTreeMap<String, Value>) -> AgentEntry {
    AgentEntry {
        name: "build".to_string(),
        description: None,
        mode: "primary".to_string(),
        hidden: false,
        native: true,
        model: Some("12th-anth/claude-opus-4-8".to_string()),
        variant: variant.map(str::to_string),
        temperature: None,
        top_p: None,
        color: None,
        steps: None,
        options,
        request_headers: BTreeMap::new(),
        request_body: BTreeMap::new(),
        permissions: Vec::new(),
        prompt: None,
    }
}

#[test]
fn apply_agent_entry_sets_reasoning_from_variant() {
    let mut agent = agent();
    let entry = entry(Some("max"), BTreeMap::new());
    let config = json!({
        "provider": {
            "12th-anth": {
                "models": {
                    "claude-opus-4-8": {
                        "variants": {
                            "max": { "thinking": { "budgetTokens": 31999 } }
                        }
                    }
                }
            }
        }
    });

    apply_agent_entry(
        &mut agent,
        &entry,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &config,
    );

    assert_eq!(agent.reasoning, Some(ReasoningEffort::Max));
}

#[test]
fn apply_agent_entry_leaves_reasoning_unset_without_signal() {
    let mut agent = agent();
    let entry = entry(None, BTreeMap::new());

    apply_agent_entry(
        &mut agent,
        &entry,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    assert_eq!(agent.reasoning, None);
}
