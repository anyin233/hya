#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_provider::{
    ModelCatalogSource, OpenAiChatProtocol, Protocol, ProviderCatalogSource, ProviderKinds,
    ProviderRouter, ReasoningEffort,
};

use super::catalog::{ProviderPlanResult, plan_for_provider};
use super::file::parse_config;
use super::provider::{
    ModelConfig, ParsedProvider, ProviderCredential, merge_provider_models,
    resolve_provider_credential_with, resolve_providers,
};
use super::*;
use crate::test_env::{EnvGuard, StableEnvGuard};

const FIXTURE: &str = "
schema_version: 2
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
    let with_version = if yaml.contains("schema_version") {
        std::borrow::Cow::Borrowed(yaml)
    } else if yaml.trim() == "{}" {
        std::borrow::Cow::Borrowed("schema_version: 2\n")
    } else {
        std::borrow::Cow::Owned(format!("schema_version: 2\n{yaml}"))
    };
    resolve_providers(&parse_config(&with_version)?)
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
        account_id: None,
        use_oauth_refresh: false,
    }
}

fn kinds() -> std::sync::Arc<ProviderKinds> {
    crate::provider_kinds::builtin().unwrap()
}

fn plan(provider: &ParsedProvider) -> anyhow::Result<ProviderPlanResult> {
    plan_for_provider(provider, &no_key(), &[], None, &kinds())
}

/// The effort menu a provider's catalog row publishes for `model_id`.
fn row_variants(provider: &ParsedProvider, model_id: &str) -> Vec<String> {
    plan(provider)
        .unwrap()
        .models
        .into_iter()
        .find(|row| row.model_id == model_id)
        .unwrap()
        .reasoning_variants
}

fn labels(protocol: &dyn Protocol) -> Vec<String> {
    protocol
        .reasoning_variant_labels()
        .iter()
        .map(|label| (*label).to_string())
        .collect()
}

fn temp_config(label: &str, yaml: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "hya-cfg-{label}-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn parses_provider_kinds_aliases_empty_and_no_apikey() {
    let _env = StableEnvGuard::new();
    assert!(parse_providers("{}").unwrap().is_empty());

    let parsed = parse_providers(FIXTURE).unwrap();
    assert_eq!(parsed.len(), 4);
    for (id, kind) in [
        ("gw-oai", "openai"),
        ("gw-anth", "anthropic"),
        ("gw-google", "google"),
    ] {
        let provider = parsed.iter().find(|p| p.id == id).unwrap();
        assert_eq!(plan(provider).unwrap().state.kind, kind);
    }

    for kind in ["openai", "openai-compatible", "openai-completion"] {
        let yaml = format!(
            "providers:\n  gw:\n    kind: {kind}\n    base_url: https://gw.example/v1\n    models: [m1]\n"
        );
        let p = parse_providers(&yaml).unwrap();
        let plan = plan(&p[0]).unwrap();
        assert_eq!((plan.state.kind.as_str(), plan.routes.len()), ("openai", 1));
        assert_eq!(plan.routes[0].kind(), "openai");
        assert_eq!(p[0].api_key, None);
        assert_eq!(p[0].models[0].reasoning_default, None);
    }
    let codex = parse_providers("providers:\n  c:\n    kind: openai-codex\n    base_url: https://chatgpt.com/backend-api/codex\n    models: [gpt-5.3-codex]\n").unwrap();
    let codex_state = plan(&codex[0]).unwrap().state;
    assert_eq!(
        (
            codex_state.kind.as_str(),
            codex_state.supports_oauth,
            codex_state.min_output_tokens
        ),
        ("openai-codex", true, 16)
    );
    let grok = parse_providers("providers:\n  g:\n    kind: grok-build\n    base_url: https://grok.example/v1\n    models: [grok-4.5]\n").unwrap();
    assert_eq!(
        row_variants(&grok[0], "grok-4.5"),
        vec!["low", "medium", "high"]
    );
    assert_eq!(grok[0].models[0].reasoning_default, None);
}

