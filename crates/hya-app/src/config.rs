//! Build a provider router from hya's own config (`~/.config/hya/config.yaml`).
//!
//! Reads YAML: each entry under `providers.<id>` maps to one `HttpProvider`
//! (route chosen by `kind`), and the union of `models` becomes the set hya can
//! address. API keys come from `~/.config/hya/auth/<id>.yaml` (via `hya login`)
//! or an inline `api_key` in the provider block. `kind: grok-build` always uses
//! CLI chat-proxy session headers with that configured bearer token (self-contained
//! config — it does not read `~/.grok/auth.json`). `kind: openai-codex` targets the
//! ChatGPT Codex backend. OAuth credentials live under `~/.config/hya/auth/` and
//! are auto-refreshed when near expiry. No resolved live model rows → offline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_core::{
    CategoryEntry, CategoryRegistry, CompactionConfig, SubagentLimits, TokenAccountingMode,
};
use hya_mcp::McpServerConfig;
use hya_plugin::config::PluginEntry;
use hya_provider::{
    AuthPresence, AuthRefresher, BearerResolver, CatalogAuth, CatalogDiscoveryRequest,
    CatalogFailure, HttpProvider, ModelCatalogSource, ProviderAuthState, ProviderCatalogResult,
    ProviderCatalogSnapshot, ProviderCatalogSource, ProviderCatalogState, ProviderDiscoveryOutcome,
    ProviderKind, ProviderModel, ProviderRouter, ReasoningEffort, discover_models,
    resolve_default_reasoning,
};
use hya_tool::{
    InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionTarget, WebSearchConfig,
};
use serde::Deserialize;
use serde_norway::{Mapping, Value};

/// Fully loaded Hya config: provider routes, one immutable catalog snapshot,
/// and derived runtime knobs.
pub struct ResolvedConfig {
    /// Ordered HTTP provider routes built from final catalog rows.
    pub router: ProviderRouter,
    /// Shared immutable model/provider startup snapshot.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Row-backed default model id for new sessions.
    pub default_model: String,
    /// Named MCP server configs from `mcp:`.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Named plugin entries from `plugins:` (before manifest merge).
    pub plugins: BTreeMap<String, PluginEntry>,
    /// Preferred primary agent id when workdir does not select one.
    pub default_agent: Option<String>,
    /// Subagent concurrency and spawn limits from config.
    pub subagents: SubagentLimits,
    /// Logical model categories → ordered concrete `provider/model` candidates.
    pub categories: CategoryRegistry,
    /// Compiled tool permission policy (`permission:` block).
    pub permission: InvocationPolicy,
    /// Web-search plane configuration.
    pub websearch: WebSearchConfig,
    /// Providers that still need background discovery refresh after startup.
    pub pending_discovery: Vec<PendingCatalogDiscovery>,
}

/// One provider deferred to background discovery refresh (no cached rows,
/// or a discovery-only provider with an empty `models:` list).
#[derive(Clone, Debug)]
pub struct PendingCatalogDiscovery {
    /// Declared provider id.
    pub provider_id: String,
    /// Protocol kind for discovery adapters.
    pub kind: ProviderKind,
    /// Catalog base URL from config.
    pub base_url: String,
    pub(crate) credential: ProviderCredential,
    pub(crate) provider: ParsedProvider,
}

/// Top-level shape of `~/.config/hya/config.yaml`.
#[derive(Debug, Deserialize)]
struct FileConfig {
    /// Model used when neither `--model` nor `HYA_MODEL` is set.
    #[serde(default)]
    default_model: Option<String>,
    /// Agent selected by default when a workdir does not specify one. Falls back to `build`.
    #[serde(default)]
    default_agent: Option<String>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    mcp: BTreeMap<String, McpServerConfig>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginEntry>,
    #[serde(default)]
    tools: Option<ToolsConfig>,
    /// Bounded nested/parallel subagent caps. Absent → defaults; per-field env
    /// overrides (`HYA_SUBAGENT_*`) win over file values.
    #[serde(default)]
    subagents: Option<SubagentLimitsFile>,
    /// Logical model categories: each maps a name (e.g. `deep`) to an ordered
    /// list of concrete `provider/model` refs (first = preferred, rest =
    /// failover). Absent → no categories (agents fall back to their own model).
    #[serde(default)]
    categories: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    permission: Option<PermissionConfig>,
    /// Compaction thresholds and token-accounting mode. Absent → engine
    /// defaults; per-field `HYA_COMPACTION_*` / `HYA_TOKEN_ACCOUNTING` env
    /// overrides win over file values.
    #[serde(default)]
    pub(crate) compaction: Option<CompactionFile>,
    /// Goal-mode settings (`goal:` block). Absent → the worker's current model
    /// judges; the `--evaluator-model` CLI flag outranks this value.
    #[serde(default)]
    goal: Option<GoalFile>,
    /// Default replay budget for every provider route. Per-provider `retry:`
    /// blocks override individual fields; `HYA_PROVIDER_RETRY_*` env vars win
    /// over both. Absent → [`hya_provider::RetryConfig`] defaults.
    #[serde(default)]
    provider_retry: Option<ProviderRetryFile>,
}

#[derive(Debug, Deserialize)]
struct ToolsConfig {
    #[serde(default)]
    websearch: WebSearchConfig,
}

#[derive(Debug, Default, Deserialize)]
struct PermissionConfig {
    /// Policy model (`allow` / `default` / `strict` / `danger`).
    /// Accepts the common alias `mode` so `permission.mode: allow` works.
    #[serde(default, alias = "mode")]
    model: PermissionModel,
    #[serde(default)]
    rules: Vec<PermissionRuleConfig>,
}

#[derive(Debug, Deserialize)]
struct PermissionRuleConfig {
    target: PermissionTarget,
    selector: String,
    permission: PermissionModeConfig,
}

/// Rule effect. Accepts both lowercase (`allow`) and PascalCase (`Allow`) so
/// config stays consistent with `permission.model` casing.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PermissionModeConfig {
    #[serde(alias = "Allow")]
    Allow,
    #[serde(alias = "Deny")]
    Deny,
    #[serde(alias = "Ask")]
    Ask,
}

impl From<PermissionModeConfig> for Mode {
    fn from(permission: PermissionModeConfig) -> Self {
        match permission {
            PermissionModeConfig::Allow => Self::Allow,
            PermissionModeConfig::Deny => Self::Deny,
            PermissionModeConfig::Ask => Self::Ask,
        }
    }
}

/// File shape of the `subagents:` block. Every field is optional so a partial
/// block keeps the [`SubagentLimits`] default for the fields it omits.
/// Recursion depth is deliberately absent: it is the hardcoded engine
/// constant `MAX_SUBAGENT_DEPTH` (ADR-0015), not a config knob.
#[derive(Debug, Default, Deserialize)]
struct SubagentLimitsFile {
    #[serde(default)]
    max_concurrency: Option<usize>,
    #[serde(default)]
    per_run_budget: Option<u64>,
    /// Per-team resident turn budget (ADR-0002); a runaway re-wake trips it.
    #[serde(default)]
    per_team_turn_budget: Option<u64>,
    /// Per-team mail message budget (ADR-0002); a message loop trips it.
    #[serde(default)]
    per_team_message_budget: Option<u64>,
}

