use serde::Serialize;
use yaca_core::SessionEngine;
use yaca_tool::{Action, Mode, Rule};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct PermissionRule {
    permission: &'static str,
    pattern: String,
    action: &'static str,
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
    PermissionRule {
        permission: permission_name(rule.action),
        pattern: rule.resource_pattern.clone(),
        action: mode_name(rule.mode),
    }
}

fn permission_name(action: Action) -> &'static str {
    match action {
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
