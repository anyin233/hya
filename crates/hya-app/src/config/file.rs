use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_bundle::BuiltinBundleSelection;
use hya_engine::{
    CategoryEntry, CategoryRegistry, CompactionConfig, SubagentLimits, TokenAccountingMode,
};
use hya_mcp::McpServerConfig;
use hya_plugin::config::PluginEntry;
use hya_provider::ReasoningEffort;
use hya_tool::{InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionTarget};
use serde::Deserialize;

use super::provider::{ProviderConfig, ProviderRetryFile};

/// Supported `config.yaml` schema version.
pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// Bundle loading configuration (`bundles:` block in `config.yaml` v2).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BundlesConfig {
    /// Built-in first-party bundle profile and selective enable/disable lists.
    pub builtin: BuiltinBundleSelection,
}

/// Top-level shape of `~/.config/hya/config.yaml` (v2).
#[derive(Debug, Default, Deserialize)]
pub(super) struct FileConfig {
    /// Required schema version (`2` only).
    #[allow(dead_code)]
    #[serde(default)]
    pub(super) schema_version: Option<u32>,
    /// Model used when neither `--model` nor `HYA_MODEL` is set.
    #[serde(default)]
    pub(super) default_model: Option<String>,
    /// Global default reasoning effort label.
    #[serde(default)]
    pub(super) reasoning: Option<String>,
    /// Agent selected by default when a workdir does not specify one. Falls back to `build`.
    #[serde(default)]
    pub(super) default_agent: Option<String>,
    #[serde(default)]
    pub(super) providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    agents: BTreeMap<String, crate::agent_model_config::AgentFileSettings>,
    #[serde(default)]
    pub(super) mcp: BTreeMap<String, McpServerConfig>,
    #[serde(default)]
    pub(super) plugins: BTreeMap<String, PluginEntry>,
    #[serde(default)]
    pub(super) bundles: BundlesConfig,
    #[serde(default)]
    pub(super) tools: Option<ToolsConfig>,
    /// Logical model categories: each maps a name (e.g. `deep`) to an ordered
    /// list of concrete `provider/model` refs (first = preferred, rest =
    /// failover). Absent → no categories (agents fall back to their own model).
    #[serde(default)]
    categories: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    permission: Option<PermissionConfig>,
    /// Default replay budget for every provider route. Per-provider `retry:`
    /// blocks override individual fields; `HYA_PROVIDER_RETRY_*` env vars win
    /// over both. Absent → [`hya_provider::RetryConfig`] defaults.
    #[serde(default)]
    pub(super) provider_retry: Option<ProviderRetryFile>,
    /// Per-Project catalog scope cache limits (`catalog_scopes:` block).
    /// Absent → [`hya_engine::CatalogScopeCacheConfig`] defaults.
    #[serde(default)]
    catalog_scopes: Option<CatalogScopesFile>,
}

/// Immutable on-disk startup settings shared with the engine builder.
#[derive(Debug, Default)]
pub struct EngineSettings {
    pub(super) file: FileConfig,
    pub(super) config_file: Option<PathBuf>,
}

impl EngineSettings {
    pub(crate) fn resolved_config_file(&self) -> PathBuf {
        self.config_file.clone().unwrap_or_else(active_config_path)
    }

    fn read_bundle_config_value(&self, bundle_ids: &[&str]) -> Option<serde_norway::Value> {
        read_bundle_config_for(&self.resolved_config_file(), bundle_ids)
    }

    /// Read a bundle's `config.yml` as a JSON value (`Value::Null` when absent
    /// or unreadable).
    #[must_use]
    pub fn bundle_config_json(&self, bundle_id: &str) -> serde_json::Value {
        read_bundle_config_json(&self.resolved_config_file(), bundle_id)
    }

    /// Active built-in bundle selection from `bundles.builtin`.
    #[must_use]
    pub fn builtin_bundles(&self) -> &BuiltinBundleSelection {
        &self.file.bundles.builtin
    }

    /// Override the active built-in bundle selection (for example `--bare`).
    pub fn set_builtin_bundles(&mut self, selection: BuiltinBundleSelection) {
        self.file.bundles.builtin = selection;
    }

    pub(crate) fn agents(
        &self,
    ) -> anyhow::Result<(
        hya_engine::AgentModelConfiguration,
        BTreeMap<String, ReasoningEffort>,
    )> {
        crate::agent_model_config::AgentModelConfigFiles::new(self.resolved_config_file())
            .load_with_builtin(&self.file.agents)
    }