/// `compaction:` block of `~/.config/hya/config.yaml`.
///
/// Absent fields keep the engine's [`CompactionConfig`] default; per-field
/// `HYA_COMPACTION_*` and `HYA_TOKEN_ACCOUNTING` env overrides win over these.
#[derive(Debug, Deserialize)]
pub(crate) struct CompactionFile {
    #[serde(default)]
    token_threshold: Option<usize>,
    #[serde(default)]
    keep_recent: Option<usize>,
    #[serde(default)]
    context_fraction: Option<f32>,
    #[serde(default)]
    reserve_tokens: Option<usize>,
    #[serde(default)]
    summary_max_tokens: Option<u32>,
    /// `auto`, `provider`, or `estimate`; anything else is ignored.
    #[serde(default)]
    token_accounting: Option<String>,
    /// oh-my-pi `compaction.methodOrder` names (`shake`, `remote`,
    /// `snapcompact`, `handoff`, `soft`); a partial list is completed with the
    /// unmentioned methods in default order, an unknown name ignores the field.
    #[serde(default)]
    method_order: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfig {
    kind: ProviderKindConfig,
    base_url: String,
    /// Literal, `{env:VAR}`, or `{file:path}`. Optional — a token saved via
    /// `hya login` (`~/.config/hya/auth/<id>.yaml`) takes precedence.
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<ModelConfig>,
    /// Per-provider overrides on top of the global `provider_retry:` block.
    /// Unset fields inherit the global value.
    #[serde(default)]
    retry: Option<ProviderRetryFile>,
}

/// File shape of a retry policy block (`provider_retry:` global default or a
/// provider's `retry:` override). Every field is optional; unset fields fall
/// back to the enclosing layer and finally the engine defaults.
#[derive(Debug, Default, Deserialize)]
struct ProviderRetryFile {
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelConfig {
    Id(String),
    Detailed(Box<DetailedModelConfig>),
}

#[derive(Debug, Deserialize)]
struct DetailedModelConfig {
    id: String,
    /// Display name shown by pickers and the Provider View; overrides the
    /// remote list's name.
    #[serde(default)]
    name: Option<String>,
    /// `true`/`false` switch or a `{ default?, variants? }` mapping.
    #[serde(default)]
    reasoning: Option<ReasoningField>,
    /// `{ context?, output? }` token limits. Kept as a raw value so
    /// `resolve_model_limit` can name the provider, model, and field in errors
    /// instead of failing the untagged `ModelConfig` match.
    #[serde(default)]
    limit: Option<Value>,
    /// `{ input: [text, image, ...] }`: declares whether the model accepts
    /// image input (prompt attachments). Raw for the same error reporting
    /// as `limit`.
    #[serde(default)]
    modalities: Option<Value>,
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

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProviderKindConfig {
    #[serde(
        rename = "openai",
        alias = "openai-compatible",
        alias = "openai-completion"
    )]
    Openai,
    #[serde(rename = "openai-response")]
    OpenaiResponse,
    #[serde(rename = "openai-codex")]
    OpenaiCodex,
    #[serde(rename = "grok-build")]
    GrokBuild,
    Anthropic,
    Google,
}

impl From<ProviderKindConfig> for ProviderKind {
    fn from(kind: ProviderKindConfig) -> Self {
        match kind {
            ProviderKindConfig::Openai => Self::OpenAiCompatible,
            ProviderKindConfig::OpenaiResponse => Self::OpenAiResponse,
            ProviderKindConfig::OpenaiCodex => Self::OpenAiCodex,
            ProviderKindConfig::GrokBuild => Self::GrokBuild,
            ProviderKindConfig::Anthropic => Self::Anthropic,
            ProviderKindConfig::Google => Self::Google,
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedModel {
    id: String,
    /// Configured display name (`name`).
    display_name: Option<String>,
    /// Effort menu from config (`reasoning.variants`, `reasoning: false` →
    /// empty), else the provider-kind fallback menu.
    reasoning_variants: Vec<String>,
    /// Whether config set the menu itself (`reasoning.variants` or
    /// `reasoning: false`); otherwise remote-list variants win over the
    /// kind fallback when the model is also in the model cache.
    variants_configured: bool,
    /// Explicit default, else the highest effort in `reasoning_variants`.
    reasoning_default: Option<ReasoningEffort>,
    /// Explicit `reasoning.default` only.
    explicit_default: Option<ReasoningEffort>,
    /// Configured token limits (`0` in a field means unspecified).
    limit: Option<hya_provider::ModelLimitOverride>,
    /// Image input from `modalities.input`; `None` when not declared.
    image_input: Option<bool>,
    /// Reasoning support the config declares (`reasoning: true|false` or a
    /// `reasoning:` mapping); `None` when the entry has no `reasoning`.
    reasoning_declared: Option<bool>,
}

/// Validate one model `modalities` block: a mapping whose optional `input`
/// (and `output`) is a list of modality names. Returns whether `input`
/// contains `image`, or `None` when `input` is absent.
fn resolve_model_modalities(
    provider_id: &str,
    model_id: &str,
    modalities: &Value,
) -> anyhow::Result<Option<bool>> {
    let Value::Mapping(fields) = modalities else {
        anyhow::bail!("provider {provider_id} model {model_id} modalities must be a mapping");
    };
    let mut image_input = None;
    for (key, value) in fields {
        let key = key.as_str().unwrap_or_default();
        if !matches!(key, "input" | "output") {
            anyhow::bail!(
                "provider {provider_id} model {model_id} has unknown modalities key {key} (expected input or output)"
            );
        }
        let names = value
            .as_sequence()
            .and_then(|items| items.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
            .with_context(|| {
                format!(
                    "provider {provider_id} model {model_id} modalities.{key} must be a list of names (for example [text, image])"
                )
            })?;
        if key == "input" {
            image_input = Some(
                names
                    .iter()
                    .any(|name| name.trim().eq_ignore_ascii_case("image")),
            );
        }
    }
    Ok(image_input)
}

/// Validate one object-form model `limit` block.
///
/// Accepts a mapping with optional `context` and `output` positive `u32`
/// integers; when both are set `output` may not exceed `context`. Unknown keys
/// are rejected so a typo cannot silently drop a limit.
fn resolve_model_limit(
    provider_id: &str,
    model_id: &str,
    limit: &Value,
) -> anyhow::Result<hya_provider::ModelLimitOverride> {
    let Value::Mapping(fields) = limit else {
        anyhow::bail!("provider {provider_id} model {model_id} limit must be a mapping");
    };
    let mut resolved = hya_provider::ModelLimitOverride::default();
    for (key, value) in fields {
        let key = key.as_str().unwrap_or_default();
        let slot = match key {
            "context" => &mut resolved.context,
            "output" => &mut resolved.output,
            _ => anyhow::bail!(
                "provider {provider_id} model {model_id} has unknown limit key {key} (expected context or output)"
            ),
        };
        *slot = value
            .as_u64()
            .and_then(|tokens| u32::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
            .with_context(|| {
                format!(
                    "provider {provider_id} model {model_id} limit.{key} must be a positive integer no larger than {}",
                    u32::MAX
                )
            })?;
    }
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
    id: String,
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    models: Vec<ParsedModel>,
    retry: hya_provider::RetryConfig,
}

/// Resolved optional authentication material for one configured provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderCredential {
    /// Hya-owned bearer or API-key material, when configured.
    token: Option<String>,
    /// When true, attach Grok Build CLI chat-proxy session headers.
    use_grok_session: bool,
    /// When true, attach ChatGPT Codex session headers.
    use_codex_session: bool,
    /// ChatGPT account id for Codex OAuth (if known).
    account_id: Option<String>,
    /// When true, re-resolve the bearer via OAuth refresh on each stream.
    use_oauth_refresh: bool,
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
}

/// Resolve optional auth for a provider from Hya login token or inline key.
fn resolve_provider_credential(provider: &ParsedProvider) -> ProviderCredential {
    if let Some(cred) = crate::auth::load_credential(&provider.id) {
        let token = cred.access_token().trim();
        let oauth = cred.oauth();
        return ProviderCredential {
            token: (!token.is_empty()).then(|| token.to_string()),
            use_grok_session: provider.kind == ProviderKind::GrokBuild,
            use_codex_session: provider.kind == ProviderKind::OpenAiCodex,
            account_id: oauth.and_then(|o| o.account_id.clone()),
            use_oauth_refresh: oauth.is_some() && !token.is_empty(),
        };
    }
    let token = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    ProviderCredential {
        token,
        use_grok_session: provider.kind == ProviderKind::GrokBuild,
        use_codex_session: provider.kind == ProviderKind::OpenAiCodex,
        account_id: None,
        use_oauth_refresh: false,
    }
}

#[cfg(test)]
fn resolve_provider_credential_with(
    kind: ProviderKind,
    login_token: Option<&str>,
    inline_api_key: Option<&str>,
) -> Option<ProviderCredential> {
    let token = login_token
        .or(inline_api_key)
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    Some(ProviderCredential {
        token: Some(token.to_string()),
        use_grok_session: kind == ProviderKind::GrokBuild,
        use_codex_session: kind == ProviderKind::OpenAiCodex,
        account_id: None,
        use_oauth_refresh: false,
    })
}

const DEFAULT_CONFIG_YAML: &str = "default_model: hya/offline\nproviders: {}\nmcp: {}\nplugins: {}\npermission:\n  model: default\n  rules: []\n";

/// Result of creating a brand-new default `config.yaml` on first run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConfig {
    /// Absolute (or resolved) path of the file that was written.
    pub path: PathBuf,
}

/// Counts from importing Compat/OpenCode config into hya's `config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatImportSummary {
    /// Source Compat config path that was read.
    pub compat_path: PathBuf,
    /// Destination hya config path that was written or updated.
    pub config_path: PathBuf,
    /// Number of provider blocks imported.
    pub providers: usize,
    /// Number of model entries imported across providers.
    pub models: usize,
    /// Local MCP servers successfully imported.
    pub mcp_servers: usize,
    /// MCP servers skipped (unsupported transport, missing fields, …).
    pub mcp_skipped: usize,
}

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir).join("hya/config.yaml");
        if path.exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/hya/config.yaml");
    path.exists().then_some(path)
}

/// Configuration file currently used by Hya, or its creation path when absent.
/// Unlike the creation-only path, this preserves the existing HOME fallback
/// when an XDG configuration directory has no Hya configuration file yet.
#[must_use]
pub fn active_config_path() -> PathBuf {
    config_path().unwrap_or_else(expected_config_path)
}

/// Where hya expects its config file, whether or not it currently exists.
///
/// Unlike `config_path` (which only returns a path that exists), this always
/// yields the location a user should create — preferring
/// `$XDG_CONFIG_HOME/hya/config.yaml`, then `$HOME/.config/hya/config.yaml`,
/// and finally the conventional `~/.config/...` spelling when neither env var
/// is set. Used to tell users where to put their config on the offline path.
#[must_use]
pub fn expected_config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("hya/config.yaml");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/hya/config.yaml");
    }
    PathBuf::from("~/.config/hya/config.yaml")
}

/// Upsert a non-secret OAuth provider route into `config.yaml`.
///
/// Creates the file (and parents) when missing. Preserves unrelated top-level
/// keys and the provider's other keys. Does **not** write secrets — tokens
/// live under `auth/<provider>.yaml`.
///
/// Existing user-authored model lists remain untouched. OAuth login creates an
/// empty model field for a new provider so the next startup can discover it;
/// fetched or guessed ids are never persisted here.
pub fn upsert_oauth_provider(
    config_path: &Path,
    provider_id: &str,
    kind: &str,
    base_url: &str,
) -> anyhow::Result<()> {
    upsert_provider_entry(config_path, provider_id, kind, base_url)
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

/// Config `kind` labels a provider declaration may use.
pub const PROVIDER_KIND_LABELS: &[&str] = &[
    "openai",
    "openai-compatible",
    "openai-completion",
    "openai-response",
    "openai-codex",
    "grok-build",
    "anthropic",
    "google",
];

/// Failure of a `config.yaml` edit.
#[derive(Debug, thiserror::Error)]
pub enum ConfigEditError {
    /// The edit would produce an invalid config, or names something absent.
    #[error("{0}")]
    Invalid(String),
    /// The provider or model entry is not in `config.yaml`.
    #[error("{0}")]
    NotFound(String),
    /// Reading, parsing, or writing the file failed.
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// Patch for one model's `models:` entry. Each field is optional: `None`
/// keeps the entry's current value (or keeps it absent). An empty (after
/// trimming) `display_name` removes `name`; a zero limit removes that
/// `limit.*` key (and an empty `limit:` map). `reasoning: Some(_)` writes a
/// boolean `reasoning` (`Some(true)` keeps a detailed `reasoning:` mapping);
/// there is no clear for `reasoning` here: remove the whole entry instead.
/// Keys this writer does not manage are always preserved.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelEntryOverride {
    /// Entry `name` (display name).
    pub display_name: Option<String>,
    /// Entry `limit.context`.
    pub context_limit: Option<u32>,
    /// Entry `limit.output`.
    pub output_limit: Option<u32>,
    /// Entry `reasoning: true|false`.
    pub reasoning: Option<bool>,
}

/// Non-secret facts about one provider declaration in `config.yaml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDeclaration {
    /// Provider id.
    pub id: String,
    /// `kind` label as written.
    pub kind: String,
    /// `base_url` as written.
    pub base_url: String,
    /// Whether the declaration has a non-empty inline `api_key`.
    pub inline_api_key: bool,
}

fn key(name: &str) -> Value {
    Value::String(name.to_string())
}

/// `config.yaml` text (the default document when the file is missing or
/// blank) and its parsed value.
struct ConfigSource {
    raw: String,
    root: Value,
}

fn read_config_source(config_path: &Path) -> anyhow::Result<ConfigSource> {
    let existing = if config_path.exists() {
        std::fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?
    } else {
        String::new()
    };
    let raw = if existing.trim().is_empty() {
        DEFAULT_CONFIG_YAML.to_string()
    } else {
        existing
    };
    let root: Value =
        serde_norway::from_str(&raw).with_context(|| format!("parse {}", config_path.display()))?;
    if root.as_mapping().is_none() {
        anyhow::bail!("config root must be a mapping");
    }
    Ok(ConfigSource { raw, root })
}

/// Read `config.yaml` as a YAML value (the default document when the file is
/// missing or blank).
fn read_config_value(config_path: &Path) -> anyhow::Result<Value> {
    read_config_source(config_path).map(|source| source.root)
}

/// Borrow `providers:` as a mapping, creating it when absent.
fn providers_mapping(root: &mut Value) -> anyhow::Result<&mut Mapping> {
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("config root must be a mapping"))?;
    let slot = map
        .entry(key("providers"))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if slot.is_null() {
        *slot = Value::Mapping(Mapping::new());
    }
    slot.as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("providers must be a mapping"))
}

/// Validate and atomically write an edited config document.
///
/// `target` is the whole new document. The file text is edited minimally
/// ([`crate::config_edit`]): only the entries whose values change are
/// rewritten, so comments, blank lines, key order, quoting, and indentation
/// elsewhere are kept. The edited text must parse back to exactly `target`,
/// otherwise the edit is aborted and the file left untouched. When the file
/// uses YAML the minimal editor does not handle (anchors, aliases, tags,
/// several documents, multi-line flow or quoted values, …) the document is
/// re-rendered whole instead, dropping comments, and a warning is logged.
///
/// The result must parse and its providers must validate (secrets are not
/// expanded). The file is replaced through a temp file in the same directory
/// that inherits the old file's permissions.
fn write_config_value(
    config_path: &Path,
    source: &ConfigSource,
    target: &Value,
) -> Result<(), ConfigEditError> {
    let rendered = match crate::config_edit::minimal_edit(&source.raw, &source.root, target) {
        Ok(text) => {
            let reparsed: Value = serde_norway::from_str(&text).with_context(|| {
                format!(
                    "comment-preserving edit of {} produced invalid YAML; file left untouched",
                    config_path.display()
                )
            })?;
            if &reparsed != target {
                return Err(ConfigEditError::Io(anyhow::anyhow!(
                    "comment-preserving edit of {} did not match the intended change; \
                     file left untouched",
                    config_path.display()
                )));
            }
            text
        }
        Err(crate::config_edit::Unsupported(reason)) => {
            tracing::warn!(
                path = %config_path.display(),
                %reason,
                "config.yaml uses YAML the minimal editor does not handle; \
                 rewriting the whole file (comments and formatting are not kept)"
            );
            serde_norway::to_string(target).context("render updated hya config.yaml")?
        }
    };
    let file = parse_config(&rendered).map_err(|error| {
        ConfigEditError::Invalid(format!("edit would make config.yaml invalid: {error:#}"))
    })?;
    resolve_providers_filtered(&file, None, false).map_err(|error| {
        ConfigEditError::Invalid(format!("edit would make config.yaml invalid: {error:#}"))
    })?;
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("hya config path should have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create config dir {}", parent.display()))?;
    let tmp = parent.join(format!(".config.yaml.tmp-{}", std::process::id()));
    std::fs::write(&tmp, rendered).with_context(|| format!("write {}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(config_path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    if let Err(error) = std::fs::rename(&tmp, config_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigEditError::Io(anyhow::Error::from(error).context(
            format!("rename {} -> {}", tmp.display(), config_path.display()),
        )));
    }
    Ok(())
}

