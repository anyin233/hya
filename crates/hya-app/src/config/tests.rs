#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use hya_provider::{ModelCatalogSource, ProviderCatalogSource, ProviderKind, ProviderRouter};

use super::catalog::plan_for_provider;
use super::file::parse_config;
use super::provider::{
    ModelConfig, ParsedProvider, ProviderCredential, merge_provider_models,
    resolve_provider_credential_with, resolve_providers,
};
use super::*;
use crate::test_env::{EnvGuard, StableEnvGuard};

const FIXTURE: &str = "
default_model: gpt-5.5
providers:
  gw-oai:
    kind: openai
    base_url: https://gw.example/v1
    api_key: sk-test-literal
    models: [gpt-5.5, gpt-5.4]
  gw-anth:
    kind: anthropic
    base_url: https://gw.example/v1
    api_key: sk-test-literal
    models: [claude-sonnet-4-6]
  gw-google:
    kind: google
    base_url: https://gl.googleapis.com/v1beta
    api_key: sk-test-literal
    models: [gemini-2.0-flash]
  no-models:
    kind: openai
    base_url: https://y/v1
    api_key: x
";

fn parse_providers(yaml: &str) -> anyhow::Result<Vec<ParsedProvider>> {
    resolve_providers(&parse_config(yaml)?)
}

#[test]
fn parses_providers_kinds_and_models() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(FIXTURE).unwrap();
    assert_eq!(
        parsed.len(),
        4,
        "empty providers remain eligible for discovery"
    );
    let oai = parsed.iter().find(|p| p.id == "gw-oai").unwrap();
    assert_eq!(oai.kind, ProviderKind::OpenAiCompatible);
    assert_eq!(oai.base_url, "https://gw.example/v1");
    assert_eq!(oai.api_key.as_deref(), Some("sk-test-literal"));
    assert!(oai.models.iter().any(|model| model.id == "gpt-5.5"));
    let anth = parsed.iter().find(|p| p.id == "gw-anth").unwrap();
    assert_eq!(anth.kind, ProviderKind::Anthropic);
    let goog = parsed.iter().find(|p| p.id == "gw-google").unwrap();
    assert_eq!(goog.kind, ProviderKind::Google);
}

#[test]
fn provider_retry_defaults_without_config() {
    let _env = EnvGuard::new();
    // Env-sensitive assertions take the same gate as env writers, so a
    // concurrent override test can never leak a value into this read.
    let parsed = parse_providers(FIXTURE).unwrap();
    for provider in &parsed {
        assert_eq!(provider.retry, hya_provider::RetryConfig::default());
    }
}

#[test]
fn global_provider_retry_overrides_defaults() {
    let _env = EnvGuard::new();
    let parsed = parse_providers(
            "provider_retry:\n  max_attempts: 5\n  backoff_base_ms: 250\n  backoff_max_ms: 60000\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    api_key: sk-test-literal\n    models: [gpt-5.5]\n",
        )
        .unwrap();
    let retry = &parsed.first().unwrap().retry;
    assert_eq!(retry.max_attempts, 5);
    assert_eq!(retry.backoff_base, std::time::Duration::from_millis(250));
    assert_eq!(retry.backoff_max, std::time::Duration::from_millis(60_000));
}

#[test]
fn per_provider_retry_overrides_global_fields() {
    let _env = EnvGuard::new();
    let parsed = parse_providers(
            "provider_retry:\n  max_attempts: 5\n  backoff_base_ms: 250\nproviders:\n  tuned:\n    kind: openai\n    base_url: https://a/v1\n    api_key: x\n    models: [m1]\n    retry:\n      max_attempts: 2\n  stock:\n    kind: openai\n    base_url: https://b/v1\n    api_key: x\n    models: [m2]\n",
        )
        .unwrap();
    let tuned = parsed.iter().find(|p| p.id == "tuned").unwrap();
    assert_eq!(tuned.retry.max_attempts, 2);
    assert_eq!(
        tuned.retry.backoff_base,
        std::time::Duration::from_millis(250),
        "unset fields inherit the global value"
    );
    let stock = parsed.iter().find(|p| p.id == "stock").unwrap();
    assert_eq!(stock.retry.max_attempts, 5);
}

#[test]
fn provider_retry_env_overrides_file_values() {
    let mut env = EnvGuard::new();
    env.set("HYA_PROVIDER_RETRY_MAX_ATTEMPTS", "7");
    let parsed = parse_providers(
            "provider_retry:\n  max_attempts: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    api_key: x\n    models: [m1]\n    retry:\n      max_attempts: 3\n",
        )
        .unwrap();
    assert_eq!(parsed.first().unwrap().retry.max_attempts, 7);
}

#[test]
fn empty_config_yields_no_providers() {
    let _env = StableEnvGuard::new();
    assert!(parse_providers("{}").unwrap().is_empty());
}

