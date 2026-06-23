use std::collections::BTreeMap;

use serde::Deserialize;

use super::agent_permission::PermissionRule;

#[derive(Deserialize)]
pub(super) struct ConfigPermissionRule {
    action: String,
    resource: String,
    effect: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum LegacyPermissionRule {
    Effect(String),
    Patterns(BTreeMap<String, String>),
}

pub(super) type LegacyPermissions = BTreeMap<String, LegacyPermissionRule>;

pub(super) fn rules(
    permissions: Option<Vec<ConfigPermissionRule>>,
    legacy: Option<LegacyPermissions>,
) -> Option<Vec<PermissionRule>> {
    let mut rules = Vec::new();
    for permission in permissions.unwrap_or_default() {
        rules.push(PermissionRule::new(
            permission.action,
            permission.resource,
            permission.effect,
        ));
    }
    for (permission, rule) in legacy.unwrap_or_default() {
        append_legacy_rule(&mut rules, permission, rule);
    }
    (!rules.is_empty()).then_some(rules)
}

fn append_legacy_rule(
    rules: &mut Vec<PermissionRule>,
    permission: String,
    rule: LegacyPermissionRule,
) {
    match rule {
        LegacyPermissionRule::Effect(action) => {
            rules.push(PermissionRule::new(permission, "*".to_string(), action));
        }
        LegacyPermissionRule::Patterns(patterns) => {
            for (pattern, action) in patterns {
                rules.push(PermissionRule::new(permission.clone(), pattern, action));
            }
        }
    }
}