#[test]
fn provider_retry_precedence_defaults_global_per_provider_and_env() {
    let mut env = EnvGuard::new();
    for p in parse_providers(FIXTURE).unwrap() {
        assert_eq!(p.retry, hya_provider::RetryConfig::default());
    }
    let yaml = "provider_retry:\n  max_attempts: 5\n  backoff_base_ms: 250\n  backoff_max_ms: 60000\nproviders:\n  tuned:\n    kind: openai\n    base_url: https://a/v1\n    api_key: x\n    models: [m1]\n    retry:\n      max_attempts: 2\n  stock:\n    kind: openai\n    base_url: https://b/v1\n    api_key: x\n    models: [m2]\n";
    let parsed = parse_providers(yaml).unwrap();
    let tuned = parsed.iter().find(|p| p.id == "tuned").unwrap();
    let stock = parsed.iter().find(|p| p.id == "stock").unwrap();
    assert_eq!(tuned.retry.max_attempts, 2);
    assert_eq!(tuned.retry.backoff_base, Duration::from_millis(250));
    assert_eq!(tuned.retry.backoff_max, Duration::from_millis(60_000));
    assert_eq!(stock.retry.max_attempts, 5);

    env.set("HYA_PROVIDER_RETRY_MAX_ATTEMPTS", "7");
    let env_over = parse_providers(yaml).unwrap();
    assert_eq!(
        env_over
            .iter()
            .find(|p| p.id == "tuned")
            .unwrap()
            .retry
            .max_attempts,
        7
    );
}

#[test]
fn credentials_and_secret_templates_resolve_by_provider_kind() {
    let mut env = EnvGuard::new();
    env.set("HYA_TEST_KEY_XYZ", "resolved-secret");
    assert_eq!(
        resolve_secret("{env:HYA_TEST_KEY_XYZ}").unwrap(),
        "resolved-secret"
    );
    assert_eq!(resolve_secret("literal-key").unwrap(), "literal-key");

    let cases = [
        (Some("oauth"), Some("inline"), Some("oauth")),
        (None, Some("inline"), Some("inline")),
        (Some("jwt"), None, Some("jwt")),
        (Some(" "), None, None),
    ];
    for (login, inline, tok) in cases {
        let cred = resolve_provider_credential_with(login, inline);
        assert_eq!(cred.and_then(|cred| cred.token).as_deref(), tok);
    }
}

