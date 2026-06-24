#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yaca_proto::ModelRef;
use yaca_provider::{AnthropicMessagesProtocol, CompletionRequest, Protocol, ReasoningEffort};

use super::reasoning_options::{load_opencode_config, resolve_reasoning};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-reasoning-options-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn empty_options() -> BTreeMap<String, Value> {
    BTreeMap::new()
}

fn base_config() -> Value {
    json!({
        "provider": {
            "12th-anth": {
                "models": {
                    "claude-opus-4-8": {
                        "variants": {
                            "max": {
                                "thinking": { "budgetTokens": 31999 }
                            }
                        }
                    }
                }
            }
        }
    })
}

#[test]
fn resolves_variant_name_from_matching_model_variant_bundle() {
    // Given: a matching model variant bundle in config.
    let config = base_config();

    // When: an agent selects the max variant.
    let result = resolve_reasoning(
        Some("max"),
        &empty_options(),
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &config,
    );

    // Then: reasoning resolves to Max.
    assert_eq!(result, Some(ReasoningEffort::Max));
}

#[test]
fn merge_precedence_keeps_selected_variant_name_highest_priority() {
    // Given: model options and agent options that disagree with the variant.
    let config = json!({
        "provider": {
            "12th-anth": {
                "models": {
                    "claude-opus-4-8": {
                        "options": { "reasoningEffort": "high" },
                        "variants": {
                            "max": { "thinking": { "budgetTokens": 31999 } }
                        }
                    }
                }
            }
        }
    });
    let agent_options = BTreeMap::from_iter([("reasoningEffort".to_string(), json!("low"))]);

    // When: variant max is selected.
    let result = resolve_reasoning(
        Some("max"),
        &agent_options,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &config,
    );

    // Then: variant selection wins.
    assert_eq!(result, Some(ReasoningEffort::Max));
}

#[test]
fn resolves_reasoning_effort_from_agent_options() {
    // Given: an agent options map with reasoningEffort.
    let agent_options = BTreeMap::from_iter([("reasoningEffort".to_string(), json!("xhigh"))]);

    // When: reasoning resolves without any variant.
    let result = resolve_reasoning(
        None,
        &agent_options,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    // Then: the direct option is honored.
    assert_eq!(result, Some(ReasoningEffort::XHigh));
}

#[test]
fn ignores_disabled_variant_bundle() {
    // Given: a disabled variant bundle.
    let config = json!({
        "provider": {
            "12th-anth": {
                "models": {
                    "claude-opus-4-8": {
                        "variants": {
                            "max": {
                                "disabled": true,
                                "thinking": { "budgetTokens": 31999 }
                            }
                        }
                    }
                }
            }
        }
    });

    // When: the disabled variant is selected.
    let result = resolve_reasoning(
        Some("max"),
        &empty_options(),
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &config,
    );

    // Then: it is ignored.
    assert_eq!(result, None);
}

#[test]
fn empty_inputs_keep_reasoning_unset() {
    // Given: no variant, no options, and no config.
    // When: reasoning resolves.
    let result = resolve_reasoning(
        None,
        &empty_options(),
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    // Then: reasoning stays unset.
    assert_eq!(result, None);
}

#[test]
fn explicit_none_maps_to_off() {
    // Given: an explicit none option.
    let agent_options = BTreeMap::from_iter([("reasoningEffort".to_string(), json!("none"))]);

    // When: reasoning resolves.
    let result = resolve_reasoning(
        None,
        &agent_options,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    // Then: Off is preserved.
    assert_eq!(result, Some(ReasoningEffort::Off));
}

#[test]
fn bare_model_id_scans_provider_models() {
    // Given: a bare model id and a provider-scoped config entry.
    let config = base_config();

    // When: reasoning resolves.
    let result = resolve_reasoning(
        Some("max"),
        &empty_options(),
        &ModelRef::new("claude-opus-4-8"),
        &config,
    );

    // Then: the bare model is matched through provider scan.
    assert_eq!(result, Some(ReasoningEffort::Max));
}

#[test]
fn file_backed_variant_bundle_reaches_request_body() {
    // Given: a real opencode.json file whose non-keyword variant bundle sets Anthropic thinking.
    let workdir = tempdir();
    std::fs::write(
        workdir.join("opencode.json"),
        r#"{
  "provider": {
    "p": {
      "models": {
        "m": {
          "variants": {
            "deep": {
              "thinking": { "budgetTokens": 31999 }
            }
          }
        }
      }
    }
  }
}
"#,
    )
    .unwrap();

    // When: config is loaded from disk and reasoning resolves through that bundle.
    let config = load_opencode_config(&workdir);
    let reasoning = resolve_reasoning(
        Some("deep"),
        &empty_options(),
        &ModelRef::new("p/m#deep"),
        &config,
    );

    // Then: the reasoning level and provider body both reflect the disk bundle.
    assert_eq!(reasoning, Some(ReasoningEffort::Max));
    let body = AnthropicMessagesProtocol
        .encode(&CompletionRequest {
            model: ModelRef::new("p/m#deep"),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            reasoning,
            headers: Default::default(),
        })
        .unwrap();
    assert_eq!(body["thinking"]["budget_tokens"], 31999);

    let _ = std::fs::remove_dir_all(workdir);
}
