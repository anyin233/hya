use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context as _;
use hya_provider::{
    AuthPresence, AuthRefresher, BearerResolver, HttpProvider, ModelCatalogSource, Protocol,
    ProviderAuthFlow, ProviderAuthState, ProviderKinds, ReasoningEffort, RouteCredential,
};
use serde::Deserialize;

use super::file::FileConfig;

#[derive(Debug, Deserialize)]
pub(super) struct ProviderConfig {
    /// Kind label, resolved against the registered provider kinds.
    pub(super) kind: String,
    pub(super) base_url: String,
    /// Literal, `{env:VAR}`, or `{file:path}`. Optional — a token saved via
    /// `hya login` (`~/.config/hya/auth/<id>.yaml`) takes precedence.
    #[serde(default)]
    pub(super) api_key: Option<String>,
    #[serde(default)]
    pub(super) models: Vec<ModelConfig>,
    /// Per-provider overrides on top of the global `provider_retry:` block.
    /// Unset fields inherit the global value.
    #[serde(default)]
    pub(super) retry: Option<ProviderRetryFile>,
    /// Optional per-route override; defaults to whether the kind supports it.
    #[serde(default)]
    prompt_cache: Option<bool>,
}

/// File shape of a retry policy block (`provider_retry:` global default or a
/// provider's `retry:` override). Every field is optional; unset fields fall
/// back to the enclosing layer and finally the engine defaults.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ProviderRetryFile {
    #[serde(default)]
    max_attempts: Option<usize>,
    #[serde(default)]
    backoff_base_ms: Option<u64>,
    #[serde(default)]
    backoff_max_ms: Option<u64>,
}

/// Resolve one route's replay budget: engine defaults ← global block ←
/// per-provider block ← `HYA_PROVIDER_RETRY_*` env overrides.
fn resolve_provider_retry(
    global: Option<&ProviderRetryFile>,
    per_provider: Option<&ProviderRetryFile>,
) -> hya_provider::RetryConfig {
    let defaults = hya_provider::RetryConfig::default();
    let mut retry = hya_provider::RetryConfig {
        max_attempts: global
            .and_then(|block| block.max_attempts)
            .unwrap_or(defaults.max_attempts),
        backoff_base: global
            .and_then(|block| block.backoff_base_ms)
            .filter(|ms| *ms > 0)
            .map_or(defaults.backoff_base, std::time::Duration::from_millis),
        backoff_max: global
            .and_then(|block| block.backoff_max_ms)
            .filter(|ms| *ms > 0)
            .map_or(defaults.backoff_max, std::time::Duration::from_millis),
    };
    if let Some(block) = per_provider {
        if let Some(max_attempts) = block.max_attempts {
            retry.max_attempts = max_attempts;
        }
        if let Some(ms) = block.backoff_base_ms.filter(|ms| *ms > 0) {
            retry.backoff_base = std::time::Duration::from_millis(ms);
        }
        if let Some(ms) = block.backoff_max_ms.filter(|ms| *ms > 0) {
            retry.backoff_max = std::time::Duration::from_millis(ms);
        }
    }
    if let Ok(value) = std::env::var("HYA_PROVIDER_RETRY_MAX_ATTEMPTS")
        && let Ok(parsed) = value.trim().parse::<usize>()
    {
        retry.max_attempts = parsed;
    }
    if let Ok(value) = std::env::var("HYA_PROVIDER_RETRY_BACKOFF_BASE_MS")
        && let Ok(parsed) = value.trim().parse::<u64>()
        && parsed > 0
    {
        retry.backoff_base = std::time::Duration::from_millis(parsed);
    }
    if let Ok(value) = std::env::var("HYA_PROVIDER_RETRY_BACKOFF_MAX_MS")
        && let Ok(parsed) = value.trim().parse::<u64>()
        && parsed > 0
    {
        retry.backoff_max = std::time::Duration::from_millis(parsed);
    }
    retry.normalized()
}

#[derive(Debug)]
pub(super) enum ModelConfig {
    Id(String),
    Detailed(Box<DetailedModelConfig>),
}

