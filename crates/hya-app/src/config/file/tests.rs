#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::config::load;
use crate::test_env::{EnvGuard, StableEnvGuard};
use hya_tool::Invocation;

#[test]
fn permission_catalog_scopes_and_goal_precedence_parse_and_validate() {
    let _env = StableEnvGuard::new();
    let file = parse_config(
        "permission:\n  model: default\n  rules:\n    - { target: tool, selector: '^(read|grep)$', permission: allow }\n    - { target: mcp, selector: '^mcp__github__', permission: ask }\n    - { target: command, selector: '^git (status|diff)', permission: deny }\ncatalog_scopes:\n  max: 4\n  idle_ttl_secs: 90\ngoal:\n  evaluator_model: deep/o3-mini\n",
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
    assert!(resolve_permission(&parse_config("{}").unwrap()).is_ok());
    assert!(
        resolve_permission(
            &parse_config(
                "permission:\n  rules: [{ target: tool, selector: '(', permission: allow }]\n"
            )
            .unwrap()
        )
        .is_err()
    );
    assert!(parse_config("permission:\n  model: unknown\n").is_err());
    assert!(
        parse_config(
            "permission:\n  rules: [{ target: unknown, selector: x, permission: allow }]\n"
        )
        .is_err()
    );

    let cache = resolve_catalog_scope_cache(file.catalog_scopes.as_ref());
    assert_eq!(
        (cache.max_scopes, cache.idle_ttl),
        (4, std::time::Duration::from_secs(90))
    );
    assert_eq!(
        resolve_catalog_scope_cache(None),
        hya_engine::CatalogScopeCacheConfig::default()
    );

    assert_eq!(
        file.goal.as_ref().unwrap().evaluator_model.as_deref(),
        Some("deep/o3-mini")
    );
    assert_eq!(GoalSettings::from_block(None), GoalSettings::default());
    assert_eq!(
        resolve_evaluator_model(Some("cli/m"), Some("cfg/m"), "w/m"),
        "cli/m"
    );
    assert_eq!(
        resolve_evaluator_model(Some("  "), Some("  cfg/m "), "w/m"),
        "cfg/m"
    );
    assert_eq!(resolve_evaluator_model(None, Some(""), "w/m"), "w/m");
}

#[test]
fn subagent_and_compaction_settings_parse_with_env_overrides() {
    let mut env = EnvGuard::new();
    let file = parse_config(
        "subagents:\n  max_depth: 9\n  max_concurrency: 200\n  per_run_budget: 1000\n  per_team_turn_budget: 700\n  per_team_message_budget: 800\ncompaction:\n  keep_recent: 12\n  summary_max_tokens: 8192\n  token_accounting: estimate\n",
    )
    .unwrap();
    let sub = resolve_subagent_limits(file.subagents.as_ref());
    assert_eq!(
        (
            sub.max_concurrency,
            sub.per_run_budget,
            sub.per_team_turn_budget,
            sub.per_team_message_budget
        ),
        (200, 1000, 700, 800)
    );
    assert_eq!(resolve_subagent_limits(None).per_run_budget, 1024);

    env.set("HYA_SUBAGENT_MESSAGE_BUDGET", "5");
    env.set("HYA_SUBAGENT_MAX_CONCURRENCY", "64");
    let over = resolve_subagent_limits(file.subagents.as_ref());
    assert_eq!(
        (
            over.max_concurrency,
            over.per_team_message_budget,
            over.per_team_turn_budget
        ),
        (64, 5, 700)
    );

    let ctx = resolve_context_settings(file.compaction.as_ref());
    assert_eq!(
        (
            ctx.compaction.keep_recent,
            ctx.compaction.summary_max_tokens,
            ctx.token_accounting
        ),
        (12, 8_192, TokenAccountingMode::Estimate)
    );
    assert_eq!(
        resolve_context_settings(None).token_accounting,
        TokenAccountingMode::Auto
    );

    env.set("HYA_COMPACTION_SUMMARY_MAX_TOKENS", "2048");
    env.set("HYA_TOKEN_ACCOUNTING", "provider");
    let ctx_over = resolve_context_settings(file.compaction.as_ref());
    assert_eq!(
        (
            ctx_over.compaction.summary_max_tokens,
            ctx_over.token_accounting,
            ctx_over.compaction.keep_recent
        ),
        (2_048, TokenAccountingMode::Provider, 12)
    );
    env.set("HYA_TOKEN_ACCOUNTING", "banana");
    assert_eq!(
        resolve_context_settings(file.compaction.as_ref()).token_accounting,
        TokenAccountingMode::Estimate
    );
}

#[test]
fn tools_categories_and_plugins_sections_parse_and_validate() {
    let _env = StableEnvGuard::new();
    for (text, expected) in [
        ("normal", CodeMode::Normal),
        ("with_tools", CodeMode::WithTools),
        ("only", CodeMode::Only),
    ] {
        let f = parse_config(&format!("tools: {{ code: {{ mode: {text} }} }}\n")).unwrap();
        assert_eq!(f.tools.unwrap().code.mode, expected);
    }
    assert!(parse_config("tools: { code: { mode: always } }\n").is_err());
    assert!(parse_config("tools: { webfetch: { enable: false } }\n").is_err());
    assert!(
        !EngineSettings {
            file: parse_config("tools: { webfetch: { enabled: false } }\n").unwrap()
        }
        .webfetch_enabled()
    );
    assert!(
        EngineSettings {
            file: parse_config("{}").unwrap()
        }
        .webfetch_enabled()
    );

    let f = parse_config(
        "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example/mcp\n    key: secret\n    enabled: false\ncategories:\n  deep: [primary/opus, backup/sonnet]\n  empty: []\nplugins:\n  memory: { command: [python3, memory.py], timeout_ms: 500, env: { TOKEN: literal } }\n  off: { enabled: false, command: [nope] }\n  ext: { kind: bun }\n  cc: { kind: claude, plugin_dir: /plugins/cc-demo }\n",
    )
    .unwrap();
    let ws = f.tools.as_ref().unwrap().websearch.clone();
    assert_eq!(
        (ws.provider, ws.endpoint.as_deref(), ws.enabled),
        (
            hya_tool::WebSearchProvider::Parallel,
            Some("https://search.example/mcp"),
            false
        )
    );

    let cats = resolve_categories(&f);
    assert_eq!(cats.resolve("deep").unwrap().model.as_str(), "primary/opus");
    assert!(cats.resolve("empty").is_none());

    assert_eq!(f.plugins.len(), 4);
    assert!(!f.plugins["off"].enabled);
    assert_eq!(
        f.plugins["ext"].kind,
        hya_plugin::messages::PluginKindWire::Bun
    );
    assert_eq!(
        f.plugins["cc"].plugin_dir,
        Some(std::path::PathBuf::from("/plugins/cc-demo"))
    );
}

#[tokio::test]
async fn mcp_validation_disabled_secret_skip_and_remote_retention() {
    let mut env = EnvGuard::new();
    let file = parse_config(
        "mcp:\n  echo: { command: [python3, echo.py], env: { TOKEN: literal }, timeout_ms: 250 }\n  remote: { url: https://example.com/mcp, transport: sse, enabled: false }\n",
    )
    .unwrap();
    let mcp = resolve_mcp(&file).unwrap();
    assert_eq!(mcp.len(), 2);
    assert_eq!(mcp["remote"].enabled, Some(false));
    assert!(
        resolve_mcp(&parse_config("mcp:\n  bad__key: { command: [echo] }\n").unwrap()).is_err()
    );
    assert!(resolve_mcp(&parse_config("mcp:\n  has.dot: { command: [echo] }\n").unwrap()).is_err());
    assert!(
        resolve_mcp(&parse_config("mcp:\n  good-key_1: { command: [echo] }\n").unwrap()).is_ok()
    );

    let root =
        std::env::temp_dir().join(format!("hya-disabled-mcp-{}", hya_proto::SessionId::new()));
    std::fs::create_dir_all(root.join("hya")).unwrap();
    std::fs::write(
        root.join("hya/config.yaml"),
        "mcp:\n  disabled:\n    command: [missing]\n    enabled: false\n    env:\n      TOKEN: '{env:HYA_DEFINITELY_UNSET_TEST}'\n",
    )
    .unwrap();
    env.set("XDG_CONFIG_HOME", root.to_str().unwrap());
    let loaded = load().await.unwrap().unwrap();
    assert_eq!(loaded.mcp["disabled"].enabled, Some(false));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ensure_config_file_at_creates_parent_dir_and_is_idempotent() {
    let _env = StableEnvGuard::new();
    let dir = std::env::temp_dir().join(format!("hya-config-first-run-{}", std::process::id()));
    let path = dir.join("nested/hya/config.yaml");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ensure_config_file_at(&path).unwrap());
    assert!(parse_config(&std::fs::read_to_string(&path).unwrap()).is_ok());
    assert!(!ensure_config_file_at(&path).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}