/// Add or update `providers.<id>` with `kind` and `base_url` in
/// `config.yaml`, creating the file when missing. Every other key of the
/// provider (models, api_key, retry, …) and of the file is preserved; a new
/// provider gets `models: []` (discovery). Returns whether the provider was
/// created.
///
/// # Errors
/// [`ConfigEditError::Invalid`] for an unknown kind or an edit that fails
/// validation; [`ConfigEditError::Io`] for read/parse/write failures.
pub fn upsert_provider_entry(
    config_path: &Path,
    provider_id: &str,
    kind: &str,
    base_url: &str,
) -> Result<bool, ConfigEditError> {
    if !PROVIDER_KIND_LABELS.contains(&kind) {
        return Err(ConfigEditError::Invalid(format!(
            "unknown provider kind `{kind}` (expected one of {})",
            PROVIDER_KIND_LABELS.join(", ")
        )));
    }
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let providers = providers_mapping(&mut root)?;
    let created = !providers.contains_key(key(provider_id));
    let entry = providers
        .entry(key(provider_id))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if !entry.is_mapping() {
        *entry = Value::Mapping(Mapping::new());
    }
    let provider = entry
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("provider entry must be a mapping"))?;
    provider.insert(key("kind"), Value::String(kind.to_string()));
    provider.insert(key("base_url"), Value::String(base_url.to_string()));
    if !provider.contains_key(key("models")) {
        provider.insert(key("models"), Value::Sequence(Vec::new()));
    }
    write_config_value(config_path, &source, &root)?;
    Ok(created)
}

fn model_entry_id(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(id) => Some(id.trim()),
        Value::Mapping(map) => map.get(key("id")).and_then(Value::as_str).map(str::trim),
        _ => None,
    }
}

/// Borrow `providers.<id>.models` as a sequence, creating it when absent.
fn provider_models<'a>(
    root: &'a mut Value,
    provider_id: &str,
) -> Result<&'a mut Vec<Value>, ConfigEditError> {
    let providers = providers_mapping(root)?;
    let provider = providers
        .get_mut(key(provider_id))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| {
            ConfigEditError::NotFound(format!("provider not configured: {provider_id}"))
        })?;
    let models = provider
        .entry(key("models"))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    if models.is_null() {
        *models = Value::Sequence(Vec::new());
    }
    models.as_sequence_mut().ok_or_else(|| {
        ConfigEditError::Invalid(format!("providers.{provider_id}.models must be a list"))
    })
}

/// Patch one model's entry in `providers.<provider_id>.models` (see
/// [`ModelEntryOverride`] for the field semantics), adding a bare `- <id>`
/// entry when the model has none. A string entry becomes a mapping when a
/// field is set; an entry left with only `id` is written back as a plain
/// string.
///
/// # Errors
/// [`ConfigEditError::NotFound`] when the provider is not declared;
/// [`ConfigEditError::Invalid`] when the patched file fails validation
/// (e.g. the merged `limit.output` exceeds the merged `limit.context`).
pub fn set_model_entry(
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
    metadata: &ModelEntryOverride,
) -> Result<(), ConfigEditError> {
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let models = provider_models(&mut root, provider_id)?;
    let position = models
        .iter()
        .position(|entry| model_entry_id(entry) == Some(model_id));
    let mut entry = match position.map(|index| models[index].clone()) {
        Some(Value::Mapping(map)) => map,
        _ => {
            let mut map = Mapping::new();
            map.insert(key("id"), Value::String(model_id.to_string()));
            map
        }
    };
    match metadata.display_name.as_deref().map(str::trim) {
        Some("") => {
            entry.remove(key("name"));
        }
        Some(name) => {
            entry.insert(key("name"), Value::String(name.to_string()));
        }
        None => {}
    }
    if metadata.context_limit.is_some() || metadata.output_limit.is_some() {
        let mut limit = match entry.remove(key("limit")) {
            Some(Value::Mapping(limit)) => limit,
            _ => Mapping::new(),
        };
        for (field, value) in [
            ("context", metadata.context_limit),
            ("output", metadata.output_limit),
        ] {
            match value {
                Some(0) => {
                    limit.remove(key(field));
                }
                Some(tokens) => {
                    limit.insert(key(field), Value::Number(u64::from(tokens).into()));
                }
                None => {}
            }
        }
        if !limit.is_empty() {
            entry.insert(key("limit"), Value::Mapping(limit));
        }
    }
    let detailed_reasoning = entry.get(key("reasoning")).is_some_and(Value::is_mapping);
    match metadata.reasoning {
        Some(false) => {
            entry.insert(key("reasoning"), Value::Bool(false));
        }
        Some(true) if !detailed_reasoning => {
            entry.insert(key("reasoning"), Value::Bool(true));
        }
        Some(true) | None => {}
    }
    let value = if entry.len() == 1 && entry.contains_key(key("id")) {
        Value::String(model_id.to_string())
    } else {
        Value::Mapping(entry)
    };
    match position {
        Some(index) => models[index] = value,
        None => models.push(value),
    }
    write_config_value(config_path, &source, &root)
}

/// Remove every `providers.<provider_id>.models` entry with `model_id`.
/// Returns whether an entry existed.
///
/// # Errors
/// [`ConfigEditError::NotFound`] when the provider is not declared.
pub fn remove_model_entry(
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, ConfigEditError> {
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let models = provider_models(&mut root, provider_id)?;
    let before = models.len();
    models.retain(|entry| model_entry_id(entry) != Some(model_id));
    if models.len() == before {
        return Ok(false);
    }
    write_config_value(config_path, &source, &root)?;
    Ok(true)
}

/// Non-secret provider declarations from the active `config.yaml` (empty
/// when there is no config file).
///
/// # Errors
/// Returns read or YAML parse failures.
pub fn provider_declarations() -> anyhow::Result<Vec<ProviderDeclaration>> {
    let Some(path) = config_path() else {
        return Ok(Vec::new());
    };
    let root = read_config_value(&path)?;
    let Some(providers) = root.get("providers").and_then(Value::as_mapping) else {
        return Ok(Vec::new());
    };
    let text = |provider: &Mapping, field: &str| {
        provider
            .get(key(field))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    let mut out = providers
        .iter()
        .filter_map(|(id, provider)| {
            let id = id.as_str()?.to_string();
            let provider = provider.as_mapping()?;
            Some(ProviderDeclaration {
                kind: text(provider, "kind"),
                base_url: text(provider, "base_url"),
                inline_api_key: !text(provider, "api_key").is_empty(),
                id,
            })
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out)
}

/// Create the default hya config file if neither supported config path exists.
///
/// Returns `Ok(Some(...))` only for the first creation. Existing configs are
/// left untouched, including empty files or files without providers.
pub fn ensure_config_file() -> anyhow::Result<Option<CreatedConfig>> {
    if config_path().is_some() {
        return Ok(None);
    }
    let path = expected_config_path();
    ensure_config_file_at(&path).map(|created| created.then_some(CreatedConfig { path }))
}

/// Write the default config YAML at `path` when it does not already exist.
///
/// Returns `Ok(true)` if the file was created, `Ok(false)` if it already existed.
pub fn ensure_config_file_at(path: &Path) -> anyhow::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("hya config path should have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create hya config directory {}", parent.display()))?;
    std::fs::write(path, DEFAULT_CONFIG_YAML)
        .with_context(|| format!("write hya config {}", path.display()))?;
    Ok(true)
}

/// Ensure a default Hya config exists; when `interactive`, print a starter
/// notice. Runtime startup never probes or opens foreign product configs.
pub fn first_run_config_bootstrap(interactive: bool) -> anyhow::Result<()> {
    let Some(created) = ensure_config_file().context("create default hya config")? else {
        return Ok(());
    };
    if interactive {
        eprintln!("hya: created default config at {}", created.path.display());
        eprintln!("hya: edit the starter config to add a provider");
    }
    Ok(())
}

fn resolve_secret(raw: &str) -> anyhow::Result<String> {
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

fn parse_config(yaml: &str) -> anyhow::Result<FileConfig> {
    serde_norway::from_str(yaml).context("parse hya config.yaml")
}

fn resolve_permission(file: &FileConfig) -> anyhow::Result<InvocationPolicy> {
    let model = file
        .permission
        .as_ref()
        .map_or(PermissionModel::Default, |permission| permission.model);
    let rules = file
        .permission
        .as_ref()
        .map(|permission| {
            permission
                .rules
                .iter()
                .map(|rule| {
                    InvocationRule::new(rule.target, &rule.selector, rule.permission.into())
                })
                .collect()
        })
        .unwrap_or_default();
    InvocationPolicy::compile(model, rules).context("compile permission.rules selector regex")
}

fn has_meaningful_permission(file: &FileConfig) -> bool {
    file.permission.as_ref().is_some_and(|permission| {
        permission.model != PermissionModel::Default || !permission.rules.is_empty()
    })
}

#[derive(Debug, Deserialize)]
struct CompatModelConfig {
    #[serde(default)]
    model: Option<String>,
    #[serde(default, alias = "defaultModel", alias = "default_model")]
    default_model: Option<String>,
    #[serde(default)]
    provider: BTreeMap<String, CompatProviderConfig>,
    #[serde(default)]
    disabled_providers: Vec<String>,
    #[serde(default)]
    mcp: BTreeMap<String, CompatMcpConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct CompatProviderConfig {
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    options: CompatProviderOptions,
    #[serde(default)]
    models: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct CompatProviderOptions {
    #[serde(default, rename = "baseURL", alias = "base_url")]
    base_url: Option<String>,
    #[serde(default, rename = "apiKey", alias = "api_key")]
    api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CompatMcpConfig {
    #[serde(default, rename = "type")]
    server_type: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug)]
struct ImportedProvider {
    id: String,
    kind: &'static str,
    base_url: String,
    api_key: Option<String>,
    models: Vec<String>,
}

struct ImportedMcpServers {
    servers: BTreeMap<String, McpServerConfig>,
    skipped: usize,
}

/// Import provider models and local MCP servers from a Compat config into hya.
///
/// Reads `compat_config_path`, merges importable providers/MCP into
/// `hya_config_path` (creating parents as needed), and returns a summary of
/// what was imported or skipped. Errors if nothing importable is found.
pub fn import_compat_models_into_config(
    compat_config_path: &Path,
    hya_config_path: &Path,
) -> anyhow::Result<CompatImportSummary> {
    let raw = std::fs::read_to_string(compat_config_path)
        .with_context(|| format!("read Compat config {}", compat_config_path.display()))?;
    let config = parse_compat_model_config(&raw)
        .with_context(|| format!("parse Compat config {}", compat_config_path.display()))?;
    let providers = imported_compat_providers(&config);
    let mcp = imported_compat_mcp_servers(&config);
    if providers.is_empty() && mcp.servers.is_empty() {
        anyhow::bail!("Compat config has no importable provider models or local MCP servers");
    }
    let default_model =
        (!providers.is_empty()).then(|| imported_default_model(&config, &providers));
    let rendered = render_imported_hya_config_for_path(
        hya_config_path,
        default_model.as_deref(),
        &providers,
        &mcp.servers,
    )?;
    let parent = hya_config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("hya config should have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create hya config directory {}", parent.display()))?;
    std::fs::write(hya_config_path, rendered)
        .with_context(|| format!("write hya config {}", hya_config_path.display()))?;
    Ok(CompatImportSummary {
        compat_path: compat_config_path.to_path_buf(),
        config_path: hya_config_path.to_path_buf(),
        providers: providers.len(),
        models: providers.iter().map(|provider| provider.models.len()).sum(),
        mcp_servers: mcp.servers.len(),
        mcp_skipped: mcp.skipped,
    })
}

fn render_imported_hya_config_for_path(
    hya_config_path: &Path,
    default_model: Option<&str>,
    providers: &[ImportedProvider],
    mcp: &BTreeMap<String, McpServerConfig>,
) -> anyhow::Result<String> {
    let imported = render_imported_hya_config(default_model, providers, mcp);
    if hya_config_path.exists() {
        merge_import_into_existing_config(hya_config_path, &imported, !providers.is_empty())
    } else {
        Ok(imported)
    }
}

fn merge_import_into_existing_config(
    hya_config_path: &Path,
    imported_yaml: &str,
    replace_models: bool,
) -> anyhow::Result<String> {
    let existing_raw = std::fs::read_to_string(hya_config_path)
        .with_context(|| format!("read existing hya config {}", hya_config_path.display()))?;
    if existing_raw.trim().is_empty() {
        return Ok(imported_yaml.to_string());
    }
    let existing = serde_norway::from_str::<Value>(&existing_raw)
        .with_context(|| format!("parse existing hya config {}", hya_config_path.display()))?;
    let imported = serde_norway::from_str::<Value>(imported_yaml)
        .context("parse rendered hya model import")?;
    let mut existing_map = match existing {
        Value::Null => Mapping::new(),
        Value::Mapping(map) => map,
        _ => anyhow::bail!("existing hya config root must be a mapping for model import"),
    };
    let imported_map = match imported {
        Value::Mapping(map) => map,
        _ => anyhow::bail!("rendered hya model import root must be a mapping"),
    };
    if replace_models {
        for key in ["default_model", "providers"] {
            if let Some(value) = imported_map.get(key).cloned() {
                existing_map.insert(Value::String(key.to_string()), value);
            }
        }
    }
    if let Some(imported_mcp) = imported_map.get("mcp") {
        merge_imported_mcp(&mut existing_map, imported_mcp)?;
    }
    serde_norway::to_string(&Value::Mapping(existing_map)).context("render merged hya config")
}

fn merge_imported_mcp(existing_map: &mut Mapping, imported_mcp: &Value) -> anyhow::Result<()> {
    let Value::Mapping(imported_mcp_map) = imported_mcp else {
        anyhow::bail!("rendered hya MCP import must be a mapping");
    };
    if imported_mcp_map.is_empty() {
        return Ok(());
    }
    let mcp_key = Value::String("mcp".to_string());
    let mut merged_mcp = match existing_map.remove(&mcp_key) {
        Some(Value::Mapping(existing_mcp)) => existing_mcp,
        Some(Value::Null) | None => Mapping::new(),
        Some(_) => anyhow::bail!("existing hya config mcp must be a mapping for Compat import"),
    };
    for (key, value) in imported_mcp_map {
        merged_mcp.insert(key.clone(), value.clone());
    }
    existing_map.insert(mcp_key, Value::Mapping(merged_mcp));
    Ok(())
}

fn parse_compat_model_config(raw: &str) -> anyhow::Result<CompatModelConfig> {
    match serde_json::from_str(raw) {
        Ok(config) => Ok(config),
        Err(json_error) => {
            let jsonc = strip_jsonc(raw);
            serde_json::from_str(&jsonc).with_context(|| {
                format!("parse as JSON or JSONC; initial JSON error: {json_error}")
            })
        }
    }
}

fn imported_compat_providers(config: &CompatModelConfig) -> Vec<ImportedProvider> {
    let disabled = config
        .disabled_providers
        .iter()
        .map(|provider| provider.as_str())
        .collect::<BTreeSet<_>>();
    let default_model = config.model.as_deref().or(config.default_model.as_deref());
    let mut providers = Vec::new();
    for (id, provider) in &config.provider {
        if disabled.contains(id.as_str()) {
            continue;
        }
        let Some(base_url) = provider
            .options
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let mut models = provider
            .models
            .keys()
            .filter(|model| !model.trim().is_empty())
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some((provider_id, model_id)) = default_model.and_then(|model| model.split_once('/'))
            && provider_id == id
            && !model_id.trim().is_empty()
        {
            models.insert(model_id.to_string());
        }
        if models.is_empty() {
            continue;
        }
        providers.push(ImportedProvider {
            id: id.clone(),
            kind: compat_provider_kind(id, provider),
            base_url: base_url.to_string(),
            api_key: provider.options.api_key.clone(),
            models: models.into_iter().collect(),
        });
    }
    providers
}

fn imported_compat_mcp_servers(config: &CompatModelConfig) -> ImportedMcpServers {
    let mut servers = BTreeMap::new();
    let mut skipped = 0;
    for (name, server) in &config.mcp {
        let url = server
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(url) = url {
            servers.insert(
                name.clone(),
                McpServerConfig {
                    command: Vec::new(),
                    env: None,
                    url: Some(url),
                    transport: None,
                    enabled: server.enabled,
                    timeout_ms: server.timeout,
                },
            );
            continue;
        }
        if !is_importable_local_mcp(server) {
            skipped += 1;
            continue;
        }
        let env = (!server.environment.is_empty()).then(|| server.environment.clone());
        servers.insert(
            name.clone(),
            McpServerConfig {
                command: server.command.clone(),
                env,
                url: None,
                transport: None,
                enabled: server.enabled,
                timeout_ms: server.timeout,
            },
        );
    }
    ImportedMcpServers { servers, skipped }
}

fn is_importable_local_mcp(server: &CompatMcpConfig) -> bool {
    server
        .server_type
        .as_deref()
        .is_some_and(|server_type| server_type.eq_ignore_ascii_case("local"))
        && server
            .url
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        && server
            .command
            .first()
            .is_some_and(|part| !part.trim().is_empty())
}

fn compat_provider_kind(id: &str, provider: &CompatProviderConfig) -> &'static str {
    let text = format!(
        "{} {} {}",
        id,
        provider.npm.as_deref().unwrap_or_default(),
        provider.name.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if text.contains("anthropic") {
        "anthropic"
    } else if text.contains("google") || text.contains("gemini") {
        "google"
    } else {
        "openai-compatible"
    }
}

fn imported_default_model(config: &CompatModelConfig, providers: &[ImportedProvider]) -> String {
    let candidate = config
        .model
        .as_deref()
        .or(config.default_model.as_deref())
        .and_then(|model| served_imported_model(model, providers));
    candidate.unwrap_or_else(|| {
        let provider = &providers[0];
        format!("{}/{}", provider.id, provider.models[0])
    })
}

fn served_imported_model(model: &str, providers: &[ImportedProvider]) -> Option<String> {
    if let Some((provider_id, model_id)) = model.split_once('/') {
        if providers.iter().any(|provider| {
            provider.id == provider_id && provider.models.iter().any(|m| m == model_id)
        }) {
            return Some(model.to_string());
        }
    } else if providers
        .iter()
        .any(|provider| provider.models.iter().any(|m| m == model))
    {
        return Some(model.to_string());
    }
    None
}

fn render_imported_hya_config(
    default_model: Option<&str>,
    providers: &[ImportedProvider],
    mcp: &BTreeMap<String, McpServerConfig>,
) -> String {
    let mut lines = vec!["# Generated by hya first-run Compat import.".to_string()];
    if let Some(default_model) = default_model {
        lines.push(format!(
            "default_model: {}",
            quote_yaml_scalar(default_model)
        ));
        lines.push("providers:".to_string());
        for provider in providers {
            lines.push(format!("  {}:", quote_yaml_scalar(&provider.id)));
            lines.push(format!("    kind: {}", provider.kind));
            lines.push(format!(
                "    base_url: {}",
                quote_yaml_scalar(&provider.base_url)
            ));
            if let Some(api_key) = provider
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                lines.push(format!("    api_key: {}", quote_yaml_scalar(api_key)));
            }
            let models = provider
                .models
                .iter()
                .map(|model| quote_yaml_scalar(model))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("    models: [{models}]"));
        }
    } else {
        lines.push("default_model: hya/offline".to_string());
        lines.push("providers: {}".to_string());
    }
    render_imported_mcp_config(&mut lines, mcp);
    lines.push("plugins: {}".to_string());
    lines.push("permission:".to_string());
    lines.push("  model: default".to_string());
    lines.push("  rules: []".to_string());
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn render_imported_mcp_config(lines: &mut Vec<String>, mcp: &BTreeMap<String, McpServerConfig>) {
    if mcp.is_empty() {
        lines.push("mcp: {}".to_string());
        return;
    }
    lines.push("mcp:".to_string());
    for (name, server) in mcp {
        lines.push(format!("  {}:", quote_yaml_key(name)));
        if let Some(url) = server
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("    url: {}", quote_yaml_scalar(url)));
            if let Some(transport) = server
                .transport
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                lines.push(format!("    transport: {}", quote_yaml_scalar(transport)));
            }
        } else {
            let command = server
                .command
                .iter()
                .map(|part| quote_yaml_scalar(part))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("    command: [{command}]"));
        }
        if let Some(env) = server.env.as_ref().filter(|env| !env.is_empty()) {
            lines.push("    env:".to_string());
            for (key, value) in env {
                lines.push(format!(
                    "      {}: {}",
                    quote_yaml_key(key),
                    quote_yaml_scalar(value)
                ));
            }
        }
        if let Some(enabled) = server.enabled {
            lines.push(format!("    enabled: {enabled}"));
        }
        if let Some(timeout_ms) = server.timeout_ms {
            lines.push(format!("    timeout_ms: {timeout_ms}"));
        }
    }
}