impl<'de> Deserialize<'de> for ModelConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ModelVisitor;
        impl<'de> serde::de::Visitor<'de> for ModelVisitor {
            type Value = ModelConfig;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a model id string or model mapping")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ModelConfig::Id(value.to_string()))
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                DetailedModelConfig::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(|model| ModelConfig::Detailed(Box::new(model)))
            }
        }
        deserializer.deserialize_any(ModelVisitor)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct DetailedModelConfig {
    id: String,
    /// Display name shown by pickers and the Provider View; overrides the
    /// remote list's name.
    #[serde(default)]
    name: Option<String>,
    /// Protocol override for this model: one of the provider `kind` labels.
    /// Kept as the raw label so validation can name the provider, model, and
    /// label in errors instead of failing the untagged `ModelConfig` match.
    #[serde(default)]
    kind: Option<String>,
    /// `true`/`false` switch or a `{ default?, variants? }` mapping.
    #[serde(default)]
    reasoning: Option<ReasoningField>,
    /// Optional explicit token limits and supported modalities.
    #[serde(default)]
    limit: Option<ModelLimitFile>,
    #[serde(default)]
    modalities: Option<ModelModalitiesFile>,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "BTreeMap<String, u32>")]
struct ModelLimitFile {
    context: Option<u32>,
    output: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "BTreeMap<String, Vec<String>>")]
struct ModelModalitiesFile {
    input: Option<Vec<String>>,
    _output: Option<Vec<String>>,
}

impl TryFrom<BTreeMap<String, u32>> for ModelLimitFile {
    type Error = anyhow::Error;
    fn try_from(mut fields: BTreeMap<String, u32>) -> Result<Self, Self::Error> {
        let result = Self {
            context: fields.remove("context"),
            output: fields.remove("output"),
        };
        if let Some(key) = fields.keys().next() {
            anyhow::bail!("unknown limit key {key}");
        }
        Ok(result)
    }
}

impl TryFrom<BTreeMap<String, Vec<String>>> for ModelModalitiesFile {
    type Error = anyhow::Error;
    fn try_from(mut fields: BTreeMap<String, Vec<String>>) -> Result<Self, Self::Error> {
        let result = Self {
            input: fields.remove("input"),
            _output: fields.remove("output"),
        };
        if let Some(key) = fields.keys().next() {
            anyhow::bail!("unknown modalities key {key}");
        }
        Ok(result)
    }
}

/// A model entry's `reasoning:` value.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ReasoningField {
    /// `reasoning: false` advertises no efforts; `true` keeps the remote or
    /// provider-kind menu.
    Flag(bool),
    /// Explicit default and/or variant menu.
    Detailed(ModelReasoningConfig),
}

#[derive(Debug, Deserialize)]
struct ModelReasoningConfig {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    variants: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub(super) struct ParsedModel {
    pub(super) id: String,
    /// Effective kind label: the entry's `kind` override, else the provider's.
    pub(super) kind: String,
    /// Configured display name (`name`).
    pub(super) display_name: Option<String>,
    /// Effort menu from config (`reasoning.variants`, `reasoning: false` →
    /// empty); empty and filled from the kind when not configured.
    pub(super) reasoning_variants: Vec<String>,
    /// Whether config set the menu itself (`reasoning.variants` or
    /// `reasoning: false`); otherwise remote-list variants win over the
    /// kind fallback when the model is also in the model cache.
    pub(super) variants_configured: bool,
    /// Explicit configured default; absent means no request effort.
    pub(super) reasoning_default: Option<ReasoningEffort>,
    /// Configured token limits (`0` in a field means unspecified).
    pub(super) limit: Option<hya_provider::ModelLimitOverride>,
    /// Image input from `modalities.input`; `None` when not declared.
    pub(super) image_input: Option<bool>,
    /// Reasoning support the config declares (`reasoning: true|false` or a
    /// `reasoning:` mapping); `None` when the entry has no `reasoning`.
    pub(super) reasoning_declared: Option<bool>,
}

/// Validate one object-form model `limit` block.
///
/// Accepts a mapping with optional `context` and `output` positive `u32`
/// integers; when both are set `output` may not exceed `context`. Unknown keys
/// are rejected so a typo cannot silently drop a limit.
fn resolve_model_limit(
    provider_id: &str,
    model_id: &str,
    limit: &ModelLimitFile,
) -> anyhow::Result<hya_provider::ModelLimitOverride> {
    for (key, value) in [("context", limit.context), ("output", limit.output)] {
        if value == Some(0) {
            anyhow::bail!(
                "provider {provider_id} model {model_id} limit.{key} must be a positive integer"
            );
        }
    }
    let resolved = hya_provider::ModelLimitOverride {
        context: limit.context.unwrap_or_default(),
        output: limit.output.unwrap_or_default(),
    };
    if resolved.context > 0 && resolved.output > resolved.context {
        anyhow::bail!(
            "provider {provider_id} model {model_id} limit.output {} exceeds limit.context {}",
            resolved.output,
            resolved.context
        );
    }
    Ok(resolved)
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedProvider {
    pub(super) id: String,
    /// Kind label as written.
    pub(super) kind: String,
    pub(super) base_url: String,
    pub(super) api_key: Option<String>,
    pub(super) models: Vec<ParsedModel>,
    pub(super) retry: hya_provider::RetryConfig,
    pub(super) prompt_cache: Option<bool>,
}
/// Resolved optional authentication material for one configured provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderCredential {
    /// Hya-owned bearer or API-key material, when configured.
    pub(super) token: Option<String>,
    /// Account id stored with an OAuth credential (ChatGPT Codex), if known.
    pub(super) account_id: Option<String>,
    /// OAuth type of a saved OAuth credential with a token; its registered
    /// flow re-resolves the bearer on each stream.
    pub(super) oauth_type: Option<String>,
}

impl ProviderCredential {
    /// Return the non-secret auth presence used by catalog status projection.
    #[must_use]
    fn auth_presence(&self) -> AuthPresence {
        self.token
            .as_deref()
            .filter(|token| !token.is_empty())
            .map_or(AuthPresence::Unauthenticated, |_| {
                AuthPresence::Credentialed
            })
    }

