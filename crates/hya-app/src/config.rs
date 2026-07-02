//! Build a provider router from hya's own config (`~/.config/hya/config.yaml`).
//!
//! Reads YAML: each entry under `providers.<id>` maps to one `HttpProvider`
//! (route chosen by `kind`), and the union of `models` becomes the set hya can
//! address. API keys come from `~/.config/hya/auth/<id>.yaml` (via `hya login`)
//! or an inline `api_key`. Absent config or key → caller falls back to offline.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_core::SubagentLimits;
use hya_mcp::McpServerConfig;
use hya_plugin::config::PluginEntry;
use hya_provider::{HttpProvider, ProviderKind, ProviderRouter};
use serde::Deserialize;
use serde_norway::{Mapping, Value};

pub struct ResolvedConfig {
    pub router: ProviderRouter,
    pub default_model: String,
    pub models: Vec<ModelEntry>,
    pub has_providers: bool,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub plugins: BTreeMap<String, PluginEntry>,
    pub default_agent: Option<String>,
    pub subagents: SubagentLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
    pub reasoning_variants: Vec<String>,
}

impl ModelEntry {
    #[must_use]
    pub fn model_ref(&self) -> String {
        if self.provider.is_empty() {
            self.id.clone()
        } else {
            format!("{}/{}", self.provider, self.id)
        }
    }

    #[must_use]
    pub fn matches_model_ref(&self, model: &str) -> bool {
        if self.id == model {
            return true;
        }
        let Some((provider, model_id)) = model.split_once('/') else {
            return false;
        };
        self.provider == provider && self.id == model_id
    }
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
    /// Bounded nested/parallel subagent caps. Absent → defaults; per-field env
    /// overrides (`HYA_SUBAGENT_*`) win over file values.
    #[serde(default)]
    subagents: Option<SubagentLimitsFile>,
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
    models: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProviderKindConfig {
    #[serde(alias = "openai-compatible")]
    Openai,
    Anthropic,
    Google,
}

impl From<ProviderKindConfig> for ProviderKind {
    fn from(kind: ProviderKindConfig) -> Self {
        match kind {
            ProviderKindConfig::Openai => Self::OpenAiCompatible,
            ProviderKindConfig::Anthropic => Self::Anthropic,
            ProviderKindConfig::Google => Self::Google,
        }
    }
}

struct ParsedProvider {
    id: String,
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    models: Vec<String>,
}

const DEFAULT_CONFIG_YAML: &str = "default_model: offline\nproviders: {}\nmcp: {}\nplugins: {}\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeImportSummary {
    pub opencode_path: PathBuf,
    pub config_path: PathBuf,
    pub providers: usize,
    pub models: usize,
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
/// Unlike [`config_path`] (which only returns a path that exists), this always
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

pub fn first_run_config_bootstrap(interactive: bool) -> anyhow::Result<()> {
    let Some(created) = ensure_config_file().context("create default hya config")? else {
        return Ok(());
    };
    if !interactive {
        return Ok(());
    }

    eprintln!("hya: created default config at {}", created.path.display());
    let Some(opencode_path) = default_opencode_config_path() else {
        eprintln!("hya: no OpenCode config found to import; keeping the starter config");
        return Ok(());
    };
    eprintln!("hya: found OpenCode config at {}", opencode_path.display());
    eprintln!(
        "hya: import copies provider base URLs, model IDs, and API key values/templates into hya config"
    );
    if !prompt_yes_no("hya: import OpenCode model config now?")? {
        eprintln!("hya: keeping the starter config; edit it later to add live providers");
        return Ok(());
    }

    match import_opencode_models_into_config(&opencode_path, &created.path) {
        Ok(summary) => eprintln!(
            "hya: imported {} providers and {} models into {}",
            summary.providers,
            summary.models,
            summary.config_path.display()
        ),
        Err(error) => eprintln!("hya: OpenCode import skipped ({error:#})"),
    }
    Ok(())
}

fn prompt_yes_no(prompt: &str) -> anyhow::Result<bool> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{prompt} [y/N] ").context("write first-run prompt")?;
    stderr.flush().context("flush first-run prompt")?;

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("read first-run prompt response")?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[must_use]
pub fn default_opencode_config_path() -> Option<PathBuf> {
    opencode_config_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

fn opencode_config_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("OPENCODE_CONFIG") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        push_opencode_dir_candidates(&mut candidates, PathBuf::from(dir).join("opencode"));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        push_opencode_dir_candidates(&mut candidates, home.join(".config/opencode"));
        push_opencode_dir_candidates(&mut candidates, home.join(".opencode"));
    }
    candidates
}