fn quote_yaml_key(value: &str) -> String {
    quote_yaml_scalar(value)
}

fn quote_yaml_scalar(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ if ch.is_control() => escaped.push_str(&format!("\\u{:04X}", u32::from(ch))),
            _ => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

fn strip_jsonc(raw: &str) -> String {
    remove_trailing_json_commas(&strip_jsonc_comments(raw))
}

fn strip_jsonc_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                out.push(ch);
            }
            continue;
        }
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                let _ = chars.next();
                in_block_comment = false;
            }
            continue;
        }
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            let _ = chars.next();
            in_line_comment = true;
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            let _ = chars.next();
            in_block_comment = true;
            continue;
        }
        out.push(ch);
    }
    out
}

fn remove_trailing_json_commas(raw: &str) -> String {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(raw.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            index += 1;
            continue;
        }
        if ch == ',' {
            let mut lookahead = index + 1;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && (chars[lookahead] == '}' || chars[lookahead] == ']') {
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

/// Parse every provider declaration, preserving empty model lists for startup
/// discovery and normalizing explicit ids before route construction.
fn resolve_providers(file: &FileConfig) -> anyhow::Result<Vec<ParsedProvider>> {
    resolve_providers_filtered(file, None, true)
}

/// Parse and validate provider declarations. `only` restricts the result to
/// those ids; `resolve_secrets: false` validates without expanding
/// `{env:…}`/`{file:…}` keys (used to validate config edits).
fn resolve_providers_filtered(
    file: &FileConfig,
    only: Option<&BTreeSet<String>>,
    resolve_secrets: bool,
) -> anyhow::Result<Vec<ParsedProvider>> {
    let mut out = Vec::new();
    for (id, provider) in &file.providers {
        if only.is_some_and(|only| !only.contains(id)) {
            continue;
        }
        let kind: ProviderKind = provider.kind.into();
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for model in &provider.models {
            let (raw_id, name, reasoning_field, limit, modalities) = match model {
                ModelConfig::Id(id) => (id.as_str(), None, None, None, None),
                ModelConfig::Detailed(model) => (
                    model.id.as_str(),
                    model.name.as_deref(),
                    model.reasoning.as_ref(),
                    model.limit.as_ref(),
                    model.modalities.as_ref(),
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
            let fallback_variants = if reasoning_off {
                Vec::new()
            } else {
                kind.reasoning_variants()
            };
            let variants_configured =
                reasoning_off || reasoning.is_some_and(|config| config.variants.is_some());
            let configured_variants = reasoning
                .and_then(|config| config.variants.as_ref())
                .unwrap_or(&fallback_variants);
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
            if let Some(default) = explicit_default
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
                .map(|modalities| resolve_model_modalities(id, model_id, modalities))
                .transpose()?
                .flatten();
            let display_name = name
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
                .map(str::to_string);
            models.push(ParsedModel {
                id: model_id.to_string(),
                display_name,
                reasoning_default: resolve_default_reasoning(explicit_default, None, &variants),
                reasoning_variants: variants,
                variants_configured,
                explicit_default,
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
            kind,
            base_url: provider.base_url.clone(),
            api_key,
            models,
            retry: resolve_provider_retry(file.provider_retry.as_ref(), provider.retry.as_ref()),
        });
    }
    Ok(out)
}

fn resolve_mcp(file: &FileConfig) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    let mut out = BTreeMap::new();
    for (id, server) in &file.mcp {
        if server.enabled == Some(false) {
            continue;
        }
        // The server key is the namespace segment of `mcp__{server}__{tool}`;
        // a malformed key would compose ambiguous or unusable tool names.
        let key_valid = !id.is_empty()
            && !id.contains("__")
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !key_valid {
            anyhow::bail!(
                "mcp server name `{id}` must contain only ASCII letters, digits, `-`, and `_`"
            );
        }
        let env = server
            .env
            .as_ref()
            .map(|vars| {
                vars.iter()
                    .map(|(key, value)| {
                        resolve_secret(value).map(|resolved| (key.clone(), resolved))
                    })
                    .collect::<anyhow::Result<BTreeMap<_, _>>>()
            })
            .transpose()?;
        out.insert(
            id.clone(),
            McpServerConfig {
                command: server.command.clone(),
                env,
                url: None,
                transport: None,
                enabled: server.enabled,
                timeout_ms: server.timeout_ms,
            },
        );
    }
    Ok(out)
}

/// Resolve subagent caps from an optional file block, then apply per-field
/// `HYA_SUBAGENT_*` env overrides (env wins). Unset file fields and unparseable
/// env values fall back to the [`SubagentLimits`] default.
fn resolve_subagent_limits(file: Option<&SubagentLimitsFile>) -> SubagentLimits {
    let defaults = SubagentLimits::default();
    let mut limits = SubagentLimits {
        max_concurrency: file
            .and_then(|f| f.max_concurrency)
            .unwrap_or(defaults.max_concurrency),
        per_run_budget: file
            .and_then(|f| f.per_run_budget)
            .unwrap_or(defaults.per_run_budget),
        per_team_turn_budget: file
            .and_then(|f| f.per_team_turn_budget)
            .unwrap_or(defaults.per_team_turn_budget),
        per_team_message_budget: file
            .and_then(|f| f.per_team_message_budget)
            .unwrap_or(defaults.per_team_message_budget),
    };
    if let Ok(v) = std::env::var("HYA_SUBAGENT_MAX_CONCURRENCY")
        && let Ok(parsed) = v.trim().parse()
    {
        limits.max_concurrency = parsed;
    }
    if let Ok(v) = std::env::var("HYA_SUBAGENT_BUDGET")
        && let Ok(parsed) = v.trim().parse()
    {
        limits.per_run_budget = parsed;
    }
    if let Ok(v) = std::env::var("HYA_SUBAGENT_TURN_BUDGET")
        && let Ok(parsed) = v.trim().parse()
    {
        limits.per_team_turn_budget = parsed;
    }
    if let Ok(v) = std::env::var("HYA_SUBAGENT_MESSAGE_BUDGET")
        && let Ok(parsed) = v.trim().parse()
    {
        limits.per_team_message_budget = parsed;
    }
    limits
}

/// Resolve the live EventBus capacity: `HYA_EVENT_BUS_CAPACITY` if set and valid,
/// otherwise the raised [`hya_core::bus::DEFAULT_BUS_CAPACITY`]. A larger buffer keeps
/// 100+ concurrently-streaming subagents from lagging subscribers into a resync.
#[must_use]
pub fn resolve_event_bus_capacity() -> usize {
    if let Ok(v) = std::env::var("HYA_EVENT_BUS_CAPACITY")
        && let Ok(parsed) = v.trim().parse::<usize>()
        && parsed > 0
    {
        return parsed;
    }
    hya_core::bus::DEFAULT_BUS_CAPACITY
}

/// Build a [`CategoryRegistry`] from the file's `categories:` block. Each entry
/// is an ordered candidate list; empty lists are dropped since a category with
/// no concrete refs cannot resolve to anything servable.
fn resolve_categories(file: &FileConfig) -> CategoryRegistry {
    let mut entries = std::collections::HashMap::new();
    for (name, candidates) in &file.categories {
        if let Some(entry) = CategoryEntry::from_candidates(candidates) {
            entries.insert(name.clone(), entry);
        }
    }
    CategoryRegistry::from_entries(entries)
}

/// Resolve model categories independent of provider config, so the offline path
/// (where [`load`] returns `None`) and the spawn supervisor can build the same
/// registry the runtime holds.
#[must_use]
pub fn load_categories() -> CategoryRegistry {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .map(|file| resolve_categories(&file))
        .unwrap_or_default()
}

/// Every plugin id declared under `plugins:` (enabled or not). A project
/// plugin manifest with one of these ids never loads: config wins.
#[must_use]
pub fn load_configured_plugin_ids() -> Vec<String> {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .map(|file| file.plugins.into_keys().collect())
        .unwrap_or_default()
}

/// Resolve subagent caps independent of provider config, so the offline path
/// (where [`load`] returns `None`) still honors configured/env limits.
#[must_use]
pub fn load_subagent_limits() -> SubagentLimits {
    let file_block = config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .and_then(|file| file.subagents);
    resolve_subagent_limits(file_block.as_ref())
}

/// Compaction thresholds plus the token-accounting mode they are measured with.
///
/// The two travel together because a threshold is only as trustworthy as the
/// token count it is compared against.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextSettings {
    /// Thresholds handed to the engine's compaction plane.
    pub compaction: CompactionConfig,
    /// How window occupancy is measured for those thresholds.
    pub token_accounting: TokenAccountingMode,
}

/// Resolve a configured method order (oh-my-pi `compaction.methodOrder`).
///
/// A partial list is honoured and completed with the unmentioned methods in
/// default order — omp's filter-and-drop behaviour, minus its ability to
/// silently lose a mechanism. An unknown name or an empty list invalidates the
/// whole value, so a typo cannot reorder the ladder by accident; the caller
/// then keeps whatever it had (file value or engine default).
fn resolve_method_order(names: &[String]) -> Option<[hya_core::CompactionRung; 5]> {
    let mut order: Vec<hya_core::CompactionRung> = Vec::new();
    for name in names {
        let rung = hya_core::CompactionRung::from_wire_name(name.trim())?;
        if !order.contains(&rung) {
            order.push(rung);
        }
    }
    if order.is_empty() {
        return None;
    }
    for rung in hya_core::CompactionRung::DEFAULT_ORDER {
        if !order.contains(&rung) {
            order.push(rung);
        }
    }
    order.try_into().ok()
}

/// Resolve context settings from an optional file block, then apply per-field
/// `HYA_COMPACTION_*` and `HYA_TOKEN_ACCOUNTING` env overrides (env wins).
///
/// Unset file fields and unparseable values fall back to the engine default
/// rather than to a guess: an unrecognized accounting mode keeps whatever the
/// file asked for, and an unparseable number keeps the default.
fn resolve_context_settings(file: Option<&CompactionFile>) -> ContextSettings {
    let defaults = CompactionConfig::default();
    let mut settings = ContextSettings {
        compaction: CompactionConfig {
            token_threshold: file
                .and_then(|f| f.token_threshold)
                .unwrap_or(defaults.token_threshold),
            keep_recent: file
                .and_then(|f| f.keep_recent)
                .unwrap_or(defaults.keep_recent),
            context_fraction: file
                .and_then(|f| f.context_fraction)
                .unwrap_or(defaults.context_fraction),
            reserve_tokens: file
                .and_then(|f| f.reserve_tokens)
                .unwrap_or(defaults.reserve_tokens),
            summary_max_tokens: file
                .and_then(|f| f.summary_max_tokens)
                .unwrap_or(defaults.summary_max_tokens),
            method_order: file
                .and_then(|f| f.method_order.as_deref())
                .and_then(resolve_method_order)
                .unwrap_or(defaults.method_order),
        },
        token_accounting: file
            .and_then(|f| f.token_accounting.as_deref())
            .and_then(TokenAccountingMode::parse)
            .unwrap_or_default(),
    };
    if let Ok(v) = std::env::var("HYA_COMPACTION_THRESHOLD")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.token_threshold = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_KEEP_RECENT")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.keep_recent = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_CONTEXT_FRACTION")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.context_fraction = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_RESERVE_TOKENS")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.reserve_tokens = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_SUMMARY_MAX_TOKENS")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.summary_max_tokens = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_METHOD_ORDER") {
        let names: Vec<String> = v.split(',').map(str::to_string).collect();
        if let Some(parsed) = resolve_method_order(&names) {
            settings.compaction.method_order = parsed;
        }
    }
    if let Ok(v) = std::env::var("HYA_TOKEN_ACCOUNTING")
        && let Some(parsed) = TokenAccountingMode::parse(&v)
    {
        settings.token_accounting = parsed;
    }
    settings
}

/// File shape of the `goal:` block of `~/.config/hya/config.yaml`.
#[derive(Debug, Default, Deserialize)]
struct GoalFile {
    /// `provider/model` ref that judges goal mode when no `--evaluator-model`
    /// flag is given. Absent/blank → the worker's current model.
    #[serde(default)]
    evaluator_model: Option<String>,
}

/// Goal-mode settings resolved from config. There are no env overrides for
/// this block by design: the CLI flag is the only layer above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GoalSettings {
    /// Evaluator model from `goal.evaluator_model` (trimmed); `None` → the
    /// worker's current model judges.
    pub evaluator_model: Option<String>,
}