#[test]
fn reasoning_variants_order_boolean_and_validation() {
    let _env = StableEnvGuard::new();
    let parsed = parse_providers(FIXTURE).unwrap();
    let variants =
        |id: &str, model: &str| row_variants(parsed.iter().find(|p| p.id == id).unwrap(), model);
    assert_eq!(
        variants("gw-oai", "gpt-5.5"),
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
    assert_eq!(
        variants("gw-anth", "claude-sonnet-4-6"),
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        variants("gw-google", "gemini-2.0-flash"),
        vec!["minimal", "low", "medium", "high", "max"]
    );

    let detailed = parse_providers(
        "providers:\n  gw:\n    kind: openai-response\n    base_url: https://gw.example/v1\n    models:\n      - id: ordered\n        name: Ordered\n        reasoning:\n          default: medium\n          variants: [max, high, low, medium]\n      - id: on\n        reasoning: true\n      - id: off\n        reasoning: false\n",
    )
    .unwrap()
    .remove(0);
    assert_eq!(detailed.models[0].display_name.as_deref(), Some("Ordered"));
    assert_eq!(
        detailed.models[0].reasoning_variants,
        vec!["max", "high", "low", "medium"]
    );
    assert_eq!(
        detailed.models[0].reasoning_default,
        Some(ReasoningEffort::Medium)
    );
    assert!(!detailed.models[1].variants_configured);
    assert!(
        detailed.models[2].reasoning_variants.is_empty() && detailed.models[2].variants_configured
    );

    let unknown = parse_providers(
        "providers:\n  g:\n    kind: openai-responses\n    base_url: https://x\n    models: [m]\n",
    )
    .unwrap()
    .remove(0);
    assert!(
        format!("{:#}", plan(&unknown).err().unwrap())
            .contains("provider g has unknown kind openai-responses")
    );
    assert!(parse_providers("providers:\n  g:\n    kind: openai-response\n    base_url: https://x\n    models:\n      - id: m\n        reasoning:\n          variants: [medium, extreme]\n").unwrap_err().to_string().contains("unknown reasoning variant extreme"));
    assert!(parse_providers("providers:\n  g:\n    kind: openai-response\n    base_url: https://x\n    models:\n      - id: m\n        reasoning:\n          default: high\n          variants: [low, medium]\n").unwrap_err().to_string().contains("reasoning default high is not advertised"));
    let off_menu = parse_providers("providers:\n  g:\n    kind: grok-build\n    base_url: https://x\n    models:\n      - id: m\n        reasoning:\n          default: max\n").unwrap().remove(0);
    assert!(
        plan(&off_menu)
            .err()
            .unwrap()
            .to_string()
            .contains("reasoning default max is not advertised")
    );
}

#[test]
fn model_limits_and_modalities_validate_and_reach_route_capabilities() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  12th:\n    kind: anthropic\n    base_url: https://api.12th.day/v1\n    models:\n      - glm-5.3\n      - id: glm-5.3-flash\n        limit: { context: 1048576, output: 131072 }\n        modalities: { input: [text, image] }\n      - id: glm-5.3-ctx\n        limit: { context: 262144 }\n        modalities: { input: [text] }\n";
    let provider = parse_providers(yaml).unwrap().remove(0);
    let plan = plan(&provider).unwrap();
    let caps = |id: &str| {
        plan.models
            .iter()
            .find(|r| r.model_id == id)
            .unwrap()
            .capabilities
            .clone()
    };
    assert_eq!(
        (
            caps("glm-5.3").max_context,
            caps("glm-5.3").max_output,
            caps("glm-5.3").image_input
        ),
        (0, 0, None)
    );
    assert_eq!(
        (
            caps("glm-5.3-flash").max_context,
            caps("glm-5.3-flash").max_output,
            caps("glm-5.3-flash").image_input
        ),
        (1_048_576, 131_072, Some(true))
    );
    assert_eq!(
        (
            caps("glm-5.3-ctx").max_context,
            caps("glm-5.3-ctx").max_output,
            caps("glm-5.3-ctx").image_input
        ),
        (262_144, 0, Some(false))
    );
    assert_eq!(
        hya_provider::Provider::capabilities(
            &plan.routes[0],
            &hya_proto::ModelRef::new("12th/glm-5.3-flash")
        )
        .map(|c| c.max_output),
        Some(131_072)
    );

    for (limit, expected) in [
        ("{ output: 0 }", "limit.output must be a positive integer"),
        ("{ context: 0 }", "limit.context must be a positive integer"),
        ("{ output: -5 }", "limit.output must be a positive integer"),
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
        let err = format!("{:#}", parse_providers(&format!("providers:\n  12th:\n    kind: anthropic\n    base_url: https://x\n    models:\n      - id: m\n        limit: {limit}\n")).unwrap_err());
        assert!(
            err.contains(expected) || err.contains("parse hya config.yaml"),
            "{limit}: {err}"
        );
    }
    assert!(format!("{:#}", parse_providers("providers:\n  g:\n    kind: openai\n    base_url: https://x\n    models:\n      - id: m\n        modalities: { input: image }\n").unwrap_err()).contains("modalities"));
}

#[test]
fn model_kind_override_partitions_routes_and_applies_to_cached_rows() {
    let _env = StableEnvGuard::new();
    let yaml = "providers:\n  12th:\n    kind: openai-response\n    base_url: https://api.12th.day/v1\n    models:\n      - gpt-6-astra\n      - id: claude-opus-5\n        kind: anthropic\n      - id: glm-exp\n        kind: anthropic\n        reasoning: { variants: [low, high] }\n";
    let provider = parse_providers(yaml).unwrap().remove(0);
    assert_eq!(
        row_variants(&provider, "claude-opus-5"),
        labels(&hya_provider::AnthropicMessagesProtocol)
    );
    assert_eq!(row_variants(&provider, "glm-exp"), vec!["low", "high"]);

    let plan = plan(&provider).unwrap();
    assert_eq!(plan.routes.len(), 2);
    assert_eq!(plan.routes[0].kind(), "openai-response");
    assert_eq!(plan.routes[1].kind(), "anthropic");
    let mut router = ProviderRouter::new();
    for route in plan.routes {
        router = router.with(std::sync::Arc::new(route));
    }
    assert!(
        router
            .resolve(&hya_proto::ModelRef::new("12th/gpt-6-astra"))
            .is_some()
    );
    assert!(
        router
            .resolve(&hya_proto::ModelRef::new("12th/claude-opus-5"))
            .is_some()
    );
    assert!(
        router
            .resolve(&hya_proto::ModelRef::new("missing"))
            .is_none()
    );

    // Cached row follows the model's effective kind, not the provider's.
    let overridden = parse_providers("providers:\n  12th:\n    kind: anthropic\n    base_url: https://x\n    models:\n      - id: gpt-6\n        kind: openai\n").unwrap().remove(0);
    let plan =
        plan_for_provider(&overridden, &no_key(), &[cached("gpt-6")], None, &kinds()).unwrap();
    assert_eq!(plan.routes[0].kind(), "openai");
    assert_eq!(
        plan.models[0].reasoning_variants,
        labels(&OpenAiChatProtocol)
    );

    // Unknown kind rejected when kinds resolve; every registered label accepted.
    let unknown = parse_providers("providers:\n  12th:\n    kind: openai\n    base_url: https://x\n    models:\n      - id: m\n        kind: anthropic-msgs\n").unwrap().remove(0);
    assert!(
        format!(
            "{:#}",
            plan_for_provider(&unknown, &no_key(), &[], None, &kinds())
                .err()
                .unwrap()
        )
        .contains("provider 12th model m has unknown kind anthropic-msgs")
    );
    for label in kinds().labels() {
        let provider = parse_providers(&format!("providers:\n  12th:\n    kind: openai\n    base_url: https://x\n    models:\n      - id: m\n        kind: {label}\n")).unwrap().remove(0);
        plan_for_provider(&provider, &no_key(), &[], None, &kinds()).unwrap();
    }
}