    /// Goal evaluator settings resolved from the `hya/goal-loop` bundle config.
    pub fn goal(&self) -> GoalSettings {
        if !self.file.bundles.builtin.is_enabled("hya/goal-loop") {
            return GoalSettings::default();
        }
        let block = self
            .read_bundle_config_value(&["hya/goal-loop", "hya/goal"])
            .and_then(|val| decode_bundle_section::<GoalFile>(&val, Some("goal")));
        GoalSettings::from_block(block)
    }

    pub(crate) fn categories(&self) -> CategoryRegistry {
        resolve_categories(&self.file)
    }

    pub(crate) fn subagents(&self) -> SubagentLimits {
        let block = self
            .read_bundle_config_value(&["hya/extended-tools", "hya/multi-agent"])
            .and_then(|val| decode_bundle_section::<SubagentLimitsFile>(&val, Some("subagents")));
        resolve_subagent_limits(block.as_ref())
    }

    pub(crate) fn context(&self) -> ContextSettings {
        let block = if self.file.bundles.builtin.is_enabled("hya/compaction") {
            self.read_bundle_config_value(&["hya/compaction"])
                .and_then(|val| decode_bundle_section::<CompactionFile>(&val, Some("compaction")))
        } else {
            None
        };
        resolve_context_settings(block.as_ref())
    }

    pub(crate) fn reasoning(&self) -> Option<ReasoningEffort> {
        self.file
            .reasoning
            .as_deref()
            .and_then(ReasoningEffort::parse)
    }

    pub(crate) fn scopes(&self) -> hya_engine::CatalogScopeCacheConfig {
        resolve_catalog_scope_cache(self.file.catalog_scopes.as_ref())
    }

    pub(crate) fn code_mode(&self) -> CodeMode {
        self.file
            .tools
            .as_ref()
            .map_or_else(CodeMode::default, |tools| tools.code.mode)
    }

    pub(crate) fn plugin_ids(&self) -> Vec<String> {
        self.file.plugins.keys().cloned().collect()
    }

    pub(crate) fn formatter(&self) -> hya_tool::FormatterPlane {
        if !self.file.bundles.builtin.is_enabled("hya/base-tools") {
            return hya_tool::FormatterPlane::default();
        }
        let raw: crate::formatter_config::RawFormatterConfig = self
            .read_bundle_config_value(&["hya/base-tools", "hya/coding-extras"])
            .and_then(|val| {
                val.get("formatter")
                    .cloned()
                    .and_then(|sub| serde_norway::from_value(sub).ok())
            })
            .unwrap_or_default();
        hya_tool::FormatterPlane::new(Arc::new(crate::BuiltinFormatterProvider::new(raw.into())))
    }

    pub(crate) fn lsp(&self) -> anyhow::Result<hya_tool::LspPlane> {
        if !self.file.bundles.builtin.is_enabled("hya/extended-tools") {
            return Ok(hya_tool::LspPlane::default());
        }
        let selection = self
            .read_bundle_config_value(&["hya/extended-tools", "hya/coding-extras"])
            .and_then(|val| {
                val.get("lsp")
                    .cloned()
                    .and_then(|sub| serde_norway::from_value(sub).ok())
            });
        crate::lsp::from_selection(selection)
    }
}

/// `catalog_scopes:` block: how many Project/Directory catalog scopes (and
/// their project plugin and bundle processes) stay cached, and for how long
/// an unbound scope is kept.
#[derive(Debug, Default, Deserialize)]
struct CatalogScopesFile {
    /// Most cached scopes; the least recently bound beyond it are dropped.
    #[serde(default)]
    max: Option<usize>,
    /// Seconds a scope may stay unbound before it is dropped.
    #[serde(default)]
    idle_ttl_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolsConfig {
    #[serde(default)]
    code: CodeToolConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeToolConfig {
    #[serde(default)]
    mode: CodeMode,
}

/// `tools.code.mode`: whether built-in agents get the code-mode tool, a tool
/// that runs a JavaScript program calling the agent's other tools.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeMode {
    /// The tool is not loaded.
    #[default]
    Normal,
    /// The tool is offered next to the agent's other tools.
    WithTools,
    /// The tool is the only one offered; the others are called from programs.
    Only,
}

impl CodeMode {
    /// The tool's name in a built-in agent's view.
    pub const TOOL: &'static str = "code-mode__run";
}

/// `tools.code.mode` of the active config file, for surfaces that run
/// without a composed runtime (bundle listings). An unreadable or invalid
/// file reads as [`CodeMode::Normal`]; starting a runtime reports it.
#[must_use]
pub fn configured_code_mode() -> CodeMode {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .and_then(|file| file.tools)
        .map_or_else(CodeMode::default, |tools| tools.code.mode)
}

/// Built-in first-party bundle selection from the active `config.yaml`,
/// defaulting to [`BuiltinBundleSelection::full`] when absent or unreadable.
#[must_use]
pub fn configured_builtin_bundles() -> BuiltinBundleSelection {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|yaml| !yaml.trim().is_empty())
        .and_then(|yaml| parse_config(&yaml).ok())
        .map(|file| file.bundles.builtin)
        .unwrap_or_default()
}

