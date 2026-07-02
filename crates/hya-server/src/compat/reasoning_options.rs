use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use hya_proto::ModelRef;
use hya_provider::ReasoningEffort;
use serde_json::Value;

use super::agent_sources::config_paths;
use super::json_merge::merge_json_value;
use super::model_ref::{BARE_PROVIDER, model_ref_parts};

pub(super) fn load_compat_config(workdir: &Path) -> Value {
    let mut merged = serde_json::json!({});
    for path in global_config_paths()
        .into_iter()
        .chain(config_paths(workdir))
    {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(parsed) = super::jsonc::from_str::<Value>(&content) else {
            continue;
        };
        merge_json_value(&mut merged, parsed);
    }
    merged
}

pub(super) fn resolve_reasoning(
    agent_variant: Option<&str>,
    agent_options: &BTreeMap<String, Value>,
    model: &ModelRef,
    config: &Value,
) -> Option<ReasoningEffort> {
    let parts = model_ref_parts(model);
    if parts.variant.is_none() && agent_variant.is_none() && !has_reasoning_signal(agent_options) {
        return None;
    }

    let selected_variant = parts.variant.as_deref().or(agent_variant);
    let model_config = find_model_config(config, &parts.provider_id, &parts.model_id);
    let variant_state = selected_variant
        .map(|name| variant_bundle_state(model_config, name))
        .unwrap_or(VariantBundleState::Missing);

    let mut merged = model_config
        .and_then(|value| value.get("options"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    merge_json_value(&mut merged, value_from_options(agent_options));
    if let VariantBundleState::Active(bundle) = variant_state {
        merge_json_value(&mut merged, bundle.clone());
    }

    if !matches!(variant_state, VariantBundleState::Disabled)
        && let Some(variant) = selected_variant
        && let Some(effort) = ReasoningEffort::parse(variant)
    {
        return Some(effort);
    }
    if let Some(effort) = direct_effort(&merged) {
        return Some(effort);
    }
    if let Some(effort) = thinking_budget_effort(&merged) {
        return Some(effort);
    }
    google_effort(&merged)
}

fn global_config_paths() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            vec![
                home.join(".config/opencode/opencode.json"),
                home.join(".config/opencode/opencode.jsonc"),
            ]
        })
        .unwrap_or_default()
}

fn has_reasoning_signal(options: &BTreeMap<String, Value>) -> bool {
    options.contains_key("reasoningEffort")
        || options.contains_key("reasoning_effort")
        || options.contains_key("effort")
        || options.contains_key("reasoning")
        || options.contains_key("thinking")
        || options.contains_key("thinkingConfig")
}

fn find_model_config<'a>(
    config: &'a Value,
    provider_id: &str,
    model_id: &str,
) -> Option<&'a Value> {
    let providers = config.get("provider")?.as_object()?;
    if provider_id != BARE_PROVIDER {
        return providers
            .get(provider_id)?
            .get("models")?
            .as_object()?
            .get(model_id);
    }

    let mut provider_names = providers.keys().cloned().collect::<Vec<_>>();
    provider_names.sort();
    provider_names.into_iter().find_map(|name| {
        providers
            .get(&name)?
            .get("models")?
            .as_object()?
            .get(model_id)
    })
}

enum VariantBundleState<'a> {
    Missing,
    Disabled,
    Active(&'a Value),
}

fn variant_bundle_state<'a>(
    model_config: Option<&'a Value>,
    variant: &str,
) -> VariantBundleState<'a> {
    let Some(bundle) = model_config
        .and_then(|value| value.get("variants"))
        .and_then(Value::as_object)
        .and_then(|variants| variants.get(variant))
    else {
        return VariantBundleState::Missing;
    };
    if bundle
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return VariantBundleState::Disabled;
    }
    VariantBundleState::Active(bundle)
}

fn value_from_options(options: &BTreeMap<String, Value>) -> Value {
    Value::Object(
        options
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn direct_effort(value: &Value) -> Option<ReasoningEffort> {
    [
        value.get("reasoningEffort"),
        value.get("reasoning_effort"),
        value.get("effort"),
        nested_value(value, &["reasoning", "effort"]),
        value.get("reasoning.effort"),
    ]
    .into_iter()
    .flatten()
    .find_map(|entry| entry.as_str().and_then(ReasoningEffort::parse))
}

fn thinking_budget_effort(value: &Value) -> Option<ReasoningEffort> {
    [
        nested_value(value, &["thinking", "budgetTokens"]),
        nested_value(value, &["thinking", "budget_tokens"]),
    ]
    .into_iter()
    .flatten()
    .find_map(|entry| entry.as_u64().and_then(budget_effort))
}

fn google_effort(value: &Value) -> Option<ReasoningEffort> {
    if let Some(level) = nested_value(value, &["thinkingConfig", "thinkingLevel"])
        .and_then(Value::as_str)
        .and_then(ReasoningEffort::parse)
    {
        return Some(level);
    }
    nested_value(value, &["thinkingConfig", "thinkingBudget"])
        .and_then(Value::as_u64)
        .and_then(budget_effort)
}

fn budget_effort(budget: u64) -> Option<ReasoningEffort> {
    if budget >= 30_000 {
        return Some(ReasoningEffort::Max);
    }
    if budget >= 20_000 {
        return Some(ReasoningEffort::XHigh);
    }
    if budget >= 15_000 {
        return Some(ReasoningEffort::High);
    }
    if budget >= 3_500 {
        return Some(ReasoningEffort::Medium);
    }
    if budget >= 512 {
        return Some(ReasoningEffort::Low);
    }
    None
}

fn nested_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}
