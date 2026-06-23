#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn resolves_slash_commands_and_aliases() {
    assert_eq!(resolve_slash("model"), Some(CommandKind::Model));
    assert_eq!(resolve_slash("models"), Some(CommandKind::Model));
    assert_eq!(resolve_slash("mo"), Some(CommandKind::Model));
    assert_eq!(resolve_slash("resume"), Some(CommandKind::Resume));
    assert_eq!(resolve_slash("sessions"), Some(CommandKind::Resume));
    assert_eq!(resolve_slash("continue"), Some(CommandKind::Resume));
    assert_eq!(resolve_slash("new"), Some(CommandKind::NewSession));
    assert_eq!(resolve_slash("clear"), Some(CommandKind::NewSession));
    assert_eq!(resolve_slash("compact"), Some(CommandKind::Compact));
    assert_eq!(resolve_slash("init"), Some(CommandKind::Init));
    assert_eq!(resolve_slash("agent"), Some(CommandKind::Agent));
    assert_eq!(resolve_slash("connect"), Some(CommandKind::Connect));
    assert_eq!(resolve_slash("tools"), Some(CommandKind::Tools));
    assert_eq!(resolve_slash("mcp"), Some(CommandKind::Tools));
    assert_eq!(resolve_slash("mcps"), Some(CommandKind::Tools));
    assert_eq!(resolve_slash("status"), Some(CommandKind::Tools));
    assert_eq!(resolve_slash("skills"), Some(CommandKind::Skills));
    assert_eq!(resolve_slash("yolo"), Some(CommandKind::Yolo));
    assert_eq!(resolve_slash("think"), Some(CommandKind::Think));
    assert_eq!(resolve_slash("export"), Some(CommandKind::Export));
    assert_eq!(resolve_slash("quit"), Some(CommandKind::Quit));
    assert_eq!(resolve_slash("exit"), Some(CommandKind::Quit));
    assert_eq!(resolve_slash("q"), Some(CommandKind::Quit));
    assert_eq!(resolve_slash("help"), Some(CommandKind::Help));
}

#[test]
fn unknown_slash_command_is_not_resolved() {
    assert_eq!(resolve_slash("nope"), None);
}

#[test]
fn help_items_come_from_registered_commands() {
    let items = help_items();
    assert!(items.iter().any(|item| item.label == "/model"));
    assert!(items.iter().any(|item| item.label == "/connect"));
    assert!(items.iter().any(|item| item.label == "/resume"));
    assert!(items.iter().any(|item| item.label == "/new"));
    assert!(items.iter().any(|item| item.label == "/export"));
    assert!(items.iter().any(|item| item.label == "/status"));
    assert!(items.iter().any(|item| item.label == "/quit"));
    assert!(items.iter().any(|item| item.label == "/help"));
}

#[test]
fn status_commands_advertise_opencode_leader_shortcut() {
    let items = help_items();

    let tools_detail = items
        .iter()
        .find(|item| item.label == "/tools")
        .map(|item| item.detail.as_str());
    let mcp_detail = items
        .iter()
        .find(|item| item.label == "/mcp")
        .map(|item| item.detail.as_str());

    assert!(matches!(tools_detail, Some(detail) if detail.starts_with("MCP · leader s")));
    assert!(matches!(mcp_detail, Some(detail) if detail.starts_with("MCP · leader s")));
}

#[test]
fn status_command_matches_opencode_system_command() {
    let items = help_items();

    let status_detail = items
        .iter()
        .find(|item| item.label == "/status")
        .map(|item| item.detail.as_str());

    assert!(
        matches!(status_detail, Some(detail) if detail.starts_with("System · leader s · View status"))
    );
}

#[test]
fn export_command_advertises_opencode_session_export_shortcut() {
    let items = help_items();

    let export_detail = items
        .iter()
        .find(|item| item.label == "/export")
        .map(|item| item.detail.as_str());

    assert!(matches!(export_detail, Some(detail) if detail.starts_with("Session · leader x")));
}

#[test]
fn command_help_avoids_unimplemented_leader_shortcuts() {
    let items = help_items();

    let detail = |label: &str| {
        items
            .iter()
            .find(|item| item.label == label)
            .map(|item| item.detail.as_str())
    };

    assert!(matches!(detail("/agent"), Some(text) if text.starts_with("Agent · leader a")));
    assert!(matches!(detail("/init"), Some(text) if text.starts_with("Context · /init")));
    assert!(matches!(detail("/think"), Some(text) if text.starts_with("Agent · /think")));
}

#[test]
fn completion_items_filter_by_prefix() {
    let items = completion_items("/mo");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "/model");
    let connect_items = completion_items("/con");
    assert!(connect_items.iter().any(|item| item.label == "/connect"));
    assert!(
        completion_items("/")
            .iter()
            .any(|item| item.label == "/resume")
    );
    assert!(completion_items("/model with args").is_empty());
}

#[test]
fn markdown_commands_load_frontmatter_and_expand_arguments() {
    let root = temp_root();
    let commands_dir = root.join(".opencode").join("commands");
    std::fs::create_dir_all(&commands_dir).unwrap();
    std::fs::write(
        commands_dir.join("component.md"),
        r#"---
description: Create a component
agent: build
model: anthropic/claude-sonnet
---
Create $1 in $2.

All args: $ARGUMENTS
"#,
    )
    .unwrap();

    let commands = custom::load_markdown_commands_from_dirs(&[commands_dir]).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "component");
    assert_eq!(commands[0].description, "Create a component");
    assert_eq!(commands[0].agent.as_deref(), Some("build"));
    assert_eq!(
        commands[0].model.as_deref(),
        Some("anthropic/claude-sonnet")
    );
    assert_eq!(
        commands[0].expand("Button src/components"),
        "Create Button in src/components.\n\nAll args: Button src/components\n"
    );
}

#[test]
fn custom_commands_appear_in_completion_items() {
    let custom = vec![CustomCommand {
        name: "test".to_string(),
        description: "Run tests".to_string(),
        template: "Run $ARGUMENTS".to_string(),
        agent: None,
        model: None,
        source: CustomCommandSource::Markdown,
    }];

    let items = completion_items_with_custom("/t", &custom);

    let detail = items
        .iter()
        .find(|item| item.label == "/test")
        .map(|item| item.detail.as_str());
    assert!(matches!(detail, Some(detail) if detail.contains("Run tests")));
}

#[test]
fn skill_commands_expand_and_appear_in_skill_items() {
    let custom = vec![CustomCommand::skill(
        "review".to_string(),
        "Review the current diff".to_string(),
    )];

    assert_eq!(
        custom[0].expand("src/main.rs"),
        "Use the review skill.\n\nsrc/main.rs"
    );

    let items = skill_items(&custom);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "review");
    assert!(items[0].detail.contains("Review the current diff"));
}

fn temp_root() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("yaca-command-test-{nanos}-{}", std::process::id()))
}