fn read_bundle_config_for(config_file: &Path, bundle_ids: &[&str]) -> Option<serde_norway::Value> {
    for &id in bundle_ids {
        let Ok(location) = crate::bundle_config::bundle_config_location(
            config_file,
            id,
            crate::bundle_config::BundleConfigScope::User,
        ) else {
            continue;
        };
        let Ok(yaml) = std::fs::read_to_string(location.file()) else {
            continue;
        };
        if yaml.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_norway::from_str::<serde_norway::Value>(&yaml) {
            return Some(value);
        }
    }
    None
}

pub(crate) fn read_bundle_config_json(config_file: &Path, bundle_id: &str) -> serde_json::Value {
    read_bundle_config_for(config_file, &[bundle_id])
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn decode_bundle_section<T: for<'de> Deserialize<'de>>(
    value: &serde_norway::Value,
    nested_key: Option<&str>,
) -> Option<T> {
    if let Some(key) = nested_key
        && let Some(nested) = value.get(key)
    {
        return serde_norway::from_value(nested.clone()).ok();
    }
    serde_norway::from_value(value.clone()).ok()
}

#[derive(Debug, Default, Deserialize)]
struct PermissionConfig {
    /// Policy model (`allow` / `default` / `strict` / `danger`).
    #[serde(default)]
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

/// Rule effect, spelled in lowercase like `permission.model`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PermissionModeConfig {
    Allow,
    Deny,
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
    keep_recent: Option<usize>,
    #[serde(default)]
    summary_max_tokens: Option<u32>,
    /// `auto`, `provider`, or `estimate`; anything else is ignored.
    #[serde(default)]
    token_accounting: Option<String>,
}

pub(super) const DEFAULT_CONFIG_YAML: &str = "schema_version: 2\ndefault_model: hya/offline\nproviders: {}\nmcp: {}\nplugins: {}\nbundles:\n  builtin:\n    profile: full\n    enable: []\n    disable: []\npermission:\n  model: default\n  rules: []\n";

/// Result of creating a brand-new default `config.yaml` on first run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConfig {
    /// Absolute (or resolved) path of the file that was written.
    pub path: PathBuf,
}

