#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::config::load;
use crate::test_env::{EnvGuard, StableEnvGuard};
use hya_tool::Invocation;

#[tokio::test]
async fn hot_reload_disabled_mcp_does_not_resolve_missing_secret() {
    let mut env = EnvGuard::new();
    let root =
        std::env::temp_dir().join(format!("hya-disabled-mcp-{}", hya_proto::SessionId::new()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(root.join("hya")).unwrap();
    let path = root.join("hya/config.yaml");
    std::fs::write(&path, "mcp:\n  disabled:\n    command: [missing]\n    enabled: false\n    env:\n      TOKEN: '{env:HYA_DEFINITELY_UNSET_HOT_RELOAD_TEST}'\n").unwrap();
    env.set("XDG_CONFIG_HOME", root.to_str().unwrap());
    let loaded = load().await.unwrap().unwrap();
    assert_eq!(loaded.mcp["disabled"].enabled, Some(false));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hot_reload_mcp_retains_remote_and_disabled_declarations() {
    let _env = StableEnvGuard::new();
    let file: FileConfig = parse_config("mcp:\n  remote:\n    url: https://example.com/mcp\n    transport: sse\n    enabled: false\n").unwrap();
    assert_eq!(resolve_mcp(&file).unwrap(), file.mcp);
}

#[test]
fn permission_yaml_compiles_documented_rules_defaults_and_validation() {
    let _env = StableEnvGuard::new();
    let file = parse_config(
        r#"
permission:
  model: default
  rules:
    - target: tool
      selector: "^(read|grep)$"
      permission: allow
    - target: mcp
      selector: "^mcp__github__"
      permission: ask
    - target: command
      selector: "^git (status|diff)"
      permission: deny
"#,
    )
    .unwrap();
    let policy = resolve_permission(&file).unwrap();
    assert_eq!(
        policy.evaluate(&Invocation::tool("read", Mode::Ask)).mode,
        Mode::Allow
    );
    assert_eq!(
        policy.evaluate(&Invocation::mcp("mcp__github__issue")).mode,
        Mode::Ask
    );
    assert_eq!(
        policy
            .evaluate(&Invocation::command("bash", "git status"))
            .mode,
        Mode::Deny
    );

    let omitted = parse_config("{}").unwrap();
    assert_eq!(
        resolve_permission(&omitted)
            .unwrap()
            .evaluate(&Invocation::tool("read", Mode::Allow))
            .mode,
        Mode::Allow
    );
    let invalid_regex = parse_config(
        "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: allow\n",
    )
    .unwrap();
    assert!(resolve_permission(&invalid_regex).is_err());
    assert!(parse_config("permission:\n  model: unknown\n").is_err());
    assert!(
            parse_config(
                "permission:\n  rules:\n    - target: unknown\n      selector: x\n      permission: allow\n",
            )
            .is_err()
        );
    // Lowercase rule effects match `permission.model` casing.
    let lower = parse_config(
        "permission:\n  rules:\n    - target: tool\n      selector: x\n      permission: allow\n",
    )
    .unwrap();
    assert!(resolve_permission(&lower).is_ok());
}

#[test]
fn catalog_scope_cache_keys_parse_and_default() {
    let _env = StableEnvGuard::new();
    let file =
        parse_config("default_model: x\ncatalog_scopes:\n  max: 4\n  idle_ttl_secs: 90\n").unwrap();
    let config = resolve_catalog_scope_cache(file.catalog_scopes.as_ref());
    assert_eq!(config.max_scopes, 4);
    assert_eq!(config.idle_ttl, std::time::Duration::from_secs(90));

    let partial = parse_config("default_model: x\ncatalog_scopes:\n  max: 7\n").unwrap();
    let config = resolve_catalog_scope_cache(partial.catalog_scopes.as_ref());
    assert_eq!(config.max_scopes, 7);
    assert_eq!(config.idle_ttl, std::time::Duration::from_secs(30 * 60));

    let absent = parse_config("default_model: x\n").unwrap();
    assert_eq!(
        resolve_catalog_scope_cache(absent.catalog_scopes.as_ref()),
        hya_engine::CatalogScopeCacheConfig::default()
    );
    assert_eq!(
        hya_engine::CatalogScopeCacheConfig::default().max_scopes,
        32,
        "documented default"
    );
}

#[test]
fn subagent_limits_parse_from_file_and_env_wins() {
    let mut env = EnvGuard::new();
    // File block sets every field; a partial block keeps defaults elsewhere.
    // A legacy `max_depth` key parses but is ignored: depth is the hardcoded
    // engine constant (ADR-0015), not a config knob.
    let file = parse_config(
            "default_model: x\nsubagents:\n  max_depth: 9\n  max_concurrency: 200\n  per_run_budget: 1000\n  per_team_turn_budget: 700\n  per_team_message_budget: 800\n",
        )
        .unwrap();
    let from_file = resolve_subagent_limits(file.subagents.as_ref());
    assert_eq!(from_file.max_concurrency, 200);
    assert_eq!(from_file.per_run_budget, 1000);
    assert_eq!(from_file.per_team_turn_budget, 700);
    assert_eq!(from_file.per_team_message_budget, 800);

    // Absent block → all defaults (per_run_budget raised to 1024 for swarms).
    let defaults = resolve_subagent_limits(None);
    assert_eq!(defaults, SubagentLimits::default());
    assert_eq!(defaults.per_run_budget, 1024);

    // The new per-team budgets honor their env overrides too.
    env.set("HYA_SUBAGENT_MESSAGE_BUDGET", "5");
    let msg = resolve_subagent_limits(file.subagents.as_ref());
    env.remove("HYA_SUBAGENT_MESSAGE_BUDGET");
    assert_eq!(
        msg.per_team_message_budget, 5,
        "env wins for message budget"
    );
    assert_eq!(msg.per_team_turn_budget, 700, "untouched field stays file");

    // Env override wins over the file value.
    env.set("HYA_SUBAGENT_MAX_CONCURRENCY", "64");
    let overridden = resolve_subagent_limits(file.subagents.as_ref());
    env.remove("HYA_SUBAGENT_MAX_CONCURRENCY");
    assert_eq!(overridden.max_concurrency, 64, "env must win over file");
    assert_eq!(
        overridden.per_run_budget, 1000,
        "untouched field stays file"
    );
}

#[test]
fn compaction_settings_parse_from_file_and_env_wins() {
    let mut env = EnvGuard::new();
    let file = parse_config(
            "default_model: x\ncompaction:\n  keep_recent: 12\n  summary_max_tokens: 8192\n  token_accounting: estimate\n",
        )
        .unwrap();
    let from_file = resolve_context_settings(file.compaction.as_ref());
    assert_eq!(from_file.compaction.keep_recent, 12);
    assert_eq!(from_file.compaction.summary_max_tokens, 8_192);
    assert_eq!(from_file.token_accounting, TokenAccountingMode::Estimate);

    // Absent block → engine defaults, accounting on Auto.
    let defaults = resolve_context_settings(None);
    assert_eq!(defaults.compaction.summary_max_tokens, 4_096);
    assert_eq!(defaults.token_accounting, TokenAccountingMode::Auto);

    // Env wins over the file value.
    env.set("HYA_COMPACTION_SUMMARY_MAX_TOKENS", "2048");
    env.set("HYA_TOKEN_ACCOUNTING", "provider");
    let overridden = resolve_context_settings(file.compaction.as_ref());
    env.remove("HYA_COMPACTION_SUMMARY_MAX_TOKENS");
    env.remove("HYA_TOKEN_ACCOUNTING");
    assert_eq!(
        overridden.compaction.summary_max_tokens, 2_048,
        "env must win"
    );
    assert_eq!(overridden.token_accounting, TokenAccountingMode::Provider);
    assert_eq!(
        overridden.compaction.keep_recent, 12,
        "untouched field stays file"
    );

    // An unparseable accounting mode is ignored rather than guessed at.
    env.set("HYA_TOKEN_ACCOUNTING", "banana");
    let bogus = resolve_context_settings(file.compaction.as_ref());
    assert_eq!(bogus.token_accounting, TokenAccountingMode::Estimate);
}

#[test]
fn goal_evaluator_model_parses_and_precedence_is_cli_over_config_over_worker() {
    let _env = StableEnvGuard::new();
    // The `goal:` block parses; absent blocks stay absent.
    let file = parse_config("default_model: x\ngoal:\n  evaluator_model: deep/o3-mini\n").unwrap();
    assert_eq!(
        file.goal.as_ref().unwrap().evaluator_model.as_deref(),
        Some("deep/o3-mini")
    );
    let absent = parse_config("default_model: x\n").unwrap();
    assert!(absent.goal.is_none());
    assert_eq!(
        GoalSettings::from_block(absent.goal),
        GoalSettings::default(),
        "no goal block means the worker's current model judges"
    );

    // Precedence (D7): CLI flag > config `goal.evaluator_model` > worker
    // current model.
    assert_eq!(
        resolve_evaluator_model(Some("cli/model"), Some("cfg/model"), "worker/model"),
        "cli/model"
    );
    assert_eq!(
        resolve_evaluator_model(None, Some("cfg/model"), "worker/model"),
        "cfg/model"
    );
    assert_eq!(
        resolve_evaluator_model(None, None, "worker/model"),
        "worker/model"
    );
    // Blank layers count as unset and fall through instead of shadowing.
    assert_eq!(
        resolve_evaluator_model(Some("   "), Some("cfg/model"), "worker/model"),
        "cfg/model"
    );
    assert_eq!(
        resolve_evaluator_model(None, Some(""), "worker/model"),
        "worker/model"
    );
    // Config values are used verbatim after trimming.
    assert_eq!(
        resolve_evaluator_model(None, Some("  cfg/model "), "worker/model"),
        "cfg/model"
    );
}

#[test]
fn code_mode_defaults_to_normal_and_parses_the_three_modes() {
    let mode = |yaml: &str| {
        parse_config(yaml).map(|file| {
            file.tools
                .map_or_else(CodeMode::default, |tools| tools.code.mode)
        })
    };
    assert_eq!(mode("default_model: a/b\n").unwrap(), CodeMode::Normal);
    assert_eq!(
        mode("tools:\n  websearch:\n    enabled: false\n").unwrap(),
        CodeMode::Normal
    );
    for (text, expected) in [
        ("normal", CodeMode::Normal),
        ("with_tools", CodeMode::WithTools),
        ("only", CodeMode::Only),
    ] {
        let yaml = format!("tools:\n  code:\n    mode: {text}\n");
        assert_eq!(mode(&yaml).unwrap(), expected);
    }
    assert!(mode("tools:\n  code:\n    mode: always\n").is_err());
    assert!(mode("tools:\n  code:\n    enabled: true\n").is_err());
}

#[test]
fn parses_webfetch_enabled_and_rejects_unknown_fields() -> anyhow::Result<()> {
    for (yaml, enabled) in [
        ("{}", true),
        ("tools: {}", true),
        ("tools: {webfetch: {}}", true),
        ("tools: {webfetch: {enabled: true}}", true),
        ("tools: {webfetch: {enabled: false}}", false),
    ] {
        let settings = EngineSettings {
            file: parse_config(yaml)?,
        };
        assert_eq!(settings.webfetch_enabled(), enabled, "{yaml}");
    }
    assert!(parse_config("tools: {webfetch: {enable: false}}").is_err());
    Ok(())
}

#[test]
fn parses_websearch_tool_config() {
    let _env = StableEnvGuard::new();
    let file = parse_config(
            "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example.test/mcp\n    key: secret\n    enabled: false\n",
        )
        .unwrap();
    let websearch = file.tools.unwrap().websearch;

    assert_eq!(websearch.provider, hya_tool::WebSearchProvider::Parallel);
    assert_eq!(
        websearch.endpoint.as_deref(),
        Some("https://search.example.test/mcp")
    );
    assert_eq!(websearch.key.as_deref(), Some("secret"));
    assert!(!websearch.enabled);
}

#[test]
fn parses_categories_into_ordered_registry() {
    let _env = StableEnvGuard::new();
    let file = parse_config(
            "default_model: x\ncategories:\n  deep: [primary/opus, backup/sonnet]\n  quick: [gw/haiku]\n  empty: []\n",
        )
        .unwrap();
    let registry = resolve_categories(&file);

    // Ordered candidates: first is preferred, rest are failover.
    let deep = registry.resolve("deep").unwrap();
    assert_eq!(deep.model.as_str(), "primary/opus");
    let chain: Vec<&str> = deep.fallback_chain.iter().map(|m| m.as_str()).collect();
    assert_eq!(chain, vec!["primary/opus", "backup/sonnet"]);
    assert!(registry.resolve("quick").is_some());
    // An empty candidate list cannot resolve to anything → dropped.
    assert!(registry.resolve("empty").is_none());

    // Absent block → empty registry.
    let bare = parse_config("default_model: x\n").unwrap();
    assert!(resolve_categories(&bare).resolve("deep").is_none());
}

#[test]
fn parses_mcp_and_keeps_disabled_servers() {
    let _env = StableEnvGuard::new();
    let yaml = "
mcp:
  echo:
    command: [python3, echo.py]
    env:
      TOKEN: literal-token
    timeout_ms: 250
  off:
    enabled: false
    command: [nope]
";
    let file = parse_config(yaml).unwrap();
    let mcp = resolve_mcp(&file).unwrap();
    assert_eq!(mcp.len(), 2);
    assert_eq!(mcp["off"].enabled, Some(false));
    let echo = mcp.get("echo").unwrap();
    assert_eq!(
        echo.command,
        vec!["python3".to_string(), "echo.py".to_string()]
    );
    assert_eq!(
        echo.env.as_ref().unwrap().get("TOKEN").unwrap(),
        "literal-token"
    );
    assert_eq!(echo.timeout_ms, Some(250));
}

#[test]
fn mcp_server_keys_must_be_valid_namespace_tokens() {
    let _env = StableEnvGuard::new();
    let yaml = "mcp:\n  bad__key:\n    command: [echo]\n";
    let file = parse_config(yaml).unwrap();
    assert!(
        resolve_mcp(&file).is_err(),
        "nested `__` keys must be rejected"
    );

    let yaml = "mcp:\n  has.dot:\n    command: [echo]\n";
    let file = parse_config(yaml).unwrap();
    assert!(resolve_mcp(&file).is_err(), "dots must be rejected");

    let yaml = "mcp:\n  good-key_1:\n    command: [echo]\n";
    let file = parse_config(yaml).unwrap();
    let resolved = resolve_mcp(&file).unwrap();
    assert!(resolved.contains_key("good-key_1"));
}

#[test]
fn parses_plugins_section() {
    let _env = StableEnvGuard::new();
    let yaml = "
plugins:
  memory:
    command: [python3, memory.py]
    timeout_ms: 500
    env:
      TOKEN: literal
  disabled-one:
    enabled: false
    command: [nope]
  ext:
    kind: bun
  cc:
    kind: claude
    plugin_dir: /plugins/cc-demo
";
    let file = parse_config(yaml).unwrap();
    assert_eq!(file.plugins.len(), 4);
    let memory = file.plugins.get("memory").unwrap();
    assert_eq!(
        memory.command,
        vec!["python3".to_string(), "memory.py".to_string()]
    );
    assert_eq!(memory.timeout_ms, Some(500));
    assert!(memory.enabled);
    assert_eq!(memory.env.get("TOKEN").map(String::as_str), Some("literal"));
    assert!(!file.plugins.get("disabled-one").unwrap().enabled);
    assert_eq!(
        file.plugins.get("ext").unwrap().kind,
        hya_plugin::messages::PluginKindWire::Bun
    );
    let cc = file.plugins.get("cc").unwrap();
    assert_eq!(cc.kind, hya_plugin::messages::PluginKindWire::Claude);
    assert_eq!(
        cc.plugin_dir,
        Some(std::path::PathBuf::from("/plugins/cc-demo"))
    );
}

#[test]
fn ensure_config_file_at_creates_parent_dir_and_minimal_config() {
    let _env = StableEnvGuard::new();
    let dir = std::env::temp_dir().join(format!("hya-config-first-run-{}", std::process::id()));
    let path = dir.join("nested/hya/config.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(ensure_config_file_at(&path).unwrap());
    assert!(path.exists(), "config file should be created");
    assert!(
        parse_config(&std::fs::read_to_string(&path).unwrap()).is_ok(),
        "created config should be valid YAML"
    );
    assert!(
        !ensure_config_file_at(&path).unwrap(),
        "second call should leave existing config untouched"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
