use hya_core::SessionEngine;
use hya_tool::{Action, Mode, Rule};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct PermissionRule {
    permission: String,
    pattern: String,
    action: String,
}

impl PermissionRule {
    pub(super) fn new(permission: String, pattern: String, action: String) -> Self {
        Self {
            permission,
            pattern,
            action,
        }
    }
}

pub(super) fn from_engine(engine: &SessionEngine) -> Vec<PermissionRule> {
    engine
        .permission_rules()
        .rules
        .iter()
        .map(from_rule)
        .collect()
}

fn from_rule(rule: &Rule) -> PermissionRule {
    PermissionRule::new(
        permission_name(rule.action).to_string(),
        rule.resource_pattern.clone(),
        mode_name(rule.mode).to_string(),
    )
}

fn permission_name(action: Action) -> &'static str {
    match action {
        Action::Tool => "tool",
        Action::Read => "read",
        Action::Edit => "edit",
        Action::Glob => "glob",
        Action::Grep => "grep",
        Action::Bash => "bash",
        Action::Task => "task",
        Action::Mcp => "mcp",
        Action::WebFetch => "webfetch",
        Action::WebSearch => "websearch",
        Action::TodoWrite => "todowrite",
        Action::Skill => "skill",
        Action::Lsp => "lsp",
        Action::ExternalDirectory => "external_directory",
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Allow => "allow",
        Mode::Ask => "ask",
        Mode::Deny => "deny",
    }
}
