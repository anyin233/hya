#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_plugin::config::{PluginEntry, merge};
use hya_plugin::manifest::Manifest;
use hya_plugin::messages::{HookName, PluginKindWire};

fn entry(command: &str, enabled: bool) -> PluginEntry {
    PluginEntry {
        kind: PluginKindWire::Rust,
        command: vec![command.to_string()],
        enabled,
        timeout_ms: None,
        env: BTreeMap::new(),
    }
}

fn manifest(id: &str, command: &str, enabled: bool) -> Manifest {
    Manifest {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: vec![command.to_string()],
        enabled,
        timeout_ms: None,
        hooks: Vec::new(),
    }
}

#[test]
fn manifest_parses_and_drops_unknown_hooks() {
    let toml = r#"
id = "remember"
kind = "rust"
command = ["./remember"]
timeout_ms = 1000

[[hooks]]
name = "tool.execute.before"
posture = "safe"

[[hooks]]
name = "totally.unknown"
posture = "open"
"#;
    let m = Manifest::parse(toml).unwrap();
    assert_eq!(m.id, "remember");
    assert_eq!(m.command, vec!["./remember".to_string()]);
    assert_eq!(m.timeout_ms, Some(1000));
    assert!(m.enabled, "enabled defaults to true");

    let hooks = m.resolved_hooks();
    assert_eq!(hooks.len(), 1, "the unknown hook is dropped");
    assert_eq!(hooks[0].0, HookName::ToolExecuteBefore);
}

#[test]
fn manifest_rejects_missing_required_fields() {
    assert!(
        Manifest::parse("id = \"x\"\n").is_err(),
        "command is required"
    );
}

#[test]
fn config_entry_parses_from_yaml() {
    let yaml = r#"
kind: rust
command: ["./bin/remember"]
enabled: true
timeout_ms: 500
env:
  KEY: value
"#;
    let entry: PluginEntry = serde_norway::from_str(yaml).unwrap();
    assert_eq!(entry.command, vec!["./bin/remember".to_string()]);
    assert_eq!(entry.timeout_ms, Some(500));
    assert_eq!(entry.env.get("KEY"), Some(&"value".to_string()));
}

#[test]
fn merge_config_wins_disabled_dropped_manifest_only_kept() {
    let mut config = BTreeMap::new();
    config.insert("a".to_string(), entry("config-a", true));
    config.insert("b".to_string(), entry("config-b", false));

    let manifests = vec![
        manifest("a", "scan-a", true),
        manifest("c", "scan-c", true),
        manifest("d", "scan-d", false),
    ];

    let specs = merge(config, manifests);
    let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();

    assert!(ids.contains(&"a"), "config a kept");
    assert!(!ids.contains(&"b"), "disabled config b dropped");
    assert!(ids.contains(&"c"), "manifest-only c kept");
    assert!(!ids.contains(&"d"), "disabled manifest d dropped");

    let a = specs.iter().find(|s| s.id == "a").unwrap();
    assert_eq!(
        a.command,
        vec!["config-a".to_string()],
        "config wins on id collision"
    );
}