#[test]
fn parsed_models_keep_provider_reasoning_variants() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(FIXTURE).unwrap();

    let openai = parsed.iter().find(|entry| entry.id == "gw-oai").unwrap();
    assert_eq!(
        openai.models[0].reasoning_variants,
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
    let anthropic = parsed.iter().find(|entry| entry.id == "gw-anth").unwrap();
    assert_eq!(
        anthropic.models[0].reasoning_variants,
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    let google = parsed.iter().find(|entry| entry.id == "gw-google").unwrap();
    assert_eq!(
        google.models[0].reasoning_variants,
        vec!["minimal", "low", "medium", "high", "max"]
    );
}

#[test]
fn response_model_config_resolves_default_and_all_variants() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(
        r#"
providers:
  gateway:
    kind: openai-response
    base_url: https://gateway.example/v1
    models:
      - id: gpt-5.6-sol
        reasoning:
          default: medium
          variants: [none, minimal, low, medium, high, xhigh, max]
"#,
    )
    .unwrap();

    assert_eq!(parsed[0].kind, ProviderKind::OpenAiResponse);
    assert_eq!(
        parsed[0].models[0].reasoning_variants,
        vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        parsed[0].models[0].reasoning_default,
        Some(hya_provider::ReasoningEffort::Medium)
    );
}

#[test]
fn reasoning_variants_keep_configured_order() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(
        r#"
providers:
  gateway:
    kind: openai-response
    base_url: https://gateway.example/v1
    models:
      - id: gpt-5.6-sol
        reasoning:
          variants: [max, high, low, medium]
"#,
    )
    .unwrap();

    assert_eq!(
        parsed[0].models[0].reasoning_variants,
        vec!["max", "high", "low", "medium"]
    );
}

#[test]
fn grok_build_config_has_capabilities_without_default_effort() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(
        r#"
providers:
  grok:
    kind: grok-build
    base_url: https://grok.example/v1
    models: [grok-4.5]
"#,
    )
    .unwrap();

    assert_eq!(
        parsed[0].models[0].reasoning_variants,
        vec!["low", "medium", "high"]
    );
    assert_eq!(parsed[0].models[0].reasoning_default, None);
}

#[test]
fn grok_build_uses_login_token_with_session_headers() {
    let _env = StableEnvGuard::new();
    let cred = resolve_provider_credential_with(
        ProviderKind::GrokBuild,
        Some("oauth-jwt"),
        Some("inline-sk"),
    )
    .unwrap();
    assert_eq!(cred.token.as_deref(), Some("oauth-jwt"));
    assert!(cred.use_grok_session);
    assert!(!cred.use_codex_session);
}

#[test]
fn grok_build_falls_back_to_inline_api_key_with_session_headers() {
    let _env = StableEnvGuard::new();
    let cred =
        resolve_provider_credential_with(ProviderKind::GrokBuild, None, Some("inline-oauth"))
            .unwrap();
    assert_eq!(cred.token.as_deref(), Some("inline-oauth"));
    assert!(cred.use_grok_session);
}

#[test]
fn non_grok_provider_uses_bearer_without_session_headers() {
    let _env = StableEnvGuard::new();
    let cred = resolve_provider_credential_with(
        ProviderKind::OpenAiResponse,
        Some("login-sk"),
        Some("inline-sk"),
    )
    .unwrap();
    assert_eq!(cred.token.as_deref(), Some("login-sk"));
    assert!(!cred.use_grok_session);
    assert!(!cred.use_codex_session);
}

#[test]
fn openai_codex_kind_parses_and_enables_codex_session() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(
        r#"
providers:
  codex:
    kind: openai-codex
    base_url: https://chatgpt.com/backend-api/codex
    api_key: unused
    models: [gpt-5.3-codex]
"#,
    )
    .unwrap();
    assert_eq!(parsed[0].kind, ProviderKind::OpenAiCodex);
    let cred =
        resolve_provider_credential_with(ProviderKind::OpenAiCodex, Some("jwt"), None).unwrap();
    assert!(cred.use_codex_session);
    assert!(!cred.use_grok_session);
}