impl GoalSettings {
    fn from_block(block: Option<GoalFile>) -> Self {
        Self {
            evaluator_model: block
                .and_then(|goal| goal.evaluator_model)
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty()),
        }
    }
}

/// Resolve the goal evaluator model: the CLI flag wins over the config
/// `goal.evaluator_model`; without either, the worker's current model judges.
/// Blank/whitespace values count as unset at every layer, so an accidentally
/// empty setting can never shadow a lower layer or break resolution.
#[must_use]
pub fn resolve_evaluator_model<'a>(
    cli_flag: Option<&'a str>,
    config_value: Option<&'a str>,
    worker_current: &'a str,
) -> &'a str {
    let non_empty = |value: Option<&'a str>| value.map(str::trim).filter(|model| !model.is_empty());
    non_empty(cli_flag)
        .or_else(|| non_empty(config_value))
        .unwrap_or(worker_current)
}

/// Goal-mode settings from `config.yaml`.
#[must_use]
pub fn load_goal_settings() -> GoalSettings {
    let file_block = config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .and_then(|file| file.goal);
    GoalSettings::from_block(file_block)
}

/// Context settings from `config.yaml`, with env overrides applied.
#[must_use]
pub fn load_context_settings() -> ContextSettings {
    let file_block = config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .and_then(|file| file.compaction);
    resolve_context_settings(file_block.as_ref())
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

fn discovery_auth(kind: ProviderKind, credential: &ProviderCredential) -> CatalogAuth {
    match kind {
        ProviderKind::Anthropic | ProviderKind::Google => credential
            .token
            .clone()
            .map_or_else(CatalogAuth::unauthenticated, CatalogAuth::api_key),
        ProviderKind::OpenAiCodex => credential
            .token
            .clone()
            .map_or_else(CatalogAuth::unauthenticated, |token| {
                CatalogAuth::bearer(token, credential.account_id.clone())
            }),
        ProviderKind::GrokBuild => CatalogAuth::grok(
            credential.token.clone(),
            env!("CARGO_PKG_VERSION"),
            "grok-cli",
        ),
        ProviderKind::OpenAiCompatible | ProviderKind::OpenAiResponse => credential
            .token
            .clone()
            .map_or_else(CatalogAuth::unauthenticated, |token| {
                CatalogAuth::bearer(token, None)
            }),
    }
}

fn status_auth(credential: &ProviderCredential) -> ProviderAuthState {
    match credential.auth_presence() {
        AuthPresence::Credentialed => ProviderAuthState::Credentialed,
        AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
    }
}

fn failed_result(error: &CatalogFailure) -> ProviderCatalogResult {
    match error {
        CatalogFailure::Decode
        | CatalogFailure::Schema
        | CatalogFailure::BodyTooLarge
        | CatalogFailure::PaginationLimit => ProviderCatalogResult::Invalid,
        CatalogFailure::InvalidUrl
        | CatalogFailure::UnsafeUrl
        | CatalogFailure::Redirect
        | CatalogFailure::Transport
        | CatalogFailure::Timeout
        | CatalogFailure::HttpStatus { .. } => ProviderCatalogResult::Unavailable,
    }
}

/// One model of a provider's effective list: the model cache row merged
/// with the provider's config `models:` entry of the same id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EffectiveModel {
    id: String,
    display_name: Option<String>,
    reasoning_variants: Vec<String>,
    reasoning_default: Option<ReasoningEffort>,
    limit: hya_provider::ModelLimitOverride,
    /// Declared image input (config `modalities.input`); `None` unknown.
    image_input: Option<bool>,
    /// Declared reasoning support (config `reasoning`, else remote-list
    /// effort metadata); `None` unknown.
    reasoning_declared: Option<bool>,
    source: ModelCatalogSource,
}