#[test]
fn cache_and_config_merge_preserves_overrides_unknowns_and_constraints() {
    let _env = StableEnvGuard::new();
    let provider = parse_providers(
        "providers:\n  gw:\n    kind: openai-response\n    base_url: https://gw.example/v1\n    models:\n      - id: shared\n        name: Shared (config)\n        limit: { context: 1000 }\n        reasoning: { default: high }\n      - id: pinned-only\n        reasoning: false\n      - plain\n",
    )
    .unwrap()
    .remove(0);

    let mut shared = cached("shared");
    shared.display_name = Some("Shared (remote)".into());
    shared.context_limit = 50_000;
    shared.output_limit = 8_000; // exceeds configured context 1000 -> clamped to 0
    shared.reasoning_variants = vec!["low".into(), "medium".into()];
    shared.reasoning_default = Some("low".into());
    let mut remote = cached("remote-only");
    remote.context_limit = 32_000;
    let bare = cached("remote-bare");

    let merged = merge_provider_models(
        &provider,
        &[shared.clone(), remote.clone(), bare.clone()],
        &kinds(),
    )
    .unwrap();
    let by_id = |id: &str| merged.iter().find(|m| m.id == id).unwrap();
    assert_eq!(by_id("shared").source, ModelCatalogSource::Overridden);
    assert_eq!(
        by_id("shared").display_name.as_deref(),
        Some("Shared (config)")
    );
    assert_eq!(
        (by_id("shared").limit.context, by_id("shared").limit.output),
        (1_000, 0)
    );
    assert_eq!(by_id("shared").reasoning_variants, vec!["low", "medium"]);
    assert_eq!(
        by_id("shared").reasoning_default,
        Some(ReasoningEffort::High)
    );
    assert_eq!(by_id("remote-only").source, ModelCatalogSource::Discovered);
    assert_eq!(by_id("pinned-only").source, ModelCatalogSource::Configured);
    assert!(by_id("pinned-only").reasoning_variants.is_empty());

    let cfg_plan = plan(&provider).unwrap();
    assert_eq!(cfg_plan.state.source, ProviderCatalogSource::Configured);
    let plan = plan_for_provider(
        &provider,
        &no_key(),
        &[shared, remote, bare],
        None,
        &kinds(),
    )
    .unwrap();
    for id in ["plain", "remote-bare"] {
        let row = plan.models.iter().find(|r| r.model_id == id).unwrap();
        assert_eq!((row.capabilities.max_context, row.reasoning), (0, None));
    }
    assert_eq!(
        hya_provider::Provider::capabilities(
            &plan.routes[0],
            &hya_proto::ModelRef::new("gw/plain")
        )
        .map(|c| c.max_context),
        Some(200_000)
    );
}