#[test]
fn upsert_oauth_provider_keeps_empty_or_explicit_models_untouched() {
    let _env = StableEnvGuard::new();
    let dir = {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hya-cfg-upsert-{}-{}-{}",
            nanos,
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let path = dir.join("config.yaml");
    upsert_oauth_provider(
        &path,
        "codex",
        "openai-codex",
        "https://chatgpt.com/backend-api/codex",
    )
    .unwrap();
    let parsed = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let provider = parsed.providers.get("codex").unwrap();
    assert_eq!(ProviderKind::from(provider.kind), ProviderKind::OpenAiCodex);
    assert_eq!(provider.base_url, "https://chatgpt.com/backend-api/codex");
    assert!(provider.models.is_empty());
    assert_eq!(parsed.default_model.as_deref(), Some("hya/offline"));
    std::fs::write(
            &path,
            "default_model: codex/user-model\nproviders:\n  codex:\n    kind: openai-codex\n    base_url: https://old.example/codex\n    models: [user-model]\n",
        )
        .unwrap();
    upsert_oauth_provider(
        &path,
        "codex",
        "openai-codex",
        "https://chatgpt.com/backend-api/codex",
    )
    .unwrap();
    let parsed = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let provider = parsed.providers.get("codex").unwrap();
    assert!(matches!(
        provider.models.as_slice(),
        [ModelConfig::Id(id)] if id == "user-model"
    ));
    assert_eq!(parsed.default_model.as_deref(), Some("codex/user-model"));
}

#[test]
fn rejects_unknown_provider_kind_and_reasoning_efforts() {
    let _env = StableEnvGuard::new();
    let unknown_kind = parse_config(
            "providers:\n  gateway:\n    kind: openai-responses\n    base_url: https://example.test/v1\n    api_key: test\n    models: [gpt-5.6-sol]\n",
        )
        .unwrap_err();
    assert!(format!("{unknown_kind:#}").contains("unknown variant"));

    let unknown_variant = parse_providers(
            "providers:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          variants: [medium, extreme]\n",
        )
        .err()
        .unwrap();
    assert!(
        unknown_variant
            .to_string()
            .contains("unknown reasoning variant extreme")
    );

    let unsupported_default = parse_providers(
            "providers:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: high\n          variants: [low, medium]\n",
        )
        .err()
        .unwrap();
    assert!(
        unsupported_default
            .to_string()
            .contains("reasoning default high is not advertised")
    );
}

#[test]
fn legacy_string_models_keep_chat_aliases_without_default_effort() {
    let _env = StableEnvGuard::new();
    for kind in ["openai", "openai-compatible", "openai-completion"] {
        let yaml = format!(
            "providers:\n  gw:\n    kind: {kind}\n    base_url: https://gw.example/v1\n    models: [m1]\n"
        );
        let parsed = parse_providers(&yaml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, ProviderKind::OpenAiCompatible);
        assert_eq!(parsed[0].models[0].reasoning_default, None);
    }
}

#[test]
fn resolves_env_template_key() {
    let mut env = EnvGuard::new();
    // SAFETY: single-threaded test; sets then reads a unique env var.
    env.set("HYA_TEST_KEY_XYZ", "resolved-secret");
    assert_eq!(
        resolve_secret("{env:HYA_TEST_KEY_XYZ}").unwrap(),
        "resolved-secret"
    );
    assert_eq!(resolve_secret("literal-key").unwrap(), "literal-key");
}

#[test]
fn parses_provider_without_apikey() {
    let _env = StableEnvGuard::new();
    let yaml = "
providers:
  12th:
    kind: openai
    base_url: https://api.example/v1
    models: [claude-sonnet-4-6]
";
    let parsed = parse_providers(yaml).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].api_key, None);
    assert_eq!(parsed[0].base_url, "https://api.example/v1");
}

const GLM_LIMIT_YAML: &str = "providers:\n  12th:\n    kind: anthropic\n    base_url: https://api.12th.day/v1\n    models:\n      - glm-5.3\n      - id: glm-5.3-flash\n        limit:\n          context: 1048576\n          output: 131072\n      - id: glm-5.3-ctx\n        limit:\n          context: 262144\n";

#[test]
fn object_models_parse_context_and_output_limits() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(GLM_LIMIT_YAML).unwrap();
    let models = &parsed[0].models;
    assert_eq!(models[0].id, "glm-5.3");
    assert_eq!(models[0].limit, None, "plain string entries carry no limit");
    assert_eq!(
        models[1].limit,
        Some(hya_provider::ModelLimitOverride {
            context: 1_048_576,
            output: 131_072,
        })
    );
    assert_eq!(
        models[2].limit,
        Some(hya_provider::ModelLimitOverride {
            context: 262_144,
            output: 0,
        }),
        "an omitted field stays unspecified"
    );
}