/// Merge cached remote rows with config entries, per model id.
///
/// Remote-only rows keep their cached metadata; config-only entries keep
/// today's configured semantics; a model in both is an override whose config
/// fields win field by field and whose unset config fields fall back to the
/// cached metadata. Cached rows come first (remote order), then config-only
/// entries (config order).
fn merge_provider_models(
    provider: &ParsedProvider,
    cached: &[crate::model_cache::CachedModel],
) -> Vec<EffectiveModel> {
    let configured = provider
        .models
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in cached {
        let id = row.id.trim();
        if id.is_empty() || (provider.id == "hya" && id == "offline") || !seen.insert(id) {
            continue;
        }
        out.push(match configured.get(id) {
            Some(config) => override_model(provider.kind, config, row),
            None => remote_model(provider.kind, row),
        });
    }
    for model in &provider.models {
        if seen.insert(model.id.as_str()) {
            out.push(EffectiveModel {
                id: model.id.clone(),
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
    out
}

fn cached_variants(kind: ProviderKind, row: &crate::model_cache::CachedModel) -> Vec<String> {
    let variants = row
        .reasoning_variants
        .iter()
        .filter_map(|variant| ReasoningEffort::parse(variant))
        .map(|effort| effort.as_str().to_string())
        .collect::<Vec<_>>();
    if variants.is_empty() {
        kind.reasoning_variants()
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

fn remote_model(kind: ProviderKind, row: &crate::model_cache::CachedModel) -> EffectiveModel {
    EffectiveModel {
        id: row.id.trim().to_string(),
        display_name: row.display_name.clone(),
        reasoning_variants: cached_variants(kind, row),
        reasoning_default: row
            .reasoning_default
            .as_deref()
            .and_then(ReasoningEffort::parse),
        limit: hya_provider::ModelLimitOverride {
            context: row.context_limit,
            output: row.output_limit,
        },
        image_input: None,
        reasoning_declared: cached_reasoning_declared(row),
        source: ModelCatalogSource::Discovered,
    }
}

fn override_model(
    kind: ProviderKind,
    config: &ParsedModel,
    row: &crate::model_cache::CachedModel,
) -> EffectiveModel {
    let (reasoning_variants, reasoning_default) = if config.variants_configured {
        (config.reasoning_variants.clone(), config.reasoning_default)
    } else {
        let variants = cached_variants(kind, row);
        let default = config.explicit_default.or_else(|| {
            row.reasoning_default
                .as_deref()
                .and_then(ReasoningEffort::parse)
                .filter(|effort| {
                    *effort == ReasoningEffort::Off
                        || variants.iter().any(|variant| variant == effort.as_str())
                })
        });
        let default = resolve_default_reasoning(default, None, &variants);
        (variants, default)
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
fn route_for_models(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    models: &[EffectiveModel],
) -> anyhow::Result<HttpProvider> {
    let mut route = HttpProvider::new(
        provider.id.clone(),
        provider.kind,
        &provider.base_url,
        credential.token.clone(),
        models.iter().map(|model| model.id.clone()),
    )?
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
    if credential.use_codex_session {
        route = route.with_codex_session_auth(credential.account_id.clone());
    }
    if credential.use_grok_session {
        route = route.with_grok_session_auth(env!("CARGO_PKG_VERSION"), "grok-cli");
    }
    if credential.use_oauth_refresh && credential.token.is_some() {
        let resolver_id = provider.id.clone();
        let refresher_id = provider.id.clone();
        let resolver: BearerResolver = Arc::new(move || {
            crate::oauth::ensure_access_token(&resolver_id).map_err(map_oauth_error)
        });
        route = route.with_bearer_resolver(resolver);
        let refresher: AuthRefresher = Arc::new(move |stale_token: &str| {
            crate::oauth::force_refresh_access_token(&refresher_id, stale_token)
                .map(|_fresh| ())
                .map_err(map_oauth_error)
        });
        route = route.with_auth_refresher(refresher);
    }
    Ok(route)
}

struct ProviderPlanResult {
    route: Option<HttpProvider>,
    models: Vec<ProviderModel>,
    state: ProviderCatalogState,
}

/// What a discovery run does to the provider's model cache rows.
#[derive(Clone, Debug)]
enum CacheAction {
    /// Replace the rows with a fresh remote list (possibly empty).
    Replace(Vec<crate::model_cache::CachedModel>),
    /// Keep the existing rows (transient failure: stale beats nothing).
    Keep,
}

/// Outcome of one remote model-list fetch for a provider.
#[derive(Clone, Debug)]
pub struct DiscoveryResult {
    auth: ProviderAuthState,
    result: ProviderCatalogResult,
    cache: CacheAction,
    error: Option<String>,
}

impl DiscoveryResult {
    /// Whether the list was fetched and parsed (possibly empty).
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(
            self.result,
            ProviderCatalogResult::Models | ProviderCatalogResult::Empty
        )
    }

    /// Stable wire label: `models`, `empty`, `auth_required`,
    /// `auth_rejected`, `unavailable`, `invalid`, or `unsupported`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match (self.auth, self.result) {
            (ProviderAuthState::AuthRequired, _) => "auth_required",
            (ProviderAuthState::AuthRejected, _) => "auth_rejected",
            (_, ProviderCatalogResult::Models) => "models",
            (_, ProviderCatalogResult::Empty) => "empty",
            (_, ProviderCatalogResult::Invalid) => "invalid",
            (_, ProviderCatalogResult::Unsupported) => "unsupported",
            (_, ProviderCatalogResult::Unavailable | ProviderCatalogResult::Offline) => {
                "unavailable"
            }
        }
    }

    /// Bounded, non-secret failure description.
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Number of remote models fetched.
    #[must_use]
    pub fn model_count(&self) -> usize {
        match &self.cache {
            CacheAction::Replace(rows) => rows.len(),
            CacheAction::Keep => 0,
        }
    }

    fn timed_out(auth: ProviderAuthState) -> Self {
        Self {
            auth,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Keep,
            error: Some(CatalogFailure::Timeout.to_string()),
        }
    }
}

/// Fetch one provider's remote model list (bounded; no retry loop).
///
/// A 401/403 clears the provider's cached rows (they are not usable with this
/// credential); a transport, status, or decode failure keeps them.
async fn discover_provider(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
) -> DiscoveryResult {
    let presence = |auth: AuthPresence| match auth {
        AuthPresence::Credentialed => ProviderAuthState::Credentialed,
        AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
    };
    let fetched_at = crate::model_cache::now_ms();
    match discover_models(CatalogDiscoveryRequest::new(
        provider.id.clone(),
        provider.kind,
        provider.base_url.clone(),
        discovery_auth(provider.kind, credential),
    ))
    .await
    {
        ProviderDiscoveryOutcome::Discovered { models, auth } => {
            let rows = models
                .iter()
                .filter(|model| provider.id != "hya" || model.id != "offline")
                .map(|model| crate::model_cache::CachedModel::from_discovered(model, fetched_at))
                .collect::<Vec<_>>();
            DiscoveryResult {
                auth: presence(auth),
                result: if rows.is_empty() {
                    ProviderCatalogResult::Empty
                } else {
                    ProviderCatalogResult::Models
                },
                cache: CacheAction::Replace(rows),
                error: None,
            }
        }
        ProviderDiscoveryOutcome::Empty { auth } => DiscoveryResult {
            auth: presence(auth),
            result: ProviderCatalogResult::Empty,
            cache: CacheAction::Replace(Vec::new()),
            error: None,
        },
        ProviderDiscoveryOutcome::AuthRequired => DiscoveryResult {
            auth: ProviderAuthState::AuthRequired,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Replace(Vec::new()),
            error: Some("the model list requires an API key (HTTP 401/403)".to_string()),
        },
        ProviderDiscoveryOutcome::AuthRejected => DiscoveryResult {
            auth: ProviderAuthState::AuthRejected,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Replace(Vec::new()),
            error: Some("the provider rejected the API key (HTTP 401/403)".to_string()),
        },
        ProviderDiscoveryOutcome::Unsupported { .. } => DiscoveryResult {
            auth: status_auth(credential),
            result: ProviderCatalogResult::Unsupported,
            cache: CacheAction::Keep,
            error: Some("no model-list adapter for this provider kind".to_string()),
        },
        ProviderDiscoveryOutcome::Failed { error } => DiscoveryResult {
            auth: status_auth(credential),
            result: failed_result(&error),
            cache: CacheAction::Keep,
            error: Some(error.to_string()),
        },
    }
}

/// Apply a discovery result to the provider's cached rows, persisting a
/// replacement, and return the rows to plan with.
async fn apply_discovery(
    provider_id: &str,
    cached: Vec<crate::model_cache::CachedModel>,
    discovery: &DiscoveryResult,
) -> Vec<crate::model_cache::CachedModel> {
    match &discovery.cache {
        CacheAction::Replace(rows) => {
            crate::model_cache::store_provider_or_warn(provider_id, rows).await;
            rows.clone()
        }
        CacheAction::Keep => cached,
    }
}

/// Build one provider's route, catalog rows, and status from its cached rows
/// merged with its config entries.
fn plan_for_provider(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    cached: &[crate::model_cache::CachedModel],
    discovery: Option<&DiscoveryResult>,
) -> anyhow::Result<ProviderPlanResult> {
    let merged = merge_provider_models(provider, cached);
    let auth = discovery.map_or_else(|| status_auth(credential), |discovery| discovery.auth);
    let source = if merged.is_empty() {
        ProviderCatalogSource::None
    } else if merged
        .iter()
        .any(|model| model.source != ModelCatalogSource::Configured)
    {
        ProviderCatalogSource::Discovered
    } else {
        ProviderCatalogSource::Configured
    };
    let result = if merged.is_empty() {
        discovery.map_or(ProviderCatalogResult::Empty, |discovery| discovery.result)
    } else {
        ProviderCatalogResult::Models
    };
    let state = ProviderCatalogState {
        provider_id: provider.id.clone(),
        kind: provider.kind,
        source,
        auth,
        result,
    };
    if merged.is_empty() {
        return Ok(ProviderPlanResult {
            route: None,
            models: Vec::new(),
            state,
        });
    }
    let route = route_for_models(provider, credential, &merged)?;
    let models = hya_provider::Provider::catalog(&route);
    Ok(ProviderPlanResult {
        route: Some(route),
        models,
        state,
    })
}

/// Load Hya config and resolve every provider catalog before publishing.
///
/// A provider's effective model list is its model-cache rows
/// (`$XDG_CACHE_HOME/hya/model_cache.db`) merged per model id with its
/// config `models:` entries (config fields win). Providers with cached rows
/// or config entries start without waiting on discovery HTTP; a provider with
/// neither makes one bounded blocking request. Providers with no cached rows,
/// and discovery-only providers (empty `models:`), are queued on
/// [`ResolvedConfig::pending_discovery`] for background refresh.
pub async fn load() -> anyhow::Result<Option<ResolvedConfig>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if yaml.trim().is_empty() {
        return Ok(None);
    }
    let file = parse_config(&yaml)?;
    let has_tools = file.tools.is_some();
    let permission = resolve_permission(&file)?;
    let has_permission = has_meaningful_permission(&file);
    let mcp = resolve_mcp(&file)?;
    let parsed = resolve_providers(&file)?;
    if parsed.is_empty()
        && mcp.is_empty()
        && file.plugins.is_empty()
        && !has_permission
        && !has_tools
        && file.default_agent.is_none()
    {
        return Ok(None);
    }

    let mut cache = if parsed.is_empty() {
        BTreeMap::new()
    } else {
        crate::model_cache::read_all_or_empty().await
    };
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut tasks = tokio::task::JoinSet::new();
    let mut plans = Vec::new();
    let mut pending_discovery = Vec::new();
    for provider in parsed {
        let credential = resolve_provider_credential(&provider);
        let cached = cache.remove(&provider.id).unwrap_or_default();
        let pending = PendingCatalogDiscovery {
            provider_id: provider.id.clone(),
            kind: provider.kind,
            base_url: provider.base_url.clone(),
            credential: credential.clone(),
            provider: provider.clone(),
        };
        if !provider.models.is_empty() || !cached.is_empty() {
            if cached.is_empty() || provider.models.is_empty() {
                pending_discovery.push(pending);
            }
            plans.push(plan_for_provider(&provider, &credential, &cached, None)?);
            continue;
        }
        // Nothing cached and nothing configured: one bounded blocking
        // request so a discovery-only provider is usable on first start. A
        // queued refresh is kept only when this attempt fails.
        pending_discovery.push(pending);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let discovery = match tokio::time::timeout_at(deadline, async {
                let _permit = semaphore.acquire_owned().await.ok();
                discover_provider(&provider, &credential).await
            })
            .await
            {
                Ok(discovery) => discovery,
                Err(_) => DiscoveryResult::timed_out(status_auth(&credential)),
            };
            let rows = apply_discovery(&provider.id, Vec::new(), &discovery).await;
            plan_for_provider(&provider, &credential, &rows, Some(&discovery))
        });
    }
    while let Some(result) = tasks.join_next().await {
        let plan = result.map_err(|error| anyhow::anyhow!("catalog discovery task: {error}"))??;
        if plan.route.is_some() && !plan.models.is_empty() {
            pending_discovery.retain(|entry| entry.provider_id != plan.state.provider_id);
        }
        plans.push(plan);
    }
    plans.sort_by(|left, right| left.state.provider_id.cmp(&right.state.provider_id));
    let mut router = ProviderRouter::new();
    let mut live_models = Vec::new();
    let mut states = Vec::new();
    for plan in plans {
        if let Some(route) = plan.route {
            router = router.with(Arc::new(route));
        }
        live_models.extend(plan.models);
        states.push(plan.state);
    }
    if live_models.is_empty() {
        router = router.with(Arc::new(hya_provider::DevProvider::new()));
    }
    let requested_default = file.default_model.clone().map(hya_proto::ModelRef::new);
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        live_models,
        states,
        requested_default,
    ));
    router = router.with_catalog_snapshot(Arc::clone(&catalog));
    let categories = resolve_categories(&file);
    let subagents = resolve_subagent_limits(file.subagents.as_ref());
    let websearch = file
        .tools
        .map_or_else(WebSearchConfig::default, |tools| tools.websearch);
    Ok(Some(ResolvedConfig {
        router,
        default_model: catalog.default_model().to_string(),
        catalog,
        mcp,
        default_agent: file.default_agent,
        plugins: file.plugins,
        subagents,
        categories,
        permission,
        websearch,
        pending_discovery,
    }))
}