fn push_opencode_dir_candidates(candidates: &mut Vec<PathBuf>, dir: PathBuf) {
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

#[derive(Debug, Deserialize)]
struct OpencodeModelConfig {
    #[serde(default)]
    model: Option<String>,
    #[serde(default, alias = "defaultModel", alias = "default_model")]
    default_model: Option<String>,
    #[serde(default)]
    provider: BTreeMap<String, OpencodeProviderConfig>,
    #[serde(default)]
    disabled_providers: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpencodeProviderConfig {
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    options: OpencodeProviderOptions,
    #[serde(default)]
    models: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct OpencodeProviderOptions {
    #[serde(default, rename = "baseURL", alias = "base_url")]
    base_url: Option<String>,
    #[serde(default, rename = "apiKey", alias = "api_key")]
    api_key: Option<String>,
}

#[derive(Debug)]
struct ImportedProvider {
    id: String,
    kind: &'static str,
    base_url: String,
    api_key: Option<String>,
    models: Vec<String>,
}

pub fn import_opencode_models_into_config(
    opencode_config_path: &Path,
    hya_config_path: &Path,
) -> anyhow::Result<OpencodeImportSummary> {
    let raw = std::fs::read_to_string(opencode_config_path)
        .with_context(|| format!("read OpenCode config {}", opencode_config_path.display()))?;
    let config = parse_opencode_model_config(&raw)
        .with_context(|| format!("parse OpenCode config {}", opencode_config_path.display()))?;
    let providers = imported_opencode_providers(&config);
    if providers.is_empty() {
        anyhow::bail!("OpenCode config has no importable provider models");
    }
    let default_model = imported_default_model(&config, &providers);
    let rendered =
        render_model_only_imported_hya_config(hya_config_path, &default_model, &providers)?;
    let parent = hya_config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("hya config should have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create hya config directory {}", parent.display()))?;
    std::fs::write(hya_config_path, rendered)
        .with_context(|| format!("write hya config {}", hya_config_path.display()))?;
    Ok(OpencodeImportSummary {
        opencode_path: opencode_config_path.to_path_buf(),
        config_path: hya_config_path.to_path_buf(),
        providers: providers.len(),
        models: providers.iter().map(|provider| provider.models.len()).sum(),
    })
}

fn render_model_only_imported_hya_config(
    hya_config_path: &Path,
    default_model: &str,
    providers: &[ImportedProvider],
) -> anyhow::Result<String> {
    let imported = render_imported_hya_config(default_model, providers);
    if hya_config_path.exists() {
        merge_model_import_into_existing_config(hya_config_path, &imported)
    } else {
        Ok(imported)
    }
}

fn merge_model_import_into_existing_config(
    hya_config_path: &Path,
    imported_yaml: &str,
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
    for key in ["default_model", "providers"] {
        if let Some(value) = imported_map.get(key).cloned() {
            existing_map.insert(Value::String(key.to_string()), value);
        }
    }
    serde_norway::to_string(&Value::Mapping(existing_map)).context("render merged hya config")
}

fn parse_opencode_model_config(raw: &str) -> anyhow::Result<OpencodeModelConfig> {
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

fn imported_opencode_providers(config: &OpencodeModelConfig) -> Vec<ImportedProvider> {
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
            kind: opencode_provider_kind(id, provider),
            base_url: base_url.to_string(),
            api_key: provider.options.api_key.clone(),
            models: models.into_iter().collect(),
        });
    }
    providers
}

fn opencode_provider_kind(id: &str, provider: &OpencodeProviderConfig) -> &'static str {
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