#[test]
fn rejects_invalid_model_limits() {
    let _env = StableEnvGuard::new();
    let limit_yaml = |limit: &str| {
        format!(
            "providers:\n  12th:\n    kind: anthropic\n    base_url: https://api.12th.day/v1\n    models:\n      - id: glm-5.3-flash\n        limit: {limit}\n"
        )
    };
    for (limit, expected) in [
        ("{ output: 0 }", "limit.output must be a positive integer"),
        ("{ context: 0 }", "limit.context must be a positive integer"),
        ("{ output: -5 }", "limit.output must be a positive integer"),
        ("{ output: 1.5 }", "limit.output must be a positive integer"),
        (
            "{ output: lots }",
            "limit.output must be a positive integer",
        ),
        (
            "{ output: 4294967296 }",
            "limit.output must be a positive integer",
        ),
        (
            "{ context: 1000, output: 2000 }",
            "limit.output 2000 exceeds limit.context 1000",
        ),
        ("{ outputs: 2000 }", "unknown limit key outputs"),
        ("[1, 2]", "limit must be a mapping"),
    ] {
        let error = parse_providers(&limit_yaml(limit)).err().unwrap();
        let message = format!("{error:#}");
        assert!(
            (message.contains("provider 12th model glm-5.3-flash") && message.contains(expected))
                || message.contains("parse hya config.yaml"),
            "{limit}: got {message}"
        );
    }
}

#[tokio::test]
async fn configured_model_modalities_reach_route_image_input() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - id: vision\n        modalities:\n          input: [text, image]\n      - id: text-only\n        modalities:\n          input: [text]\n      - plain\n";
    let provider = parse_providers(yaml).unwrap().into_iter().next().unwrap();
    let credential = ProviderCredential {
        token: Some("test".to_string()),
        use_grok_session: false,
        use_codex_session: false,
        account_id: None,
        use_oauth_refresh: false,
    };
    let plan = plan_for_provider(&provider, &credential, &[], None).unwrap();
    let image_input = |model: &str| {
        plan.models
            .iter()
            .find(|row| row.model_id == model)
            .map(|row| row.capabilities.image_input)
            .unwrap()
    };
    assert_eq!(image_input("vision"), Some(true));
    assert_eq!(image_input("text-only"), Some(false));
    assert_eq!(
        image_input("plain"),
        None,
        "undeclared support stays unknown"
    );

    let bad = "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - id: m\n        modalities:\n          input: image\n";
    let error = format!("{:#}", parse_providers(bad).unwrap_err());
    assert!(error.contains("modalities"), "{error}");
}

#[test]
fn model_entry_kind_overrides_the_effective_protocol_menu() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  12th:\n    kind: openai\n    base_url: https://api.12th.day/v1\n    models:\n      - gpt-6-astra\n      - id: claude-opus-5\n        kind: anthropic\n      - id: glm-exp\n        kind: anthropic\n        reasoning:\n          variants: [low, high]\n";
    let provider = parse_providers(yaml).unwrap().into_iter().next().unwrap();
    let models = &provider.models;
    assert_eq!(models.len(), 3);
    // Inherits the provider kind's capability menu without selecting an effort.
    assert_eq!(
        models[0].reasoning_variants,
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
    assert_eq!(models[0].reasoning_default, None);
    // Entry-level kind override changes only the capability menu.
    assert_eq!(
        models[1].reasoning_variants,
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(models[1].reasoning_default, None);
    // Explicit reasoning.variants still replaces the overridden kind's menu.
    assert_eq!(models[2].reasoning_variants, vec!["low", "high"]);
    assert_eq!(models[2].reasoning_default, None);
}

#[test]
fn model_entry_kind_rejects_unknown_labels() {
    let _env = StableEnvGuard::new();
    let bad = "providers:\n  12th:\n    kind: openai\n    base_url: https://api.12th.day/v1\n    models:\n      - id: m\n        kind: anthropic-msgs\n";
    let error = parse_providers(bad).err().unwrap();
    let message = format!("{error:#}");
    assert!(
        message.contains("provider 12th model m has unknown kind anthropic-msgs"),
        "got {message}"
    );
    assert!(
        message.contains(&format!(
            "(expected one of {})",
            PROVIDER_KIND_LABELS.join(", ")
        )),
        "got {message}"
    );
    for label in PROVIDER_KIND_LABELS {
        let yaml = format!(
            "providers:\n  12th:\n    kind: openai\n    base_url: https://api.12th.day/v1\n    models:\n      - id: m\n        kind: {label}\n"
        );
        parse_providers(&yaml).unwrap_or_else(|error| panic!("{label}: {error:#}"));
    }
}

#[test]
fn model_kind_override_partitions_one_route_per_protocol() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  12th:\n    kind: openai-response\n    base_url: https://api.12th.day/v1\n    models:\n      - gpt-6-astra\n      - id: claude-opus-5\n        kind: anthropic\n      - id: glm-exp\n        kind: anthropic\n";
    let provider = parse_providers(yaml).unwrap().into_iter().next().unwrap();
    let plan = plan_for_provider(&provider, &no_key(), &[], None).unwrap();
    assert_eq!(plan.routes.len(), 2, "one route per effective protocol");
    // The provider-kind partition comes first, override partitions after in
    // first-appearance order; both routes share the provider id.
    assert_eq!(plan.routes[0].kind(), ProviderKind::OpenAiResponse);
    assert_eq!(plan.routes[1].kind(), ProviderKind::Anthropic);
    assert_eq!(hya_provider::Provider::id(&plan.routes[0]), "12th");
    assert_eq!(hya_provider::Provider::id(&plan.routes[1]), "12th");
    // Each route claims exactly its partition's models.
    let claims = |route: &hya_provider::HttpProvider, model: &str| {
        hya_provider::Provider::capabilities(route, &hya_proto::ModelRef::new(model)).is_some()
    };
    assert!(claims(&plan.routes[0], "gpt-6-astra"));
    assert!(!claims(&plan.routes[0], "claude-opus-5"));
    assert!(claims(&plan.routes[1], "claude-opus-5"));
    assert!(claims(&plan.routes[1], "glm-exp"));
    assert!(!claims(&plan.routes[1], "gpt-6-astra"));
    // The catalog rows keep the provider-wide partition order (provider
    // kind first); intra-partition order is not a contract because a
    // route's model set is a HashSet and the snapshot re-sorts rows.
    let mut ids: Vec<&str> = plan
        .models
        .iter()
        .map(|row| row.model_id.as_str())
        .collect();
    assert_eq!(
        ids[0], "gpt-6-astra",
        "the provider-kind partition comes first"
    );
    ids.sort_unstable();
    assert_eq!(ids, vec!["claude-opus-5", "glm-exp", "gpt-6-astra"]);
    // Router-level resolution picks the route that claims the model ref.
    let mut router = ProviderRouter::new();
    for route in plan.routes {
        router = router.with(std::sync::Arc::new(route));
    }
    let resolved = |model: &str| {
        router
            .resolve(&hya_proto::ModelRef::new(model))
            .map(|route| hya_provider::Provider::id(route.as_ref()).to_string())
    };
    assert_eq!(resolved("12th/gpt-6-astra").as_deref(), Some("12th"));
    assert_eq!(resolved("12th/claude-opus-5").as_deref(), Some("12th"));
    assert_eq!(resolved("claude-opus-5").as_deref(), Some("12th"));
    assert_eq!(resolved("missing-model").as_deref(), None);
}

