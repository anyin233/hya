//! Build a provider router from yaca's own config (`~/.config/yaca/config.yaml`).
//!
//! Reads YAML: each entry under `providers.<id>` maps to one `HttpProvider`
//! (route chosen by `kind`), and the union of `models` becomes the set yaca can
//! address. API keys come from `~/.config/yaca/auth/<id>.yaml` (via `yaca login`)
//! or an inline `api_key`. Absent config or key → caller falls back to offline.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use yaca_mcp::McpServerConfig;
use yaca_plugin::config::PluginEntry;
use yaca_provider::{HttpProvider, ProviderKind, ProviderRouter};

pub struct ResolvedConfig {
    pub router: ProviderRouter,
    pub default_model: String,
    pub models: Vec<ModelEntry>,
    pub has_providers: bool,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
}

/// Top-level shape of `~/.config/yaca/config.yaml`.
#[derive(Debug, Deserialize)]
struct FileConfig {
    /// Model used when neither `--model` nor `YACA_MODEL` is set.
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    mcp: BTreeMap<String, McpServerConfig>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfig {
    kind: ProviderKindConfig,
    base_url: String,
    /// Literal, `{env:VAR}`, or `{file:path}`. Optional — a token saved via
    /// `yaca login` (`~/.config/yaca/auth/<id>.yaml`) takes precedence.
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

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir).join("yaca/config.yaml");
        if path.exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/yaca/config.yaml");
    path.exists().then_some(path)
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
    serde_norway::from_str(yaml).context("parse yaca config.yaml")
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

fn choose_default(file_default: Option<String>, models: &[ModelEntry]) -> String {
    if let Some(model) = file_default {
        return model;
    }
    if let Ok(model) = std::env::var("YACA_MODEL") {
        return model;
    }
    models
        .iter()
        .find(|m| m.id.contains("sonnet"))
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

/// Load yaca's config into a ready router. `Ok(None)` means no usable config
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
    let mut models = Vec::new();
    for p in parsed {
        let Some(api_key) = crate::auth::load_token(&p.id).or(p.api_key) else {
            continue;
        };
        if api_key.trim().is_empty() {
            continue;
        }
        for m in &p.models {
            models.push(ModelEntry {
                id: m.clone(),
                provider: p.id.clone(),
            });
        }
        let provider = HttpProvider::new(p.id, p.kind, &p.base_url, api_key, p.models)?;
        router = router.with(Arc::new(provider));
    }
    if models.is_empty() && mcp.is_empty() && file.plugins.is_empty() {
        return Ok(None);
    }
    let default_model = choose_default(file.default_model, &models);
    Ok(Some(ResolvedConfig {
        router,
        default_model,
        has_providers: !models.is_empty(),
        models,
        mcp,
        plugins: file.plugins,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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
        unsafe { std::env::set_var("YACA_TEST_KEY_XYZ", "resolved-secret") };
        assert_eq!(
            resolve_secret("{env:YACA_TEST_KEY_XYZ}").unwrap(),
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
            yaca_plugin::messages::PluginKindWire::Opencode
        );
    }
}
