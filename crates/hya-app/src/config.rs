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
use hya_core::{CategoryEntry, CategoryRegistry, SubagentLimits};
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
#[derive(Debug, Default, Deserialize)]
struct SubagentLimitsFile {
    #[serde(default)]
    max_depth: Option<u32>,
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
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelConfig {
    Id(String),
    Detailed(DetailedModelConfig),
}

#[derive(Debug, Deserialize)]
struct DetailedModelConfig {
    id: String,
    #[serde(default)]
    reasoning: Option<ModelReasoningConfig>,
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

#[derive(Debug)]
struct ParsedModel {
    id: String,
    reasoning_variants: Vec<String>,
    reasoning_default: Option<ReasoningEffort>,
}

struct ParsedProvider {
    id: String,
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    models: Vec<ParsedModel>,
}

/// Resolved optional authentication material for one configured provider.
#[derive(Clone, PartialEq, Eq)]
struct ProviderCredential {
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
/// keys. Does **not** write secrets — tokens live under `auth/<provider>.yaml`.
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
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    let existing = if config_path.exists() {
        std::fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?
    } else {
        DEFAULT_CONFIG_YAML.to_string()
    };
    let mut root: Value = if existing.trim().is_empty() {
        serde_norway::from_str(DEFAULT_CONFIG_YAML).context("parse default config")?
    } else {
        serde_norway::from_str(&existing)
            .with_context(|| format!("parse {}", config_path.display()))?
    };
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("config root must be a mapping"))?;

    // Ensure providers mapping exists.
    if !map.contains_key(Value::String("providers".into())) {
        map.insert(
            Value::String("providers".into()),
            Value::Mapping(Mapping::new()),
        );
    }
    let providers = map
        .get_mut(Value::String("providers".into()))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| anyhow::anyhow!("providers must be a mapping"))?;

    let mut provider_map = Mapping::new();
    provider_map.insert(
        Value::String("kind".into()),
        Value::String(kind.to_string()),
    );
    provider_map.insert(
        Value::String("base_url".into()),
        Value::String(base_url.to_string()),
    );

    let models_value = providers
        .get(Value::String(provider_id.into()))
        .and_then(Value::as_mapping)
        .and_then(|provider| provider.get(Value::String("models".into())))
        .cloned()
        .unwrap_or_else(|| Value::Sequence(Vec::new()));
    provider_map.insert(Value::String("models".into()), models_value);

    // Preserve existing inline api_key when re-logging the same provider.
    if let Some(existing_provider) = providers
        .get(Value::String(provider_id.into()))
        .and_then(Value::as_mapping)
        && let Some(api_key) = existing_provider.get(Value::String("api_key".into()))
    {
        provider_map.insert(Value::String("api_key".into()), api_key.clone());
    }

    providers.insert(
        Value::String(provider_id.to_string()),
        Value::Mapping(provider_map),
    );

    // Do not change `default_model` during OAuth login. In particular, a
    // fetched catalog or provider-specific default must not become startup
    // catalog authority.

    let rendered = serde_norway::to_string(&root).context("render updated hya config.yaml")?;
    std::fs::write(config_path, rendered)
        .with_context(|| format!("write {}", config_path.display()))?;
    Ok(())
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
        eprintln!(
            "hya: edit the starter config to add a provider; automatic Compat/OpenCode import is disabled"
        );
    }
    Ok(())
}

/// First existing Compat/OpenCode config path among the usual candidates, if any.
///
/// Checks `COMPAT_CONFIG`, then XDG/home OpenCode config locations.
#[must_use]
pub fn default_compat_config_path() -> Option<PathBuf> {
    compat_config_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

fn compat_config_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("COMPAT_CONFIG") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        push_compat_dir_candidates(&mut candidates, PathBuf::from(dir).join("compat"));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        push_compat_dir_candidates(&mut candidates, home.join(".config/opencode"));
        push_compat_dir_candidates(&mut candidates, home.join(".opencode"));
    }
    candidates
}