fn imported_default_model(config: &OpencodeModelConfig, providers: &[ImportedProvider]) -> String {
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

fn render_imported_hya_config(default_model: &str, providers: &[ImportedProvider]) -> String {
    let mut lines = vec![
        "# Generated by hya first-run OpenCode import.".to_string(),
        format!("default_model: {}", quote_yaml_scalar(default_model)),
        "providers:".to_string(),
    ];
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
    lines.push("mcp: {}".to_string());
    lines.push("plugins: {}".to_string());
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn quote_yaml_scalar(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
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

/// Flatten the file's providers into addressable routes, skipping any provider
/// that declares no models and resolving each `api_key` template.
fn resolve_providers(file: &FileConfig) -> anyhow::Result<Vec<ParsedProvider>> {
    let mut out = Vec::new();
    for (id, provider) in &file.providers {
        if provider.models.is_empty() {
            continue;
        }
        let api_key = provider
            .api_key
            .as_deref()
            .map(resolve_secret)
            .transpose()?;
        out.push(ParsedProvider {
            id: id.clone(),
            kind: provider.kind.into(),
            base_url: provider.base_url.clone(),
            api_key,
            models: provider.models.clone(),
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

fn model_entries(providers: &[ParsedProvider]) -> Vec<ModelEntry> {
    providers
        .iter()
        .flat_map(|provider| {
            let variants = provider.kind.reasoning_variants();
            provider.models.iter().map(move |model| ModelEntry {
                id: model.clone(),
                provider: provider.id.clone(),
                reasoning_variants: variants.clone(),
            })
        })
        .collect()
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
    limits
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

fn choose_default(file_default: Option<String>, models: &[ModelEntry]) -> String {
    if let Some(model) = file_default {
        return model;
    }
    if let Ok(model) = std::env::var("HYA_MODEL") {
        return model;
    }
    models
        .iter()
        .find(|m| m.id.contains("sonnet"))
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

/// Load hya's config into a ready router. `Ok(None)` means no usable config
/// (no file, empty, or no providers) — the caller should use the offline provider.
pub fn load() -> anyhow::Result<Option<ResolvedConfig>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if yaml.trim().is_empty() {
        return Ok(None);
    }
    let file = parse_config(&yaml)?;
    let mcp = resolve_mcp(&file)?;
    let parsed = resolve_providers(&file)?;
    if parsed.is_empty() && mcp.is_empty() && file.plugins.is_empty() {
        return Ok(None);
    }
    let mut router = ProviderRouter::new();
    let mut authorized = Vec::new();
    for p in parsed {
        let Some(api_key) = crate::auth::load_token(&p.id).or(p.api_key.clone()) else {
            continue;
        };
        if api_key.trim().is_empty() {
            continue;
        }
        let provider =
            HttpProvider::new(p.id.clone(), p.kind, &p.base_url, api_key, p.models.clone())?;
        router = router.with(Arc::new(provider));
        authorized.push(p);
    }
    let models = model_entries(&authorized);
    if models.is_empty() && mcp.is_empty() && file.plugins.is_empty() {
        return Ok(None);
    }
    let default_model = choose_default(file.default_model, &models);
    let subagents = resolve_subagent_limits(file.subagents.as_ref());
    Ok(Some(ResolvedConfig {
        router,
        default_model,
        has_providers: !models.is_empty(),
        models,
        mcp,
        default_agent: file.default_agent,
        plugins: file.plugins,
        subagents,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn subagent_limits_parse_from_file_and_env_wins() {
        // File block sets all three; a partial block keeps defaults elsewhere.
        let file = parse_config(
            "default_model: x\nsubagents:\n  max_depth: 9\n  max_concurrency: 200\n  per_run_budget: 1000\n",
        )
        .unwrap();
        let from_file = resolve_subagent_limits(file.subagents.as_ref());
        assert_eq!(from_file.max_depth, 9);
        assert_eq!(from_file.max_concurrency, 200);
        assert_eq!(from_file.per_run_budget, 1000);

        // Absent block → all defaults.
        let defaults = resolve_subagent_limits(None);
        assert_eq!(defaults, SubagentLimits::default());

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
    fn parses_providers_kinds_and_models() {
        let parsed = parse_providers(FIXTURE).unwrap();
        assert_eq!(parsed.len(), 3, "providers without models are skipped");
        let oai = parsed.iter().find(|p| p.id == "gw-oai").unwrap();
        assert_eq!(oai.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(oai.base_url, "https://gw.example/v1");
        assert_eq!(oai.api_key.as_deref(), Some("sk-test-literal"));
        assert!(oai.models.contains(&"gpt-5.5".to_string()));
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
    fn model_entries_include_provider_reasoning_variants() {
        let parsed = parse_providers(FIXTURE).unwrap();

        let entries = model_entries(&parsed);

        let openai = entries
            .iter()
            .find(|entry| entry.provider == "gw-oai")
            .unwrap();
        assert_eq!(
            openai.reasoning_variants,
            vec!["minimal", "low", "medium", "high", "xhigh"]
        );
        let anthropic = entries
            .iter()
            .find(|entry| entry.provider == "gw-anth")
            .unwrap();
        assert_eq!(
            anthropic.reasoning_variants,
            vec!["low", "medium", "high", "max"]
        );
        let google = entries
            .iter()
            .find(|entry| entry.provider == "gw-google")
            .unwrap();
        assert_eq!(google.reasoning_variants, vec!["high", "max"]);
    }

    #[test]
    fn openai_compatible_alias_is_accepted() {
        let yaml = "
providers:
  gw:
    kind: openai-compatible
    base_url: https://gw.example/v1
    models: [m1]
";
        let parsed = parse_providers(yaml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, ProviderKind::OpenAiCompatible);
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
    fn explicit_default_model_wins() {
        let models: Vec<ModelEntry> = Vec::new();
        assert_eq!(
            choose_default(Some("gpt-5.5".to_string()), &models),
            "gpt-5.5"
        );
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
  opencode:
    kind: opencode
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
            file.plugins.get("opencode").unwrap().kind,
            hya_plugin::messages::PluginKindWire::Opencode
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
    fn import_opencode_models_writes_hya_provider_config() {
        let dir =
            std::env::temp_dir().join(format!("hya-opencode-model-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opencode = dir.join("opencode.json");
        let hya_config = dir.join("hya/config.yaml");

        std::fs::write(
            &opencode,
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

        let summary = import_opencode_models_into_config(&opencode, &hya_config).unwrap();

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
        assert_eq!(gateway.models, vec!["gpt-5.4", "gpt-5.5"]);
        let anthropic = file.providers.get("anthropic").unwrap();
        assert!(matches!(anthropic.kind, ProviderKindConfig::Anthropic));
        assert!(!text.contains("disabled-model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_opencode_models_preserves_existing_non_model_config() {
        let dir = std::env::temp_dir().join(format!(
            "hya-opencode-model-import-merge-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opencode = dir.join("opencode.json");
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
            &opencode,
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

        let summary = import_opencode_models_into_config(&opencode, &hya_config).unwrap();

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
}