/// Replace the routes, rows, and statuses of `replaced` providers in the
/// current router/catalog with `plans`, keeping every other provider.
fn splice_catalog(
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
    replaced: &BTreeSet<String>,
    plans: Vec<ProviderPlanResult>,
) -> (ProviderRouter, Arc<ProviderCatalogSnapshot>) {
    let mut states = current
        .providers()
        .iter()
        .filter(|state| {
            !replaced.contains(&state.provider_id) && state.source != ProviderCatalogSource::Offline
        })
        .cloned()
        .collect::<Vec<_>>();
    states.extend(plans.iter().map(|plan| plan.state.clone()));
    states.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    let mut models = current
        .models()
        .iter()
        .filter(|model| {
            !replaced.contains(&model.provider_id) && model.source != ModelCatalogSource::Offline
        })
        .cloned()
        .collect::<Vec<_>>();
    for plan in &plans {
        models.extend(plan.models.iter().cloned());
    }
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        models,
        states,
        Some(current.default_model().clone()),
    ));
    let offline = catalog
        .models()
        .iter()
        .all(|model| model.source == ModelCatalogSource::Offline);
    // The built-in offline route (`hya`) is present only while the current
    // catalog has no live row; it is re-added below only while the new one
    // has none. A configured provider named `hya` is an ordinary route.
    let offline_route_present = current
        .models()
        .iter()
        .all(|model| model.source == ModelCatalogSource::Offline);
    let mut router = ProviderRouter::new();
    for provider in current_router.providers() {
        if replaced.contains(provider.id()) || (offline_route_present && provider.id() == "hya") {
            continue;
        }
        router = router.with(Arc::clone(provider));
    }
    for plan in plans {
        if let Some(route) = plan.route {
            router = router.with(Arc::new(route));
        }
    }
    if offline {
        router = router.with(Arc::new(hya_provider::DevProvider::new()));
    }
    router = router.with_catalog_snapshot(Arc::clone(&catalog));
    (router, catalog)
}

/// Force network discovery for the queued providers, update the model
/// cache, and return rebuilt routes plus a replacement snapshot.
///
/// Callers swap the result onto the live [`hya_core::SessionEngine`] and notify
/// the TUI (`catalog.updated`) after this future completes.
///
/// # Errors
/// Returns route-build failures.
pub async fn refresh_pending_catalogs(
    pending: Vec<PendingCatalogDiscovery>,
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
) -> anyhow::Result<(ProviderRouter, Arc<ProviderCatalogSnapshot>)> {
    let replaced = pending
        .iter()
        .map(|entry| entry.provider_id.clone())
        .collect::<BTreeSet<_>>();
    let mut cache = crate::model_cache::read_all_or_empty().await;
    let mut plans = Vec::with_capacity(pending.len());
    for entry in pending {
        let discovery = discover_provider(&entry.provider, &entry.credential).await;
        let cached = cache.remove(&entry.provider_id).unwrap_or_default();
        let rows = apply_discovery(&entry.provider_id, cached, &discovery).await;
        plans.push(plan_for_provider(
            &entry.provider,
            &entry.credential,
            &rows,
            Some(&discovery),
        )?);
    }
    Ok(splice_catalog(current, current_router, &replaced, plans))
}

/// When a live provider rebuild fetches the remote model list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoverMode {
    /// Always fetch (refresh, provider upsert).
    Always,
    /// Fetch only when the model cache has no rows for the provider (key save).
    IfUncached,
    /// Never fetch (key removal, config model edits).
    Never,
}

/// Result of rebuilding providers from the current `config.yaml`.
pub struct ProviderRebuild {
    /// Router with the rebuilt routes spliced in.
    pub router: ProviderRouter,
    /// Catalog with the rebuilt rows and statuses spliced in.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Rebuilt provider ids that are declared in `config.yaml`.
    pub configured: BTreeSet<String>,
    /// Fetch outcome per provider that fetched.
    pub discovery: BTreeMap<String, DiscoveryResult>,
}

