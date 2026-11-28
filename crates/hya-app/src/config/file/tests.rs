#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::config::load;
use crate::test_env::{EnvGuard, StableEnvGuard};
use hya_bundle::{BuiltinBundleProfile, BuiltinBundleSelection};
use hya_tool::Invocation;

#[test]
fn permission_catalog_scopes_and_v1_rejection() {
    let _env = StableEnvGuard::new();
    let file = parse_config(
        "schema_version: 2\npermission:\n  model: default\n  rules:\n    - { target: tool, selector: '^(read|grep)$', permission: allow }\n    - { target: mcp, selector: '^mcp__github__', permission: ask }\n    - { target: command, selector: '^git (status|diff)', permission: deny }\ncatalog_scopes:\n  max: 4\n  idle_ttl_secs: 90\n",
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
    assert!(resolve_permission(&parse_config("schema_version: 2\n").unwrap()).is_ok());
    assert!(
        resolve_permission(
            &parse_config(
                "schema_version: 2\npermission:\n  rules: [{ target: tool, selector: '(', permission: allow }]\n"
            )
            .unwrap()
        )
        .is_err()
    );
    assert!(parse_config("schema_version: 2\npermission:\n  model: unknown\n").is_err());
    assert!(
        parse_config(
            "schema_version: 2\npermission:\n  rules: [{ target: unknown, selector: x, permission: allow }]\n"
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

    // Missing or v1 schema_version and removed v1 top-level keys reject with actionable hya-migration guidance.
    for bad_cfg in [
        "default_model: hya/offline\n",
        "schema_version: 1\ndefault_model: hya/offline\n",
        "schema_version: 2\ncompaction:\n  keep_recent: 12\n",
        "schema_version: 2\ngoal:\n  evaluator_model: deep/o3-mini\n",
        "schema_version: 2\nsubagents:\n  max_concurrency: 4\n",
        "schema_version: 2\nlsp: false\n",
        "schema_version: 2\nformatter: false\n",
        "schema_version: 2\ntools:\n  websearch:\n    enabled: false\n",
        "schema_version: 2\ntools:\n  webfetch:\n    enabled: false\n",
    ] {
        let err = parse_config(bad_cfg).expect_err("legacy v1 config must be rejected");
        assert!(
            err.to_string().contains("hya-migration"),
            "missing hya-migration guidance in error for `{bad_cfg}`: {err}"
        );
    }

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
fn per_bundle_configs_and_builtin_profile_resolve_cleanly_with_env_overrides() {
    let mut env = EnvGuard::new();
    let dir =
        std::env::temp_dir().join(format!("hya-cfg-v2-bundle-{}", hya_proto::SessionId::new()));
    let cfg_path = dir.join("hya/config.yaml");
    std::fs::create_dir_all(dir.join("hya")).unwrap();
    std::fs::write(
        &cfg_path,
        "schema_version: 2\nbundles:\n  builtin:\n    profile: bare\n    enable: [hya/coding-extras, hya/network, hya/compaction, hya/goal-loop, hya/extended-tools]\n    disable: [hya/channel-tools]\n",
    )
    .unwrap();

    let write_bundle_cfg = |id: &str, body: &str| {
        let loc = crate::bundle_config::bundle_config_location(
            &cfg_path,
            id,
            crate::bundle_config::BundleConfigScope::User,
        )
        .unwrap();
        std::fs::create_dir_all(loc.dir()).unwrap();
        std::fs::write(loc.file(), body).unwrap();
    };
    write_bundle_cfg("hya/goal-loop", "goal:\n  evaluator_model: deep/o3-mini\n");
    write_bundle_cfg("hya/coding-extras", "lsp: false\n");
    write_bundle_cfg(
        "hya/extended-tools",
        "subagents:\n  max_concurrency: 200\n  per_run_budget: 1000\n  per_team_turn_budget: 700\n  per_team_message_budget: 800\n",
    );
    write_bundle_cfg(
        "hya/compaction",
        "compaction:\n  keep_recent: 12\n  summary_max_tokens: 8192\n  token_accounting: estimate\n",
    );
    write_bundle_cfg(
        "hya/network",
        "webfetch:\n  enabled: false\nwebsearch:\n  provider: parallel\n  endpoint: https://search.example/mcp\n  key: secret\n  enabled: false\n",
    );

    let file = parse_config(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
    assert_eq!(file.bundles.builtin.profile, BuiltinBundleProfile::Bare);
    assert!(file.bundles.builtin.is_enabled("hya/coding-extras"));
    assert!(file.bundles.builtin.is_enabled("hya/network"));
    assert!(!file.bundles.builtin.is_enabled("hya/channel-tools"));

    let settings = EngineSettings {
        file,
        config_file: Some(cfg_path.clone()),
    };
    assert_eq!(
        settings.goal().evaluator_model.as_deref(),
        Some("deep/o3-mini")
    );
    let net_json = settings.bundle_config_json("hya/network");
    assert_eq!(net_json["webfetch"]["enabled"], false);
    assert_eq!(net_json["websearch"]["provider"], "parallel");
    assert_eq!(
        net_json["websearch"]["endpoint"],
        "https://search.example/mcp"
    );
    assert_eq!(net_json["websearch"]["enabled"], false);

    let sub = settings.subagents();
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
    let over = settings.subagents();
    assert_eq!(
        (
            over.max_concurrency,
            over.per_team_message_budget,
            over.per_team_turn_budget
        ),
        (64, 5, 700)
    );

    let ctx = settings.context();
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
    let ctx_over = settings.context();
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
        settings.context().token_accounting,
        TokenAccountingMode::Estimate
    );

    // Bare without hya/network disables the network bundle cleanly.
    let bare_settings = EngineSettings {
        file: FileConfig {
            bundles: BundlesConfig {
                builtin: BuiltinBundleSelection::bare(),
            },
            ..FileConfig::default()
        },
        config_file: Some(cfg_path.clone()),
    };
    assert!(!bare_settings.builtin_bundles().is_enabled("hya/network"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn tools_categories_and_plugins_sections_parse_and_validate() {
    let _env = StableEnvGuard::new();
    for (text, expected) in [
        ("normal", CodeMode::Normal),
        ("with_tools", CodeMode::WithTools),
        ("only", CodeMode::Only),
    ] {
        let f = parse_config(&format!(
            "schema_version: 2\ntools: {{ code: {{ mode: {text} }} }}\n"
        ))
        .unwrap();
        assert_eq!(f.tools.unwrap().code.mode, expected);
    }
    assert!(parse_config("schema_version: 2\ntools: { code: { mode: always } }\n").is_err());
    assert!(
        EngineSettings {
            file: parse_config("schema_version: 2\n").unwrap(),
            config_file: None,
        }
        .builtin_bundles()
        .is_enabled("hya/network")
    );

    let f = parse_config(
        "schema_version: 2\ncategories:\n  deep: [primary/opus, backup/sonnet]\n  empty: []\nplugins:\n  memory: { command: [python3, memory.py], timeout_ms: 500, env: { TOKEN: literal } }\n  off: { enabled: false, command: [nope] }\n  ext: { kind: bun }\n  cc: { kind: claude, plugin_dir: /plugins/cc-demo }\n",
    )
    .unwrap();

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
        "schema_version: 2\nmcp:\n  echo: { command: [python3, echo.py], env: { TOKEN: literal }, timeout_ms: 250 }\n  remote: { url: https://example.com/mcp, transport: sse, enabled: false }\n",
    )
    .unwrap();
    let mcp = resolve_mcp(&file).unwrap();
    assert_eq!(mcp.len(), 2);
    assert_eq!(mcp["remote"].enabled, Some(false));
    assert!(
        resolve_mcp(
            &parse_config("schema_version: 2\nmcp:\n  bad__key: { command: [echo] }\n").unwrap()
        )
        .is_err()
    );
    assert!(
        resolve_mcp(
            &parse_config("schema_version: 2\nmcp:\n  has.dot: { command: [echo] }\n").unwrap()
        )
        .is_err()
    );
    assert!(
        resolve_mcp(
            &parse_config("schema_version: 2\nmcp:\n  good-key_1: { command: [echo] }\n").unwrap()
        )
        .is_ok()
    );

    let root =
        std::env::temp_dir().join(format!("hya-disabled-mcp-{}", hya_proto::SessionId::new()));
    std::fs::create_dir_all(root.join("hya")).unwrap();
    std::fs::write(
        root.join("hya/config.yaml"),
        "schema_version: 2\nmcp:\n  disabled:\n    command: [missing]\n    enabled: false\n    env:\n      TOKEN: '{env:HYA_DEFINITELY_UNSET_TEST}'\n",
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