#[test]
fn upsert_provider_and_oauth_entries_preserve_other_keys_and_models() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "upsert",
        "schema_version: 2\ndefault_model: gw/m\nprovider_retry:\n  max_attempts: 5\nproviders:\n  gw:\n    kind: openai\n    base_url: https://old.example/v1\n    api_key: \"{env:GW_KEY}\"\n    models: [m]\n    retry: { max_attempts: 2 }\n",
    );
    assert!(!upsert_provider_entry(&path, "gw", "anthropic", "https://new.example/v1").unwrap());
    assert!(upsert_provider_entry(&path, "fresh", "google", "https://g.example").unwrap());
    // The edit writes any label: callers check it against the registered kinds.
    assert!(upsert_provider_entry(&path, "x", "bogus", "https://x").unwrap());
    let file = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let gw = file.providers.get("gw").unwrap();
    assert_eq!(
        (
            gw.kind.as_str(),
            gw.base_url.as_str(),
            gw.api_key.as_deref()
        ),
        ("anthropic", "https://new.example/v1", Some("{env:GW_KEY}"))
    );
    assert_eq!(file.providers["x"].kind, "bogus");
    assert!(file.providers.get("fresh").unwrap().models.is_empty());

    // OAuth upsert creates with empty models and preserves explicit user models on re-upsert.
    let oauth_path = temp_config("oauth", "").parent().unwrap().join("new.yaml");
    upsert_oauth_provider(
        &oauth_path,
        "codex",
        "openai-codex",
        "https://chatgpt.com/backend-api/codex",
    )
    .unwrap();
    let parsed = parse_config(&std::fs::read_to_string(&oauth_path).unwrap()).unwrap();
    assert!(parsed.providers["codex"].models.is_empty());
    assert_eq!(parsed.default_model.as_deref(), Some("hya/offline"));
    std::fs::write(&oauth_path, "schema_version: 2\ndefault_model: codex/user-model\nproviders:\n  codex:\n    kind: openai-codex\n    base_url: https://old\n    models: [user-model]\n").unwrap();
    upsert_oauth_provider(
        &oauth_path,
        "codex",
        "openai-codex",
        "https://chatgpt.com/backend-api/codex",
    )
    .unwrap();
    let parsed = parse_config(&std::fs::read_to_string(&oauth_path).unwrap()).unwrap();
    assert!(
        matches!(parsed.providers["codex"].models.as_slice(), [ModelConfig::Id(id)] if id == "user-model")
    );
}

#[test]
fn set_and_remove_model_entries_patch_preserve_kind_and_validate() {
    let _env = StableEnvGuard::new();
    let path = temp_config(
        "model-edit",
        "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - plain\n      - id: typed\n        kind: anthropic\n        name: Mine\n        limit: { context: 1000, output: 100 }\n        reasoning: { variants: [low, high] }\n",
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

    // Patching one field keeps kind, name, limits, and reasoning variants.
    set_model_entry(
        &path,
        "gw",
        "typed",
        &ModelEntryOverride {
            output_limit: Some(200),
            ..Default::default()
        },
    )
    .unwrap();
    let m = entry("typed");
    assert_eq!(
        (
            m.kind.as_str(),
            m.display_name.as_deref(),
            m.limit.as_ref().map(|l| (l.context, l.output))
        ),
        ("anthropic", Some("Mine"), Some((1000, 200)))
    );
    assert_eq!(m.reasoning_variants, vec!["low", "high"]);

    // Output above kept context fails without modifying file.
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(matches!(
        set_model_entry(
            &path,
            "gw",
            "typed",
            &ModelEntryOverride {
                output_limit: Some(2000),
                ..Default::default()
            }
        ),
        Err(ConfigEditError::Invalid(_))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // Clearing name and zeroing limits drops limit block; bare string entry converts and reverts cleanly.
    set_model_entry(
        &path,
        "gw",
        "typed",
        &ModelEntryOverride {
            display_name: Some(" ".into()),
            context_limit: Some(0),
            output_limit: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(entry("typed").display_name, None);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("limit"));

    set_model_entry(
        &path,
        "gw",
        "vendor/new:free",
        &ModelEntryOverride {
            output_limit: Some(1000),
            ..Default::default()
        },
    )
    .unwrap();
    set_model_entry(
        &path,
        "gw",
        "vendor/new:free",
        &ModelEntryOverride {
            output_limit: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("- vendor/new:free\n")
    );
    assert!(remove_model_entry(&path, "gw", "vendor/new:free").unwrap());
    assert!(!remove_model_entry(&path, "gw", "vendor/new:free").unwrap());
    assert!(matches!(
        remove_model_entry(&path, "missing", "m"),
        Err(ConfigEditError::NotFound(_))
    ));
    assert!(matches!(
        set_model_entry(&path, "missing", "m", &Default::default()),
        Err(ConfigEditError::NotFound(_))
    ));
}