#[test]
fn model_kind_override_applies_effective_kind_to_cached_rows() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  12th:\n    kind: anthropic\n    base_url: https://api.12th.day/v1\n    models:\n      - id: gpt-6-astra\n        kind: openai\n";
    let provider = parse_providers(yaml).unwrap().into_iter().next().unwrap();
    // A cached row without effort metadata: the merged row's fallback menu
    // must come from the entry's effective kind, not the provider's.
    let row = cached("gpt-6-astra");
    let plan = plan_for_provider(&provider, &no_key(), &[row], None).unwrap();
    assert_eq!(plan.routes.len(), 1);
    assert_eq!(plan.routes[0].kind(), ProviderKind::OpenAiCompatible);
    let merged = plan
        .models
        .iter()
        .find(|row| row.model_id == "gpt-6-astra")
        .expect("merged row");
    assert_eq!(
        merged.reasoning_variants,
        vec!["minimal", "low", "medium", "high", "xhigh"],
        "the fallback menu follows the entry's effective kind, not the provider's"
    );
}

#[test]
fn set_model_entry_preserves_an_entry_kind_override() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "model-kind-keep",
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - id: m\n        kind: anthropic\n        name: Mine\n",
    );
    set_model_entry(
        &path,
        "gw",
        "m",
        &ModelEntryOverride {
            output_limit: Some(4_096),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("kind: anthropic"), "{raw}");
    assert!(raw.contains("output: 4096"), "{raw}");
    let provider = parse_providers(&raw).unwrap().into_iter().next().unwrap();
    assert_eq!(provider.models[0].kind, ProviderKind::Anthropic);
}

#[tokio::test]
async fn configured_model_limits_reach_route_capabilities() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(GLM_LIMIT_YAML)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let credential = ProviderCredential {
        token: Some("test".to_string()),
        use_grok_session: false,
        use_codex_session: false,
        account_id: None,
        use_oauth_refresh: false,
    };
    let plan = plan_for_provider(&provider, &credential, &[], None).unwrap();
    let caps = |model: &str| {
        plan.models
            .iter()
            .find(|row| row.model_id == model)
            .map(|row| row.capabilities.clone())
            .unwrap()
    };
    assert_eq!(caps("glm-5.3-flash").max_output, 131_072);
    assert_eq!(caps("glm-5.3-flash").max_context, 1_048_576);
    assert_eq!(caps("glm-5.3").max_output, 0, "unlimited rows stay unknown");
    assert_eq!(
        caps("glm-5.3").max_context,
        0,
        "a model without a known window reports it unknown"
    );
    assert_eq!(caps("glm-5.3-ctx").max_context, 262_144);
    assert_eq!(caps("glm-5.3-ctx").max_output, 0);
    let route = plan.routes.first().unwrap();
    assert_eq!(
        hya_provider::Provider::capabilities(
            route,
            &hya_proto::ModelRef::new("12th/glm-5.3-flash")
        )
        .map(|caps| caps.max_output),
        Some(131_072)
    );
}