    /// The credential a route or discovery request sends, when there is a token.
    pub(super) fn route_credential(&self) -> Option<RouteCredential> {
        self.token.clone().map(|token| RouteCredential {
            token,
            account_id: self.account_id.clone(),
        })
    }
}

/// Resolve optional auth for a provider from Hya login token or inline key.
pub(super) fn resolve_provider_credential(
    provider: &ParsedProvider,
) -> anyhow::Result<ProviderCredential> {
    if let Some(cred) = crate::auth::load_credential(&provider.id)? {
        let token = cred.access_token().trim();
        let oauth = cred.oauth();
        return Ok(ProviderCredential {
            token: (!token.is_empty()).then(|| token.to_string()),
            account_id: oauth.and_then(|o| o.account_id.clone()),
            oauth_type: oauth
                .filter(|_| !token.is_empty())
                .map(|oauth| oauth.oauth_type.clone()),
        });
    }
    let token = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    Ok(ProviderCredential {
        token,
        account_id: None,
        oauth_type: None,
    })
}

#[cfg(test)]
pub(super) fn resolve_provider_credential_with(
    login_token: Option<&str>,
    inline_api_key: Option<&str>,
) -> Option<ProviderCredential> {
    let token = login_token
        .or(inline_api_key)
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    Some(ProviderCredential {
        token: Some(token.to_string()),
        account_id: None,
        oauth_type: None,
    })
}

pub(crate) fn resolve_secret(raw: &str) -> anyhow::Result<String> {
    if let Some(var) = raw.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
        std::env::var(var).with_context(|| format!("apiKey env var {var} is not set"))
    } else if let Some(path) = raw.strip_prefix("{file:").and_then(|s| s.strip_suffix('}')) {
        Ok(std::fs::read_to_string(path)
            .with_context(|| format!("read apiKey file {path}"))?
            .trim()
            .to_string())
    } else {
        Ok(raw.to_string())
    }
}

/// Parse every provider declaration, preserving empty model lists for startup
/// discovery and normalizing explicit ids before route construction.
pub(super) fn resolve_providers(file: &FileConfig) -> anyhow::Result<Vec<ParsedProvider>> {
    resolve_providers_filtered(file, None, true)
}