fn push_compat_dir_candidates(candidates: &mut Vec<PathBuf>, dir: PathBuf) {
    candidates.push(dir.join("opencode.json"));
    candidates.push(dir.join("config.json"));
    candidates.push(dir.join("opencode.jsonc"));
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
        let command = server
            .command
            .iter()
            .map(|part| quote_yaml_scalar(part))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("    command: [{command}]"));
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
    let mut out = Vec::new();
    for (id, provider) in &file.providers {
        let kind: ProviderKind = provider.kind.into();
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for model in &provider.models {
            let (raw_id, reasoning) = match model {
                ModelConfig::Id(id) => (id.as_str(), None),
                ModelConfig::Detailed(model) => (model.id.as_str(), model.reasoning.as_ref()),
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
            let fallback_variants = kind.reasoning_variants();
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
            models.push(ParsedModel {
                id: model_id.to_string(),
                reasoning_default: resolve_default_reasoning(explicit_default, None, &variants),
                reasoning_variants: variants,
            });
        }
        let api_key = provider
            .api_key
            .as_deref()
            .map(resolve_secret)
            .transpose()?;
        out.push(ParsedProvider {
            id: id.clone(),
            kind,
            base_url: provider.base_url.clone(),
            api_key,
            models,
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
        max_depth: file.and_then(|f| f.max_depth).unwrap_or(defaults.max_depth),
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
    if let Ok(v) = std::env::var("HYA_SUBAGENT_MAX_DEPTH")
        && let Ok(parsed) = v.trim().parse()
    {
        limits.max_depth = parsed;
    }
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
                "{reason}. Re-login: hya-backend oauth login --provider {provider} --type {oauth_type}"
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

fn route_for_plan(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    models: &[String],
    source: ModelCatalogSource,
) -> anyhow::Result<HttpProvider> {
    let mut route = HttpProvider::new(
        provider.id.clone(),
        provider.kind,
        &provider.base_url,
        credential.token.clone(),
        models.iter().cloned(),
    )?
    .with_catalog_source(source)
    .with_model_reasoning_variants(
        provider
            .models
            .iter()
            .map(|model| (model.id.clone(), model.reasoning_variants.clone())),
    )
    .with_model_reasoning_defaults(
        provider
            .models
            .iter()
            .map(|model| (model.id.clone(), model.reasoning_default)),
    );
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

async fn resolve_provider_plan(
    provider: ParsedProvider,
    credential: ProviderCredential,
) -> anyhow::Result<ProviderPlanResult> {
    let auth_state = status_auth(&credential);
    if !provider.models.is_empty() {
        let ids = provider
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        let route = route_for_plan(&provider, &credential, &ids, ModelCatalogSource::Configured)?;
        let models = hya_provider::Provider::catalog(&route);
        return Ok(ProviderPlanResult {
            route: Some(route),
            models,
            state: ProviderCatalogState {
                provider_id: provider.id,
                kind: provider.kind,
                source: ProviderCatalogSource::Configured,
                auth: auth_state,
                result: ProviderCatalogResult::Models,
            },
        });
    }

    let outcome = discover_models(CatalogDiscoveryRequest::new(
        provider.id.clone(),
        provider.kind,
        provider.base_url.clone(),
        discovery_auth(provider.kind, &credential),
    ))
    .await;
    let (ids, source, auth, result) = match outcome {
        ProviderDiscoveryOutcome::Discovered { models, auth } => {
            let ids = models
                .into_iter()
                .map(|model| model.id)
                .filter(|model_id| provider.id != "hya" || model_id != "offline")
                .collect::<Vec<_>>();
            let (source, result) = if ids.is_empty() {
                (ProviderCatalogSource::None, ProviderCatalogResult::Empty)
            } else {
                (
                    ProviderCatalogSource::Discovered,
                    ProviderCatalogResult::Models,
                )
            };
            (
                ids,
                source,
                match auth {
                    AuthPresence::Credentialed => ProviderAuthState::Credentialed,
                    AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
                },
                result,
            )
        }
        ProviderDiscoveryOutcome::Empty { auth } => (
            Vec::new(),
            ProviderCatalogSource::None,
            match auth {
                AuthPresence::Credentialed => ProviderAuthState::Credentialed,
                AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
            },
            ProviderCatalogResult::Empty,
        ),
        ProviderDiscoveryOutcome::AuthRequired => (
            Vec::new(),
            ProviderCatalogSource::None,
            ProviderAuthState::AuthRequired,
            ProviderCatalogResult::Unavailable,
        ),
        ProviderDiscoveryOutcome::AuthRejected => (
            Vec::new(),
            ProviderCatalogSource::None,
            ProviderAuthState::AuthRejected,
            ProviderCatalogResult::Unavailable,
        ),
        ProviderDiscoveryOutcome::Unsupported { .. } => (
            Vec::new(),
            ProviderCatalogSource::None,
            auth_state,
            ProviderCatalogResult::Unsupported,
        ),
        ProviderDiscoveryOutcome::Failed { error } => (
            Vec::new(),
            ProviderCatalogSource::None,
            auth_state,
            failed_result(&error),
        ),
    };
    if ids.is_empty() {
        return Ok(ProviderPlanResult {
            route: None,
            models: Vec::new(),
            state: ProviderCatalogState {
                provider_id: provider.id,
                kind: provider.kind,
                source,
                auth,
                result,
            },
        });
    }
    let route = route_for_plan(&provider, &credential, &ids, ModelCatalogSource::Discovered)?;
    let models = hya_provider::Provider::catalog(&route);
    Ok(ProviderPlanResult {
        route: Some(route),
        models,
        state: ProviderCatalogState {
            provider_id: provider.id,
            kind: provider.kind,
            source,
            auth,
            result,
        },
    })
}

/// Load Hya config and resolve every empty provider catalog before publishing.
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
    {
        return Ok(None);
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut tasks = tokio::task::JoinSet::new();
    let mut plans = Vec::new();
    for provider in parsed {
        let credential = resolve_provider_credential(&provider);
        if !provider.models.is_empty() {
            plans.push(resolve_provider_plan(provider, credential).await?);
            continue;
        }
        let timeout_provider_id = provider.id.clone();
        let timeout_kind = provider.kind;
        let timeout_auth = status_auth(&credential);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            match tokio::time::timeout_at(deadline, async {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| anyhow::anyhow!("catalog discovery semaphore closed"))?;
                resolve_provider_plan(provider, credential).await
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Ok(ProviderPlanResult {
                    route: None,
                    models: Vec::new(),
                    state: ProviderCatalogState {
                        provider_id: timeout_provider_id,
                        kind: timeout_kind,
                        source: ProviderCatalogSource::None,
                        auth: timeout_auth,
                        result: ProviderCatalogResult::Unavailable,
                    },
                }),
            }
        });
    }
    while let Some(result) = tasks.join_next().await {
        plans.push(result.map_err(|error| anyhow::anyhow!("catalog discovery task: {error}"))??);
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
    }))
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
                .evaluate(&Invocation::command("shell", "git status"))
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
        let file = parse_config(
            "default_model: x\nsubagents:\n  max_depth: 9\n  max_concurrency: 200\n  per_run_budget: 1000\n  per_team_turn_budget: 700\n  per_team_message_budget: 800\n",
        )
        .unwrap();
        let from_file = resolve_subagent_limits(file.subagents.as_ref());
        assert_eq!(from_file.max_depth, 9);
        assert_eq!(from_file.max_concurrency, 200);
        assert_eq!(from_file.per_run_budget, 1000);
        assert_eq!(from_file.per_team_turn_budget, 700);
        assert_eq!(from_file.per_team_message_budget, 800);

        // Absent block → all defaults (per_run_budget raised to 1024 for swarms).
        let defaults = resolve_subagent_limits(None);
        assert_eq!(defaults, SubagentLimits::default());
        assert_eq!(defaults.per_run_budget, 1024);

        // The new per-team budgets honor their env overrides too.
        unsafe { std::env::set_var("HYA_SUBAGENT_MESSAGE_BUDGET", "5") };
        let msg = resolve_subagent_limits(file.subagents.as_ref());
        unsafe { std::env::remove_var("HYA_SUBAGENT_MESSAGE_BUDGET") };
        assert_eq!(
            msg.per_team_message_budget, 5,
            "env wins for message budget"
        );
        assert_eq!(msg.per_team_turn_budget, 700, "untouched field stays file");

        // Env override wins over the file value.
        unsafe { std::env::set_var("HYA_SUBAGENT_MAX_DEPTH", "3") };
        let overridden = resolve_subagent_limits(file.subagents.as_ref());
        unsafe { std::env::remove_var("HYA_SUBAGENT_MAX_DEPTH") };
        assert_eq!(overridden.max_depth, 3, "env must win over file");
        assert_eq!(
            overridden.max_concurrency, 200,
            "untouched field stays file"
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
        unsafe { std::env::set_var("HYA_TEST_KEY_XYZ", "resolved-secret") };
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
  compat:
    kind: compat
";
        let file = parse_config(yaml).unwrap();
        assert_eq!(file.plugins.len(), 3);
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
            file.plugins.get("compat").unwrap().kind,
            hya_plugin::messages::PluginKindWire::Compat
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
        assert_eq!(summary.mcp_servers, 1);
        assert_eq!(summary.mcp_skipped, 1);
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
        assert!(!file.mcp.contains_key("remote"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