fn cached(id: &str) -> crate::model_cache::CachedModel {
    crate::model_cache::CachedModel {
        id: id.to_string(),
        tools: true,
        fetched_at_ms: 1,
        ..crate::model_cache::CachedModel::default()
    }
}

fn no_key() -> ProviderCredential {
    ProviderCredential {
        token: None,
        use_grok_session: false,
        use_codex_session: false,
        account_id: None,
        use_oauth_refresh: false,
    }
}

#[test]
fn effective_models_merge_cache_and_config_per_id_with_field_level_override() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(
        r#"
providers:
  gw:
    kind: openai-response
    base_url: https://gw.example/v1
    models:
      - id: shared
        name: Shared (config)
        limit:
          context: 100000
      - id: pinned-only
        reasoning: false
"#,
    )
    .unwrap()
    .remove(0);
    let mut shared = cached("shared");
    shared.display_name = Some("Shared (remote)".into());
    shared.context_limit = 50_000;
    shared.output_limit = 8_000;
    shared.reasoning_variants = vec!["low".into(), "high".into()];
    shared.reasoning_default = Some("low".into());
    let mut remote = cached("remote-only");
    remote.display_name = Some("Remote Only".into());
    remote.context_limit = 32_000;

    let merged = merge_provider_models(&provider, &[shared, remote]);
    let by_id = |id: &str| merged.iter().find(|model| model.id == id).unwrap();

    let shared = by_id("shared");
    assert_eq!(shared.source, ModelCatalogSource::Overridden);
    assert_eq!(shared.display_name.as_deref(), Some("Shared (config)"));
    assert_eq!(shared.limit.context, 100_000, "config field wins");
    assert_eq!(shared.limit.output, 8_000, "unset config field falls back");
    assert_eq!(shared.reasoning_variants, vec!["low", "high"]);
    assert_eq!(shared.reasoning_default, None);

    let remote = by_id("remote-only");
    assert_eq!(remote.source, ModelCatalogSource::Discovered);
    assert_eq!(remote.display_name.as_deref(), Some("Remote Only"));
    assert_eq!(remote.limit.context, 32_000);
    assert_eq!(
        remote.reasoning_variants,
        ProviderKind::OpenAiResponse.reasoning_variants(),
        "no remote variants: the kind menu applies"
    );

    let pinned = by_id("pinned-only");
    assert_eq!(pinned.source, ModelCatalogSource::Configured);
    assert!(pinned.reasoning_variants.is_empty(), "reasoning: false");
    assert_eq!(pinned.reasoning_default, None);
    assert_eq!(merged.len(), 3);

    let plan = plan_for_provider(&provider, &no_key(), &[], None).unwrap();
    assert_eq!(plan.state.source, ProviderCatalogSource::Configured);
    let rows = plan_for_provider(
        &provider,
        &no_key(),
        &[cached("shared"), cached("remote-only")],
        None,
    )
    .unwrap()
    .models;
    let shared_row = rows.iter().find(|row| row.model_id == "shared").unwrap();
    assert_eq!(shared_row.source, ModelCatalogSource::Overridden);
    assert_eq!(shared_row.display_name.as_deref(), Some("Shared (config)"));
    assert_eq!(shared_row.capabilities.max_context, 100_000);
}

/// Catalog rows report metadata the model does not publish as unknown:
/// no context window and no reasoning claim, while the route keeps its
/// runtime fallbacks (context window and the kind's effort menu).
#[test]
fn catalog_rows_report_unknown_metadata_as_unknown() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(
        r#"
providers:
  gw:
    kind: openai-response
    base_url: https://gw.example/v1
    models:
      - plain
      - id: thinks
        reasoning: true
      - id: never
        reasoning: false
"#,
    )
    .unwrap()
    .remove(0);
    let mut declared = cached("remote-reasoning");
    declared.reasoning_variants = vec!["low".into()];
    let plan = plan_for_provider(
        &provider,
        &no_key(),
        &[cached("remote-bare"), declared],
        None,
    )
    .unwrap();
    let row = |id: &str| {
        plan.models
            .iter()
            .find(|row| row.model_id == id)
            .cloned()
            .unwrap()
    };
    for id in ["plain", "remote-bare"] {
        assert_eq!(row(id).capabilities.max_context, 0, "{id}");
        assert_eq!(row(id).reasoning, None, "{id}");
        assert!(
            !row(id).reasoning_variants.is_empty(),
            "{id}: the kind's effort menu still applies at runtime"
        );
    }
    assert_eq!(row("thinks").reasoning, Some(true));
    assert_eq!(row("never").reasoning, Some(false));
    assert_eq!(row("remote-reasoning").reasoning, Some(true));
    let route = plan.routes.first().unwrap();
    assert_eq!(
        hya_provider::Provider::capabilities(route, &hya_proto::ModelRef::new("gw/plain"))
            .map(|caps| caps.max_context),
        Some(200_000),
        "the runtime keeps its context fallback"
    );
}

