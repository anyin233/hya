use std::collections::BTreeMap;

use serde_json::Value;

pub(super) fn merge_json_map(target: &mut BTreeMap<String, Value>, patch: BTreeMap<String, Value>) {
    for (key, value) in patch {
        merge_json_value(target.entry(key).or_insert(Value::Null), value);
    }
}

pub(super) fn merge_json_value(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                merge_json_value(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, patch) => *target = patch,
    }
}