/// Re-read `config.yaml`, credentials, and the model cache, then rebuild
/// `ids` (every declared provider when `None`) and splice them into the
/// current router/catalog. A requested id no longer declared in config loses
/// its route and rows.
///
/// # Errors
/// Returns config read/parse/validation or route-build failures.
pub async fn rebuild_providers(
    ids: Option<&BTreeSet<String>>,
    mode: DiscoverMode,
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
) -> anyhow::Result<ProviderRebuild> {
    let selected = match config_path() {
        Some(path) => {
            let yaml = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            if yaml.trim().is_empty() {
                Vec::new()
            } else {
                resolve_providers_filtered(&parse_config(&yaml)?, ids, true)?
            }
        }
        None => Vec::new(),
    };
    let configured = selected
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<BTreeSet<_>>();
    let mut replaced = configured.clone();
    if let Some(ids) = ids {
        replaced.extend(ids.iter().cloned());
    }
    let mut cache = crate::model_cache::read_all_or_empty().await;
    let mut plans = Vec::with_capacity(selected.len());
    let mut discovery = BTreeMap::new();
    for provider in selected {
        let credential = resolve_provider_credential(&provider);
        let mut cached = cache.remove(&provider.id).unwrap_or_default();
        let fetch = match mode {
            DiscoverMode::Always => true,
            DiscoverMode::IfUncached => cached.is_empty(),
            DiscoverMode::Never => false,
        };
        let outcome = if fetch {
            let outcome = discover_provider(&provider, &credential).await;
            cached = apply_discovery(&provider.id, cached, &outcome).await;
            Some(outcome)
        } else {
            None
        };
        plans.push(plan_for_provider(
            &provider,
            &credential,
            &cached,
            outcome.as_ref(),
        )?);
        if let Some(outcome) = outcome {
            discovery.insert(provider.id.clone(), outcome);
        }
    }
    let (router, catalog) = splice_catalog(current, current_router, &replaced, plans);
    Ok(ProviderRebuild {
        router,
        catalog,
        configured,
        discovery,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_tool::{Invocation, Mode};

    #[test]
    fn permission_yaml_compiles_documented_rules_defaults_and_validation() {
        let file = parse_config(
            r#"
permission:
  model: default
  rules:
    - target: tool
      selector: "^(read|grep)$"
      permission: Allow
    - target: mcp
      selector: "^mcp__github__"
      permission: Ask
    - target: command
      selector: "^git (status|diff)"
      permission: Deny
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
            "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: Allow\n",
        )
        .unwrap();
        assert!(resolve_permission(&invalid_regex).is_err());
        assert!(parse_config("permission:\n  model: unknown\n").is_err());
        assert!(
            parse_config(
                "permission:\n  rules:\n    - target: unknown\n      selector: x\n      permission: Allow\n",
            )
            .is_err()
        );
        // Lowercase rule effects match `permission.model` casing.
        let lower = parse_config(
            "permission:\n  rules:\n    - target: tool\n      selector: x\n      permission: allow\n",
        )
        .unwrap();
        assert!(resolve_permission(&lower).is_ok());
        // `mode` is accepted as an alias for `model`.
        let mode_alias = parse_config("permission:\n  mode: allow\n").unwrap();
        assert_eq!(
            resolve_permission(&mode_alias).unwrap().model(),
            PermissionModel::Allow
        );
    }

    #[test]
    fn subagent_limits_parse_from_file_and_env_wins() {
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
        let mut env = EnvGuard::new();
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
        let file = parse_config(
            "default_model: x\ncompaction:\n  token_threshold: 40000\n  keep_recent: 12\n  context_fraction: 0.6\n  reserve_tokens: 32768\n  summary_max_tokens: 8192\n  token_accounting: estimate\n",
        )
        .unwrap();
        let from_file = resolve_context_settings(file.compaction.as_ref());
        assert_eq!(from_file.compaction.token_threshold, 40_000);
        assert_eq!(from_file.compaction.keep_recent, 12);
        assert!((from_file.compaction.context_fraction - 0.6).abs() < f32::EPSILON);
        assert_eq!(from_file.compaction.reserve_tokens, 32_768);
        assert_eq!(from_file.compaction.summary_max_tokens, 8_192);
        assert_eq!(from_file.token_accounting, TokenAccountingMode::Estimate);

        // Absent block → engine defaults, accounting on Auto.
        let defaults = resolve_context_settings(None);
        assert_eq!(defaults.compaction.reserve_tokens, 16_384);
        assert_eq!(defaults.token_accounting, TokenAccountingMode::Auto);

        // Env wins over the file value.
        let mut env = EnvGuard::new();
        env.set("HYA_COMPACTION_RESERVE_TOKENS", "4096");
        env.set("HYA_TOKEN_ACCOUNTING", "provider");
        let overridden = resolve_context_settings(file.compaction.as_ref());
        env.remove("HYA_COMPACTION_RESERVE_TOKENS");
        env.remove("HYA_TOKEN_ACCOUNTING");
        assert_eq!(overridden.compaction.reserve_tokens, 4_096, "env must win");
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
        // The `goal:` block parses; absent blocks stay absent.
        let file =
            parse_config("default_model: x\ngoal:\n  evaluator_model: deep/o3-mini\n").unwrap();
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
    fn compaction_method_order_parses_from_file_and_env_wins() {
        // oh-my-pi's own default order, spelled as an omp user would.
        let file = parse_config(
            "default_model: x\ncompaction:\n  method_order: [remote, snapcompact, handoff, shake, soft]\n",
        )
        .unwrap();
        let from_file = resolve_context_settings(file.compaction.as_ref());
        assert_eq!(
            from_file.compaction.method_order,
            hya_core::parse_method_order(&["remote", "snapcompact", "handoff", "shake", "soft"])
                .unwrap(),
            "file order is honoured"
        );

        // Env wins over the file order; a partial list is completed with the
        // unmentioned methods in default order (omp filter-and-drop, but the
        // ladder never loses a mechanism).
        let mut env = EnvGuard::new();
        env.set("HYA_COMPACTION_METHOD_ORDER", "handoff, soft, shake");
        let overridden = resolve_context_settings(file.compaction.as_ref());
        assert_eq!(
            overridden.compaction.method_order,
            hya_core::parse_method_order(&["handoff", "soft", "shake", "remote", "snapcompact"])
                .unwrap(),
            "env order wins and missing methods append in default order"
        );

        // An unknown method name invalidates the env value, which keeps the
        // file order — matching how unparseable numbers keep the file value.
        env.set("HYA_COMPACTION_METHOD_ORDER", "shake, banana");
        let bogus_env = resolve_context_settings(file.compaction.as_ref());
        assert_eq!(
            bogus_env.compaction.method_order, from_file.compaction.method_order,
            "an invalid env order keeps the file order"
        );

        // An unknown name in the file falls back to the engine default rather
        // than silently dropping a mechanism from the ladder.
        let broken =
            parse_config("default_model: x\ncompaction:\n  method_order: [shake, banana]\n")
                .unwrap();
        let bogus_file = resolve_context_settings(broken.compaction.as_ref());
        assert_eq!(
            bogus_file.compaction.method_order,
            hya_core::CompactionConfig::default().method_order,
            "a broken file order keeps the default ladder"
        );
    }

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
    fn parses_websearch_tool_config() {
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
        assert!(resolve_categories(&bare).is_empty());
    }

    #[test]
    fn parses_providers_kinds_and_models() {
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
        // Env-sensitive assertions take the same gate as env writers, so a
        // concurrent override test can never leak a value into this read.
        let _env = EnvGuard::new();
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
        assert!(parse_providers("{}").unwrap().is_empty());
    }

    #[test]
    fn parsed_models_keep_provider_reasoning_variants() {
        let parsed = parse_providers(FIXTURE).unwrap();

        let openai = parsed.iter().find(|entry| entry.id == "gw-oai").unwrap();
        assert_eq!(
            openai.models[0].reasoning_variants,
            vec!["minimal", "low", "medium", "high", "xhigh"]
        );
        let anthropic = parsed.iter().find(|entry| entry.id == "gw-anth").unwrap();
        assert_eq!(
            anthropic.models[0].reasoning_variants,
            vec!["low", "medium", "high", "max"]
        );
        let google = parsed.iter().find(|entry| entry.id == "gw-google").unwrap();
        assert_eq!(google.models[0].reasoning_variants, vec!["high", "max"]);
    }

    #[test]
    fn response_model_config_resolves_default_and_all_variants() {
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
    fn grok_build_config_defaults_to_high_reasoning() {
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
        assert_eq!(
            parsed[0].models[0].reasoning_default,
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn grok_build_uses_login_token_with_session_headers() {
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
        let cred =
            resolve_provider_credential_with(ProviderKind::GrokBuild, None, Some("inline-oauth"))
                .unwrap();
        assert_eq!(cred.token.as_deref(), Some("inline-oauth"));
        assert!(cred.use_grok_session);
    }

    #[test]
    fn non_grok_provider_uses_bearer_without_session_headers() {
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
    fn legacy_string_models_keep_chat_aliases_and_highest_default() {
        for kind in ["openai", "openai-compatible", "openai-completion"] {
            let yaml = format!(
                "providers:\n  gw:\n    kind: {kind}\n    base_url: https://gw.example/v1\n    models: [m1]\n"
            );
            let parsed = parse_providers(&yaml).unwrap();
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].kind, ProviderKind::OpenAiCompatible);
            assert_eq!(
                parsed[0].models[0].reasoning_default,
                Some(ReasoningEffort::XHigh)
            );
        }
    }

    #[test]
    fn resolves_env_template_key() {
        // SAFETY: single-threaded test; sets then reads a unique env var.
        let mut env = EnvGuard::new();
        env.set("HYA_TEST_KEY_XYZ", "resolved-secret");
        assert_eq!(
            resolve_secret("{env:HYA_TEST_KEY_XYZ}").unwrap(),
            "resolved-secret"
        );
        assert_eq!(resolve_secret("literal-key").unwrap(), "literal-key");
    }

    #[test]
    fn parses_provider_without_apikey() {
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

    #[test]
    fn parses_mcp_and_skips_disabled_servers() {
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
        assert_eq!(mcp.len(), 1);
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

    /// Serialize tests that mutate process-global environment variables and
    /// restore the previous values on drop, so concurrent tests never observe
    /// leaked `HYA_*` state (a race that otherwise fires as flaky failures).
    ///
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(String, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        /// Take the env gate. A test must call this **once** — re-entering
        /// while an earlier guard is still alive would wait on itself.
        fn new() -> Self {
            let lock = ENV_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self {
                _lock: lock,
                saved: Vec::new(),
            }
        }

        fn set(&mut self, name: &str, value: &str) {
            if !self.saved.iter().any(|(n, _)| n == name) {
                self.saved.push((name.to_string(), std::env::var_os(name)));
            }
            unsafe { std::env::set_var(name, value) };
        }

        fn remove(&mut self, name: &str) {
            if !self.saved.iter().any(|(n, _)| n == name) {
                self.saved.push((name.to_string(), std::env::var_os(name)));
            }
            unsafe { std::env::remove_var(name) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, previous) in self.saved.iter().rev() {
                match previous {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    #[test]
    fn mcp_server_keys_must_be_valid_namespace_tokens() {
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

    #[test]
    fn import_compat_models_writes_hya_provider_config() {
        let dir =
            std::env::temp_dir().join(format!("hya-compat-model-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let compat = dir.join("opencode.json");
        let hya_config = dir.join("hya/config.yaml");

        std::fs::write(
            &compat,
            r#"{
  "model": "gateway/gpt-5.5",
  "disabled_providers": ["disabled"],
  "provider": {
    "gateway": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://gateway.example/v1",
        "apiKey": "{env:GATEWAY_KEY}"
      },
      "models": {
        "gpt-5.5": { "name": "GPT 5.5" },
        "gpt-5.4": { "name": "GPT 5.4" }
      }
    },
    "anthropic": {
      "npm": "@ai-sdk/anthropic",
      "options": {
        "baseURL": "https://api.anthropic.com/v1",
        "apiKey": "{env:ANTHROPIC_API_KEY}"
      },
      "models": {
        "claude-sonnet-4-6": { "name": "Claude Sonnet" }
      }
    },
    "disabled": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://disabled.example/v1",
        "apiKey": "unused"
      },
      "models": { "disabled-model": {} }
    }
  }
}"#,
        )
        .unwrap();

        let summary = import_compat_models_into_config(&compat, &hya_config).unwrap();

        assert_eq!(summary.providers, 2);
        assert_eq!(summary.models, 3);
        let text = std::fs::read_to_string(&hya_config).unwrap();
        let file = parse_config(&text).unwrap();
        assert_eq!(file.default_model.as_deref(), Some("gateway/gpt-5.5"));
        assert_eq!(file.providers.len(), 2);
        let gateway = file.providers.get("gateway").unwrap();
        assert!(matches!(gateway.kind, ProviderKindConfig::Openai));
        assert_eq!(gateway.base_url, "https://gateway.example/v1");
        assert_eq!(gateway.api_key.as_deref(), Some("{env:GATEWAY_KEY}"));
        let model_ids = gateway
            .models
            .iter()
            .map(|model| match model {
                ModelConfig::Id(id) => id.as_str(),
                ModelConfig::Detailed(model) => model.id.as_str(),
            })
            .collect::<Vec<_>>();
        assert_eq!(model_ids, vec!["gpt-5.4", "gpt-5.5"]);
        let anthropic = file.providers.get("anthropic").unwrap();
        assert!(matches!(anthropic.kind, ProviderKindConfig::Anthropic));
        assert!(!text.contains("disabled-model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_compat_models_preserves_existing_non_model_config() {
        let dir = std::env::temp_dir().join(format!(
            "hya-compat-model-import-merge-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let compat = dir.join("opencode.json");
        let hya_config = dir.join("hya/config.yaml");
        std::fs::create_dir_all(hya_config.parent().unwrap()).unwrap();
        std::fs::write(
            &hya_config,
            r#"
default_model: old/model
default_agent: build
providers:
  old:
    kind: openai-compatible
    base_url: https://old.example/v1
    api_key: old-key
    models: [old-model]
mcp:
  filesystem:
    command: [node, server.js]
plugins:
  memory:
    command: [python3, memory.py]
"#,
        )
        .unwrap();
        std::fs::write(
            &compat,
            r#"{
  "model": "gateway/gpt-5.5",
  "provider": {
    "gateway": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "https://gateway.example/v1" },
      "models": { "gpt-5.5": {} }
    }
  }
}"#,
        )
        .unwrap();

        let summary = import_compat_models_into_config(&compat, &hya_config).unwrap();

        assert_eq!(summary.providers, 1);
        assert_eq!(summary.models, 1);
        let text = std::fs::read_to_string(&hya_config).unwrap();
        assert!(text.contains("default_agent: build"));
        assert!(text.contains("filesystem:"));
        assert!(text.contains("memory:"));
        assert!(text.contains("gateway:"));
        assert!(!text.contains("old-model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_compat_accepts_local_mcp_without_provider_models() {
        let dir =
            std::env::temp_dir().join(format!("hya-compat-mcp-only-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let compat = dir.join("opencode.json");
        let hya_config = dir.join("hya/config.yaml");

        std::fs::write(
            &compat,
            r#"{
  "mcp": {
    "true": {
      "type": "local",
      "command": ["node", "server.js"],
      "environment": {
        "TOKEN": "{env:TOKEN}",
        "null": "reserved-null",
        "123": "numeric-key",
        "": "empty-key",
        "MULTILINE": "line\nbreak"
      },
      "enabled": false,
      "timeout": 2500
    },
    "remote": {
      "type": "remote",
      "url": "https://example.invalid/mcp"
    }
  }
}"#,
        )
        .unwrap();

        let summary = import_compat_models_into_config(&compat, &hya_config).unwrap();

        assert_eq!(summary.providers, 0);
        assert_eq!(summary.models, 0);
        assert_eq!(summary.mcp_servers, 2);
        assert_eq!(summary.mcp_skipped, 0);
        let text = std::fs::read_to_string(&hya_config).unwrap();
        let file = parse_config(&text).unwrap();
        assert_eq!(file.default_model.as_deref(), Some("hya/offline"));
        assert!(file.providers.is_empty());
        let local = file.mcp.get("true").unwrap();
        assert_eq!(local.command, vec!["node", "server.js"]);
        assert_eq!(
            local
                .env
                .as_ref()
                .and_then(|env| env.get("TOKEN"))
                .map(String::as_str),
            Some("{env:TOKEN}")
        );
        assert_eq!(
            local
                .env
                .as_ref()
                .and_then(|env| env.get("null"))
                .map(String::as_str),
            Some("reserved-null")
        );
        assert_eq!(
            local
                .env
                .as_ref()
                .and_then(|env| env.get("123"))
                .map(String::as_str),
            Some("numeric-key")
        );
        assert_eq!(
            local
                .env
                .as_ref()
                .and_then(|env| env.get(""))
                .map(String::as_str),
            Some("empty-key")
        );
        assert_eq!(
            local
                .env
                .as_ref()
                .and_then(|env| env.get("MULTILINE"))
                .map(String::as_str),
            Some("line\nbreak")
        );
        assert_eq!(local.enabled, Some(false));
        assert_eq!(local.timeout_ms, Some(2500));
        assert_eq!(
            file.mcp
                .get("remote")
                .and_then(|remote| remote.url.as_deref()),
            Some("https://example.invalid/mcp")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_compat_maps_remote_mcp_url_to_url_transport() {
        let dir = std::env::temp_dir().join(format!(
            "hya-compat-mcp-remote-import-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let compat = dir.join("opencode.json");
        let hya_config = dir.join("hya/config.yaml");

        std::fs::write(
            &compat,
            r#"{
  "mcp": {
    "remote": {
      "type": "remote",
      "url": "https://example.invalid/mcp",
      "enabled": true,
      "timeout": 4000
    },
    "empty-url": {
      "type": "remote",
      "url": "  "
    }
  }
}"#,
        )
        .unwrap();

        let summary = import_compat_models_into_config(&compat, &hya_config).unwrap();

        assert_eq!(summary.mcp_servers, 1, "only the url entry imports");
        assert_eq!(summary.mcp_skipped, 1, "blank url is not importable");
        let text = std::fs::read_to_string(&hya_config).unwrap();
        assert!(
            text.contains("url: \"https://example.invalid/mcp\"")
                || text.contains("url: https://example.invalid/mcp"),
            "got: {text}"
        );
        let file = parse_config(&text).unwrap();
        let remote = file.mcp.get("remote").unwrap();
        assert_eq!(remote.url.as_deref(), Some("https://example.invalid/mcp"));
        assert!(remote.command.is_empty());
        assert_eq!(remote.enabled, Some(true));
        assert_eq!(remote.timeout_ms, Some(4000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    const GLM_LIMIT_YAML: &str = "providers:\n  12th:\n    kind: anthropic\n    base_url: https://api.12th.day/v1\n    models:\n      - glm-5.3\n      - id: glm-5.3-flash\n        limit:\n          context: 1048576\n          output: 131072\n      - id: glm-5.3-ctx\n        limit:\n          context: 262144\n";

    #[test]
    fn object_models_parse_context_and_output_limits() {
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
                message.contains("provider 12th model glm-5.3-flash") && message.contains(expected),
                "{limit}: got {message}"
            );
        }
    }

    #[tokio::test]
    async fn configured_model_modalities_reach_route_image_input() {
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
        let error = parse_providers(bad).unwrap_err().to_string();
        assert!(error.contains("modalities"), "{error}");
    }

    #[tokio::test]
    async fn configured_model_limits_reach_route_capabilities() {
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
        let route = plan.route.unwrap();
        assert_eq!(
            hya_provider::Provider::capabilities(
                &route,
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
        assert_eq!(shared.reasoning_default, Some(ReasoningEffort::Low));

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
        let route = plan.route.unwrap();
        assert_eq!(
            hya_provider::Provider::capabilities(&route, &hya_proto::ModelRef::new("gw/plain"))
                .map(|caps| caps.max_context),
            Some(200_000),
            "the runtime keeps its context fallback"
        );
    }

    #[test]
    fn cached_output_limit_never_contradicts_a_configured_context() {
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

    fn temp_config(label: &str, yaml: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("hya-cfg-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        path
    }

    #[test]
    fn upsert_provider_entry_preserves_other_keys_and_creates_discovery_providers() {
        let path = temp_config(
            "upsert",
            "default_model: gw/m\nprovider_retry:\n  max_attempts: 5\nproviders:\n  gw:\n    kind: openai\n    base_url: https://old.example/v1\n    api_key: \"{env:GW_KEY}\"\n    models: [m]\n    retry:\n      max_attempts: 2\n",
        );
        assert!(
            !upsert_provider_entry(&path, "gw", "anthropic", "https://new.example/v1").unwrap()
        );
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
}