#[test]
fn cached_output_limit_never_contradicts_a_configured_context() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(
            "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - id: m\n        limit:\n          context: 1000\n",
        )
        .unwrap()
        .remove(0);
    let mut row = cached("m");
    row.output_limit = 4_096;
    let merged = merge_provider_models(&provider, &[row]);
    assert_eq!(merged[0].limit.context, 1_000);
    assert_eq!(merged[0].limit.output, 0);
}

/// Cached model-list rows refine the variant menu but never choose the
/// request effort: only the explicit configured default does.
#[test]
fn cached_variants_never_override_an_explicit_configured_default() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(
        r#"
providers:
  gw:
    kind: openai-response
    base_url: https://gw.example/v1
    models:
      - id: shared
        reasoning:
          default: high
"#,
    )
    .unwrap()
    .remove(0);
    let mut row = cached("shared");
    row.reasoning_variants = vec!["low".into(), "medium".into()];
    row.reasoning_default = Some("low".into());

    let merged = merge_provider_models(&provider, &[row]);
    let shared = merged.iter().find(|model| model.id == "shared").unwrap();

    assert_eq!(shared.source, ModelCatalogSource::Overridden);
    assert_eq!(shared.reasoning_variants, vec!["low", "medium"]);
    assert_eq!(
        shared.reasoning_default,
        Some(hya_provider::ReasoningEffort::High)
    );
}