pub(super) fn config_path() -> Option<PathBuf> {
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

pub(super) fn parse_config(yaml: &str) -> anyhow::Result<FileConfig> {
    let value: serde_norway::Value =
        serde_norway::from_str(yaml).context("parse hya config.yaml")?;
    let Some(map) = value.as_mapping() else {
        anyhow::bail!(
            "config.yaml must be a YAML mapping with `schema_version: 2`; run `hya-migration run` to migrate legacy v1 configs"
        );
    };
    match map.get("schema_version") {
        Some(version) if version.as_u64() == Some(u64::from(CONFIG_SCHEMA_VERSION)) => {}
        Some(version) => {
            let found = version
                .as_u64()
                .map_or_else(|| format!("{version:?}"), |n| n.to_string());
            anyhow::bail!(
                "unsupported config.yaml `schema_version: {found}`; only `schema_version: 2` is supported — run `hya-migration run` to migrate v1 configuration"
            );
        }
        None => {
            anyhow::bail!(
                "missing required `schema_version: 2` in config.yaml (v1 configs are no longer supported); run `hya-migration run` to migrate"
            );
        }
    }
    const REMOVED_V1_SECTIONS: [(&str, &str); 5] = [
        ("compaction", "hya/compaction"),
        ("formatter", "hya/base-tools"),
        ("goal", "hya/goal-loop"),
        ("lsp", "hya/extended-tools"),
        ("subagents", "hya/extended-tools"),
    ];
    for (key, bundle_id) in REMOVED_V1_SECTIONS {
        if map.contains_key(key) {
            anyhow::bail!(
                "config.yaml v1 top-level section `{key}` was moved to per-bundle config for `{bundle_id}`; run `hya-migration run` to migrate"
            );
        }
    }
    if let Some(tools) = map.get("tools").and_then(serde_norway::Value::as_mapping) {
        for key in ["websearch", "webfetch"] {
            if tools.contains_key(key) {
                anyhow::bail!(
                    "config.yaml v1 section `tools.{key}` was moved to per-bundle config for `hya/network`; run `hya-migration run` to migrate"
                );
            }
        }
    }
    serde_norway::from_str(yaml).context("parse hya config.yaml")
}

pub(super) fn resolve_permission(file: &FileConfig) -> anyhow::Result<InvocationPolicy> {
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

pub(super) fn resolve_mcp(file: &FileConfig) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    let mut out = BTreeMap::new();
    for (id, server) in &file.mcp {
        // The server key is the namespace segment of `mcp__{server}__{tool}`;
        // a malformed key would compose ambiguous or unusable tool names.
        if !hya_mcp::bridge::valid_server_name(id) {
            anyhow::bail!(
                "mcp server name `{id}` must contain only ASCII letters, digits, `-`, and `_`"
            );
        }
        // Resolve secrets at source preparation, so disabled or broken MCP
        // declarations cannot prevent unrelated configuration from loading.
        out.insert(id.clone(), server.clone());
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
/// otherwise the raised [`hya_engine::bus::DEFAULT_BUS_CAPACITY`]. A larger buffer keeps
/// 100+ concurrently-streaming subagents from lagging subscribers into a resync.
#[must_use]
pub fn resolve_event_bus_capacity() -> usize {
    if let Ok(v) = std::env::var("HYA_EVENT_BUS_CAPACITY")
        && let Ok(parsed) = v.trim().parse::<usize>()
        && parsed > 0
    {
        return parsed;
    }
    hya_engine::bus::DEFAULT_BUS_CAPACITY
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

/// Resolve the catalog scope cache limits from an optional file block;
/// absent fields keep the defaults (32 scopes, 30 minutes idle).
fn resolve_catalog_scope_cache(
    file: Option<&CatalogScopesFile>,
) -> hya_engine::CatalogScopeCacheConfig {
    let defaults = hya_engine::CatalogScopeCacheConfig::default();
    hya_engine::CatalogScopeCacheConfig {
        max_scopes: file.and_then(|f| f.max).unwrap_or(defaults.max_scopes),
        idle_ttl: file
            .and_then(|f| f.idle_ttl_secs)
            .map_or(defaults.idle_ttl, std::time::Duration::from_secs),
    }
}

pub(crate) fn parse_plugin_config(
    yaml: &str,
) -> anyhow::Result<Vec<hya_plugin::config::PluginSpec>> {
    let file = parse_config(yaml)?;
    let declarations = hya_plugin::config::merge(file.plugins.clone(), Vec::new());
    let mut specs = crate::plugins::resolve(file.plugins);
    let resolved = specs
        .iter()
        .map(|spec| spec.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    // An unavailable adapter is a preparation failure, not a removed entry.
    specs.extend(
        declarations
            .into_iter()
            .filter(|spec| !resolved.contains(&spec.id)),
    );
    Ok(specs)
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

/// Compaction settings plus the token-accounting mode `ContextStatus` is
/// measured with.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextSettings {
    /// Compaction and terminal handoff values resolved from the shared config block.
    pub compaction: CompactionConfig,
    /// How window occupancy is measured.
    pub token_accounting: TokenAccountingMode,
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
            keep_recent: file
                .and_then(|f| f.keep_recent)
                .unwrap_or(defaults.keep_recent),
            summary_max_tokens: file
                .and_then(|f| f.summary_max_tokens)
                .unwrap_or(defaults.summary_max_tokens),
        },
        token_accounting: file
            .and_then(|f| f.token_accounting.as_deref())
            .and_then(TokenAccountingMode::parse)
            .unwrap_or_default(),
    };
    if let Ok(v) = std::env::var("HYA_COMPACTION_KEEP_RECENT")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.keep_recent = parsed;
    }
    if let Ok(v) = std::env::var("HYA_COMPACTION_SUMMARY_MAX_TOKENS")
        && let Ok(parsed) = v.trim().parse()
    {
        settings.compaction.summary_max_tokens = parsed;
    }
    if let Ok(v) = std::env::var("HYA_TOKEN_ACCOUNTING")
        && let Some(parsed) = TokenAccountingMode::parse(&v)
    {
        settings.token_accounting = parsed;
    }
    settings
}

/// File shape of the `goal:` block of `~/.config/hya/config.yaml`.
#[derive(Clone, Debug, Default, Deserialize)]
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

#[cfg(test)]
mod tests;
