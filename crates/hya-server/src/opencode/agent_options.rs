use std::collections::BTreeMap;

use serde_json::Value;

pub(super) type AgentOptions = BTreeMap<String, Value>;

pub(super) fn from_config(
    options: Option<AgentOptions>,
    mut extra: AgentOptions,
) -> Option<AgentOptions> {
    extra.remove("name");
    extra.remove("tools");
    let mut merged = options.unwrap_or_default();
    merged.extend(extra);
    (!merged.is_empty()).then_some(merged)
}