fn temp_config(label: &str, yaml: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let dir = std::env::temp_dir().join(format!("hya-cfg-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn upsert_provider_entry_preserves_other_keys_and_creates_discovery_providers() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "upsert",
        "default_model: gw/m\nprovider_retry:\n  max_attempts: 5\nproviders:\n  gw:\n    kind: openai\n    base_url: https://old.example/v1\n    api_key: \"{env:GW_KEY}\"\n    models: [m]\n    retry:\n      max_attempts: 2\n",
    );
    assert!(!upsert_provider_entry(&path, "gw", "anthropic", "https://new.example/v1").unwrap());
    assert!(upsert_provider_entry(&path, "fresh", "google", "https://g.example").unwrap());
    let file = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let gw = file.providers.get("gw").unwrap();
    assert_eq!(ProviderKind::from(gw.kind), ProviderKind::Anthropic);
    assert_eq!(gw.base_url, "https://new.example/v1");
    assert_eq!(gw.api_key.as_deref(), Some("{env:GW_KEY}"));
    assert_eq!(gw.models.len(), 1);
    assert!(gw.retry.is_some());
    assert!(file.provider_retry.is_some());
    assert_eq!(file.default_model.as_deref(), Some("gw/m"));
    let fresh = file.providers.get("fresh").unwrap();
    assert!(fresh.models.is_empty());

    assert!(matches!(
        upsert_provider_entry(&path, "x", "bogus", "https://x"),
        Err(ConfigEditError::Invalid(_))
    ));
}

#[test]
fn set_and_remove_model_entries_round_trip_string_and_mapping_forms() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "models",
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - plain\n      - id: detailed\n        reasoning:\n          variants: [low, high]\n",
    );
    set_model_entry(
        &path,
        "gw",
        "plain",
        &ModelEntryOverride {
            display_name: Some("Plain".into()),
            context_limit: Some(64_000),
            output_limit: Some(4_096),
            reasoning: Some(false),
        },
    )
    .unwrap();
    set_model_entry(
        &path,
        "gw",
        "vendor/new:free",
        &ModelEntryOverride {
            output_limit: Some(1_000),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    set_model_entry(
        &path,
        "gw",
        "detailed",
        &ModelEntryOverride {
            reasoning: Some(true),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let parsed = parse_providers(&std::fs::read_to_string(&path).unwrap())
        .unwrap()
        .remove(0);
    let plain = parsed.models.iter().find(|m| m.id == "plain").unwrap();
    assert_eq!(plain.display_name.as_deref(), Some("Plain"));
    assert_eq!(
        plain.limit.as_ref().map(|limit| limit.context),
        Some(64_000)
    );
    assert_eq!(plain.limit.as_ref().map(|limit| limit.output), Some(4_096));
    assert!(plain.reasoning_variants.is_empty());
    let added = parsed
        .models
        .iter()
        .find(|m| m.id == "vendor/new:free")
        .unwrap();
    assert_eq!(added.limit.as_ref().map(|limit| limit.output), Some(1_000));
    let detailed = parsed.models.iter().find(|m| m.id == "detailed").unwrap();
    assert_eq!(
        detailed.reasoning_variants,
        vec!["low", "high"],
        "mapping kept"
    );

    // Clearing every managed field writes the entry back as a string.
    set_model_entry(
        &path,
        "gw",
        "vendor/new:free",
        &ModelEntryOverride {
            output_limit: Some(0),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("- vendor/new:free\n"), "{raw}");

    // Validation: an output above the context is rejected, file untouched.
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(matches!(
        set_model_entry(
            &path,
            "gw",
            "plain",
            &ModelEntryOverride {
                context_limit: Some(10),
                output_limit: Some(20),
                ..ModelEntryOverride::default()
            },
        ),
        Err(ConfigEditError::Invalid(_))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    assert!(remove_model_entry(&path, "gw", "vendor/new:free").unwrap());
    assert!(!remove_model_entry(&path, "gw", "vendor/new:free").unwrap());
    assert!(matches!(
        remove_model_entry(&path, "missing", "m"),
        Err(ConfigEditError::NotFound(_))
    ));
    assert!(matches!(
        set_model_entry(&path, "missing", "m", &ModelEntryOverride::default()),
        Err(ConfigEditError::NotFound(_))
    ));
}

#[test]
fn set_model_entry_patches_only_the_fields_present() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "model-patch",
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - id: m\n        name: Mine\n        limit:\n          context: 1000\n          output: 100\n        reasoning: true\n      - plain\n",
    );
    let entry = |id: &str| {
        parse_providers(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .remove(0)
            .models
            .into_iter()
            .find(|m| m.id == id)
            .unwrap()
    };

    // Editing one field keeps the others.
    set_model_entry(
        &path,
        "gw",
        "m",
        &ModelEntryOverride {
            output_limit: Some(200),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let m = entry("m");
    assert_eq!(m.display_name.as_deref(), Some("Mine"));
    assert_eq!(
        m.limit.as_ref().map(|l| (l.context, l.output)),
        Some((1000, 200))
    );
    assert!(!m.reasoning_variants.is_empty(), "reasoning kept");

    // Validation uses the merged values: output above the kept context.
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(matches!(
        set_model_entry(
            &path,
            "gw",
            "m",
            &ModelEntryOverride {
                output_limit: Some(2000),
                ..ModelEntryOverride::default()
            },
        ),
        Err(ConfigEditError::Invalid(_))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // An empty name clears `name`; limits stay.
    set_model_entry(
        &path,
        "gw",
        "m",
        &ModelEntryOverride {
            display_name: Some("  ".into()),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let m = entry("m");
    assert_eq!(m.display_name, None);
    assert_eq!(
        m.limit.as_ref().map(|l| (l.context, l.output)),
        Some((1000, 200))
    );

    // Zero limits clear them and drop the empty `limit:` map.
    set_model_entry(
        &path,
        "gw",
        "m",
        &ModelEntryOverride {
            context_limit: Some(0),
            output_limit: Some(0),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("limit"), "{raw}");
    assert!(!entry("m").reasoning_variants.is_empty(), "reasoning kept");

    // A request that sets nothing leaves a string entry alone and adds
    // a missing model as a bare id.
    set_model_entry(&path, "gw", "plain", &ModelEntryOverride::default()).unwrap();
    set_model_entry(&path, "gw", "added", &ModelEntryOverride::default()).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("- plain\n"), "{raw}");
    assert!(raw.contains("- added\n"), "{raw}");

    // Setting a field on a string entry converts it to a mapping.
    set_model_entry(
        &path,
        "gw",
        "plain",
        &ModelEntryOverride {
            context_limit: Some(500),
            ..ModelEntryOverride::default()
        },
    )
    .unwrap();
    assert_eq!(entry("plain").limit.as_ref().map(|l| l.context), Some(500));
}

#[test]
fn detailed_model_entries_accept_name_and_boolean_reasoning() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(
            "providers:\n  gw:\n    kind: anthropic\n    base_url: https://gw.example/v1\n    models:\n      - id: a\n        name: Model A\n        reasoning: true\n      - id: b\n        reasoning: false\n",
        )
        .unwrap()
        .remove(0);
    assert_eq!(parsed.models[0].display_name.as_deref(), Some("Model A"));
    assert_eq!(
        parsed.models[0].reasoning_variants,
        ProviderKind::Anthropic.reasoning_variants()
    );
    assert!(!parsed.models[0].variants_configured);
    assert!(parsed.models[1].reasoning_variants.is_empty());
    assert!(parsed.models[1].variants_configured);
}