/// Parse and validate provider declarations. `only` restricts the result to
/// those ids; `resolve_secrets: false` validates without expanding
/// `{env:…}`/`{file:…}` keys (used to validate config edits).
///
/// Kind labels are not resolved here, so the result does not depend on which
/// provider kinds are registered; [`merge_provider_models`] resolves them.
pub(super) fn resolve_providers_filtered(
    file: &FileConfig,
    only: Option<&BTreeSet<String>>,
    resolve_secrets: bool,
) -> anyhow::Result<Vec<ParsedProvider>> {
    let mut out = Vec::new();
    for (id, provider) in &file.providers {
        if only.is_some_and(|only| !only.contains(id)) {
            continue;
        }
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for model in &provider.models {
            let (raw_id, name, reasoning_field, limit, modalities, entry_kind) = match model {
                ModelConfig::Id(id) => (id.as_str(), None, None, None, None, None),
                ModelConfig::Detailed(model) => (
                    model.id.as_str(),
                    model.name.as_deref(),
                    model.reasoning.as_ref(),
                    model.limit.as_ref(),
                    model.modalities.as_ref(),
                    model.kind.as_deref(),
                ),
            };
            let reasoning_off = matches!(reasoning_field, Some(ReasoningField::Flag(false)));
            let reasoning_declared = match reasoning_field {
                Some(ReasoningField::Flag(flag)) => Some(*flag),
                Some(ReasoningField::Detailed(_)) => Some(true),
                None => None,
            };
            let reasoning = match reasoning_field {
                Some(ReasoningField::Detailed(config)) => Some(config),
                _ => None,
            };
            let model_id = raw_id.trim();
            if model_id.is_empty() || model_id.chars().any(char::is_control) {
                continue;
            }
            if id == "hya" && model_id == "offline" {
                anyhow::bail!("provider hya cannot claim reserved model hya/offline");
            }
            if !seen.insert(model_id.to_string()) {
                continue;
            }
            let effective_kind = entry_kind
                .unwrap_or(provider.kind.as_str())
                .trim()
                .to_string();
            let variants_configured =
                reasoning_off || reasoning.is_some_and(|config| config.variants.is_some());
            let configured_variants = reasoning
                .and_then(|config| config.variants.clone())
                .unwrap_or_default();
            let efforts = configured_variants
                .iter()
                .map(|variant| {
                    ReasoningEffort::parse(variant).with_context(|| {
                        format!(
                            "provider {id} model {model_id} has unknown reasoning variant {variant}"
                        )
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let variants = efforts
                .iter()
                .map(|effort| effort.as_str().to_string())
                .collect::<Vec<_>>();
            let explicit_default = reasoning
                .and_then(|config| config.default.as_deref())
                .map(|value| {
                    ReasoningEffort::parse(value).with_context(|| {
                        format!(
                            "provider {id} model {model_id} has unknown reasoning default {value}"
                        )
                    })
                })
                .transpose()?;
            // Without configured variants the menu comes from the kind, and
            // `merge_provider_models` checks the default against it.
            if let Some(default) = explicit_default
                && variants_configured
                && default != ReasoningEffort::Off
                && !efforts.contains(&default)
            {
                anyhow::bail!(
                    "provider {id} model {model_id} reasoning default {} is not advertised",
                    default.as_str()
                );
            }
            let limit = limit
                .map(|limit| resolve_model_limit(id, model_id, limit))
                .transpose()?;
            let image_input = modalities
                .and_then(|modalities| modalities.input.as_ref())
                .map(|names| {
                    names
                        .iter()
                        .any(|name| name.trim().eq_ignore_ascii_case("image"))
                });
            let display_name = name
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
                .map(str::to_string);
            models.push(ParsedModel {
                id: model_id.to_string(),
                kind: effective_kind,
                display_name,
                reasoning_default: explicit_default,
                reasoning_variants: variants,
                variants_configured,
                limit,
                image_input,
                reasoning_declared,
            });
        }
        let api_key = if resolve_secrets {
            provider
                .api_key
                .as_deref()
                .map(resolve_secret)
                .transpose()?
        } else {
            provider.api_key.clone()
        };
        out.push(ParsedProvider {
            id: id.clone(),
            kind: provider.kind.trim().to_string(),
            base_url: provider.base_url.clone(),
            api_key,
            models,
            retry: resolve_provider_retry(file.provider_retry.as_ref(), provider.retry.as_ref()),
            prompt_cache: provider.prompt_cache,
        });
    }
    Ok(out)
}

/// Map OAuth failures onto provider errors exactly as the bearer-resolver
/// route already does: re-login/entitlement issues are human actions and stay
/// non-retryable `ProviderError::AuthExpired` for router/engine failover.
fn map_oauth_error(err: crate::oauth::OAuthError) -> hya_provider::ProviderError {
    use hya_provider::ProviderError;

    match err {
        crate::oauth::OAuthError::NeedsLogin {
            provider,
            oauth_type,
            reason,
        } => ProviderError::AuthExpired {
            provider: provider.clone(),
            hint: format!(
                "{reason}. Re-login: hya oauth login --provider {provider} --type {oauth_type}"
            ),
        },
        crate::oauth::OAuthError::Entitlement { provider, detail } => ProviderError::AuthExpired {
            provider,
            hint: format!(
                "not entitled for API access ({detail}); API key path or subscription upgrade required"
            ),
        },
        other => ProviderError::Http(other.to_string()),
    }
}

pub(super) fn status_auth(credential: &ProviderCredential) -> ProviderAuthState {
    match credential.auth_presence() {
        AuthPresence::Credentialed => ProviderAuthState::Credentialed,
        AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
    }
}

/// One model of a provider's effective list: the model cache row merged
/// with the provider's config `models:` entry of the same id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EffectiveModel {
    pub(super) id: String,
    /// Canonical label of the effective kind: the entry's `kind` override,
    /// else the provider's.
    pub(super) kind: String,
    pub(super) display_name: Option<String>,
    pub(super) reasoning_variants: Vec<String>,
    pub(super) reasoning_default: Option<ReasoningEffort>,
    pub(super) limit: hya_provider::ModelLimitOverride,
    /// Declared image input (config `modalities.input`); `None` unknown.
    pub(super) image_input: Option<bool>,
    /// Declared reasoning support (config `reasoning`, else remote-list
    /// effort metadata); `None` unknown.
    pub(super) reasoning_declared: Option<bool>,
    pub(super) source: ModelCatalogSource,
}

/// Protocol registered for kind `label` of `provider_id` (`model_id` for an
/// entry's `kind` override).
pub(super) fn kind_protocol(
    kinds: &ProviderKinds,
    provider_id: &str,
    model_id: Option<&str>,
    label: &str,
) -> anyhow::Result<Arc<dyn Protocol>> {
    kinds.protocol(label).cloned().ok_or_else(|| {
        let owner = model_id.map_or_else(
            || format!("provider {provider_id}"),
            |model_id| format!("provider {provider_id} model {model_id}"),
        );
        anyhow::anyhow!(
            "{owner} has unknown kind {label} (expected one of {})",
            kinds.labels().collect::<Vec<_>>().join(", ")
        )
    })
}

/// What a provider routes with once its kinds and OAuth type are registered.
pub(super) struct ProviderSupport {
    /// Protocol of the provider's own `kind`.
    pub(super) protocol: Arc<dyn Protocol>,
    /// Flow that refreshes the provider's saved OAuth credential.
    pub(super) auth_flow: Option<Arc<dyn ProviderAuthFlow>>,
}

/// Look up `provider`'s kind, every model `kind:` override, and its saved
/// OAuth type in `kinds`; the error says which one no enabled bundle provides.
pub(super) fn provider_support(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    kinds: &ProviderKinds,
) -> Result<ProviderSupport, String> {
    let missing_kind = |owner: String, label: &str| {
        format!(
            "{owner}: kind `{label}` is not provided by any enabled bundle (enabled kinds: {})",
            kinds.labels().collect::<Vec<_>>().join(", ")
        )
    };
    let protocol = kinds
        .protocol(&provider.kind)
        .cloned()
        .ok_or_else(|| missing_kind(format!("provider {}", provider.id), &provider.kind))?;
    if let Some(model) = provider
        .models
        .iter()
        .find(|model| kinds.protocol(&model.kind).is_none())
    {
        return Err(missing_kind(
            format!("provider {} model {}", provider.id, model.id),
            &model.kind,
        ));
    }
    let auth_flow = match &credential.oauth_type {
        Some(oauth_type) => Some(kinds.auth_flow(oauth_type).cloned().ok_or_else(|| {
            format!(
                "provider {}: OAuth type `{oauth_type}` of its saved credential is not provided by any enabled bundle",
                provider.id
            )
        })?),
        None => None,
    };
    Ok(ProviderSupport {
        protocol,
        auth_flow,
    })
}

fn variant_labels(protocol: &dyn Protocol) -> Vec<String> {
    protocol
        .reasoning_variant_labels()
        .iter()
        .map(|label| (*label).to_string())
        .collect()
}

/// Merge cached remote rows with config entries, per model id.
///
/// Remote-only rows keep their cached metadata; config-only entries keep
/// today's configured semantics; a model in both is an override whose config
/// fields win field by field and whose unset config fields fall back to the
/// cached metadata. Cached rows come first (remote order), then config-only
/// entries (config order). Kind labels resolve against `kinds`: an entry
/// without configured variants gets its kind's menu, and its configured
/// default must be on it.
pub(super) fn merge_provider_models(
    provider: &ParsedProvider,
    cached: &[crate::model_cache::CachedModel],
    kinds: &ProviderKinds,
) -> anyhow::Result<Vec<EffectiveModel>> {
    let provider_protocol = kind_protocol(kinds, &provider.id, None, &provider.kind)?;
    let mut configured = Vec::with_capacity(provider.models.len());
    for model in &provider.models {
        let protocol = kind_protocol(kinds, &provider.id, Some(&model.id), &model.kind)?;
        let mut model = model.clone();
        model.kind = protocol.kind().to_string();
        if !model.variants_configured {
            model.reasoning_variants = variant_labels(protocol.as_ref());
            if let Some(default) = model.reasoning_default
                && default != ReasoningEffort::Off
                && !model
                    .reasoning_variants
                    .iter()
                    .any(|v| v == default.as_str())
            {
                anyhow::bail!(
                    "provider {} model {} reasoning default {} is not advertised",
                    provider.id,
                    model.id,
                    default.as_str()
                );
            }
        }
        configured.push(model);
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in cached {
        let id = row.id.trim();
        if id.is_empty() || (provider.id == "hya" && id == "offline") || !seen.insert(id) {
            continue;
        }
        out.push(match configured.iter().find(|model| model.id == id) {
            Some(config) => override_model(config, row),
            None => remote_model(provider_protocol.as_ref(), row),
        });
    }
    for model in &configured {
        if seen.insert(model.id.as_str()) {
            out.push(EffectiveModel {
                id: model.id.clone(),
                kind: model.kind.clone(),
                display_name: model.display_name.clone(),
                reasoning_variants: model.reasoning_variants.clone(),
                reasoning_default: model.reasoning_default,
                limit: model.limit.clone().unwrap_or_default(),
                image_input: model.image_input,
                reasoning_declared: model.reasoning_declared,
                source: ModelCatalogSource::Configured,
            });
        }
    }
    Ok(out)
}

fn cached_variants(fallback: &[String], row: &crate::model_cache::CachedModel) -> Vec<String> {
    let variants = row
        .reasoning_variants
        .iter()
        .filter_map(|variant| ReasoningEffort::parse(variant))
        .map(|effort| effort.as_str().to_string())
        .collect::<Vec<_>>();
    if variants.is_empty() {
        fallback.to_vec()
    } else {
        variants
    }
}

/// Reasoning support a cached remote row declares: effort metadata means
/// `Some(true)`; a row without any is unknown.
fn cached_reasoning_declared(row: &crate::model_cache::CachedModel) -> Option<bool> {
    let declared = row
        .reasoning_variants
        .iter()
        .any(|variant| ReasoningEffort::parse(variant).is_some())
        || row.reasoning_default.is_some();
    declared.then_some(true)
}

fn remote_model(protocol: &dyn Protocol, row: &crate::model_cache::CachedModel) -> EffectiveModel {
    EffectiveModel {
        id: row.id.trim().to_string(),
        kind: protocol.kind().to_string(),
        display_name: row.display_name.clone(),
        reasoning_variants: cached_variants(&variant_labels(protocol), row),
        // Remote metadata describes capabilities, not a user selection.
        reasoning_default: None,
        limit: hya_provider::ModelLimitOverride {
            context: row.context_limit,
            output: row.output_limit,
        },
        image_input: None,
        reasoning_declared: cached_reasoning_declared(row),
        source: ModelCatalogSource::Discovered,
    }
}

fn override_model(config: &ParsedModel, row: &crate::model_cache::CachedModel) -> EffectiveModel {
    let (reasoning_variants, reasoning_default) = if config.variants_configured {
        (config.reasoning_variants.clone(), config.reasoning_default)
    } else {
        // Cached variants refine the menu; the explicit configured default
        // (if any) still decides the request effort.
        (
            cached_variants(&config.reasoning_variants, row),
            config.reasoning_default,
        )
    };
    let configured_limit = config.limit.clone().unwrap_or_default();
    let mut limit = hya_provider::ModelLimitOverride {
        context: if configured_limit.context > 0 {
            configured_limit.context
        } else {
            row.context_limit
        },
        output: if configured_limit.output > 0 {
            configured_limit.output
        } else {
            row.output_limit
        },
    };
    // A cached value never contradicts a configured one: drop the cached side.
    if limit.context > 0 && limit.output > limit.context {
        if configured_limit.output == 0 {
            limit.output = 0;
        } else {
            limit.context = 0;
        }
    }
    EffectiveModel {
        id: config.id.clone(),
        kind: config.kind.clone(),
        display_name: config
            .display_name
            .clone()
            .or_else(|| row.display_name.clone()),
        reasoning_variants,
        reasoning_default,
        limit,
        image_input: config.image_input,
        reasoning_declared: config
            .reasoning_declared
            .or_else(|| cached_reasoning_declared(row)),
        source: ModelCatalogSource::Overridden,
    }
}

/// Build one provider's HTTP route over its effective model list.
pub(super) fn route_for_models(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    protocol: Arc<dyn Protocol>,
    auth_flow: Option<&Arc<dyn ProviderAuthFlow>>,
    models: &[EffectiveModel],
) -> anyhow::Result<HttpProvider> {
    let prompt_cache = provider
        .prompt_cache
        .unwrap_or_else(|| protocol.supports_prompt_cache());
    let mut route = HttpProvider::new(
        provider.id.clone(),
        protocol,
        &provider.base_url,
        credential.route_credential(),
        models.iter().map(|model| model.id.clone()),
    )?
    .with_prompt_cache(prompt_cache)
    .with_catalog_source(ModelCatalogSource::Configured)
    .with_retry(provider.retry)
    .with_model_sources(models.iter().map(|model| (model.id.clone(), model.source)))
    .with_model_reasoning_variants(
        models
            .iter()
            .map(|model| (model.id.clone(), model.reasoning_variants.clone())),
    )
    .with_model_reasoning_defaults(
        models
            .iter()
            .map(|model| (model.id.clone(), model.reasoning_default)),
    )
    .with_model_limits(
        models
            .iter()
            .filter(|model| model.limit.context > 0 || model.limit.output > 0)
            .map(|model| (model.id.clone(), model.limit.clone())),
    )
    .with_model_display_names(models.iter().filter_map(|model| {
        model
            .display_name
            .clone()
            .map(|name| (model.id.clone(), name))
    }))
    .with_model_image_input(
        models
            .iter()
            .filter_map(|model| model.image_input.map(|image| (model.id.clone(), image))),
    )
    .with_model_reasoning_declared(models.iter().filter_map(|model| {
        model
            .reasoning_declared
            .map(|declared| (model.id.clone(), declared))
    }));
    if let Some(flow) = auth_flow {
        let resolver_id = provider.id.clone();
        let refresher_id = provider.id.clone();
        let resolver_flow = Arc::clone(flow);
        let refresher_flow = Arc::clone(flow);
        let resolver: BearerResolver = Arc::new(move || {
            crate::oauth::ensure_access_token(&resolver_id, resolver_flow.as_ref())
                .map_err(map_oauth_error)
        });
        route = route.with_bearer_resolver(resolver);
        let refresher: AuthRefresher = Arc::new(move |stale_token: &str| {
            crate::oauth::force_refresh_access_token(
                &refresher_id,
                stale_token,
                refresher_flow.as_ref(),
            )
            .map(|_fresh| ())
            .map_err(map_oauth_error)
        });
        route = route.with_auth_refresher(refresher);
    }
    Ok(route)
}

/// Validate an `http(s)://host…` provider base URL without credentials.
///
/// # Errors
/// Returns a user-facing message naming what is wrong.
pub fn validate_base_url(base_url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|error| format!("invalid base URL `{base_url}`: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!(
            "invalid base URL `{base_url}`: use http:// or https:// with a host"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "invalid base URL: credentials in the URL are not allowed (save an API key instead)"
                .to_string(),
        );
    }
    Ok(())
}
