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
pub(super) type LegacyTools = BTreeMap<String, bool>;

pub(super) fn rules(
    permissions: Option<Vec<ConfigPermissionRule>>,
    legacy: Option<LegacyPermissions>,
    tools: Option<LegacyTools>,
) -> Option<Vec<PermissionRule>> {
    let mut rules = Vec::new();
    for permission in permissions.unwrap_or_default() {
        rules.push(PermissionRule::new(
            permission.action,
            permission.resource,
            permission.effect,
        ));
    }
    let mut legacy_rules = tool_permissions(tools);
    legacy_rules.extend(legacy.unwrap_or_default());
    for (permission, rule) in legacy_rules {
        append_legacy_rule(&mut rules, permission, rule);
    }
    (!rules.is_empty()).then_some(rules)
}

fn tool_permissions(tools: Option<LegacyTools>) -> LegacyPermissions {
    let mut permissions = LegacyPermissions::new();
    for (tool, enabled) in tools.unwrap_or_default() {
        let permission = match tool.as_str() {
            "write" | "edit" | "patch" => "edit".to_string(),
            _ => tool,
        };
        let action = if enabled { "allow" } else { "deny" };
        permissions.insert(permission, LegacyPermissionRule::Effect(action.to_string()));
    }
    permissions
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
