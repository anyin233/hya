//! Build a provider router from opencode's config, reusing its providers + keys.
//!
//! Reads `~/.config/opencode/opencode.json`: each `provider.<id>` maps to one
//! `HttpProvider` (route chosen by `npm`), and the union of `models` keys becomes
//! the set yaca can address. Absent config or key → caller falls back to offline.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use yaca_provider::{HttpProvider, ProviderKind, ProviderRouter};

pub struct ResolvedConfig {
    pub router: ProviderRouter,
    pub default_model: String,
}

struct ParsedProvider {
    id: String,
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    models: Vec<String>,
}

fn opencode_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir).join("opencode/opencode.json");
        if path.exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/opencode/opencode.json");
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

fn parse_providers(config_json: &str) -> anyhow::Result<Vec<ParsedProvider>> {
    let root: serde_json::Value =
        serde_json::from_str(config_json).context("parse opencode.json")?;
    let mut out = Vec::new();
    let Some(providers) = root.get("provider").and_then(serde_json::Value::as_object) else {
        return Ok(out);
    };
    for (id, pv) in providers {
        let npm = pv
            .get("npm")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let kind = if npm.contains("anthropic") {
            ProviderKind::Anthropic
        } else if npm.contains("google") {
            ProviderKind::Google
        } else if npm.contains("openai") {
            ProviderKind::OpenAiCompatible
        } else {
            continue;
        };
        let options = pv.get("options");
        let Some(base_url) = options
            .and_then(|o| o.get("baseURL"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let api_key = options
            .and_then(|o| o.get("apiKey"))
            .and_then(serde_json::Value::as_str)
            .map(resolve_secret)
            .transpose()?;
        let models: Vec<String> = pv
            .get("models")
            .and_then(serde_json::Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        if models.is_empty() {
            continue;
        }
        out.push(ParsedProvider {
            id: id.clone(),
            kind,
            base_url: base_url.to_string(),
            api_key,
            models,
        });
    }
    Ok(out)
}

fn choose_default(models: &[String]) -> String {
    if let Ok(model) = std::env::var("YACA_MODEL") {
        return model;
    }
    models
        .iter()
        .find(|m| m.contains("sonnet"))
        .or_else(|| models.first())
        .cloned()
        .unwrap_or_default()
}

/// Load opencode's config into a ready router. `Ok(None)` means no usable config
/// (no file or no providers) — the caller should use the offline provider.
pub fn load() -> anyhow::Result<Option<ResolvedConfig>> {
    let Some(path) = opencode_config_path() else {
        return Ok(None);
    };
    let json =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed = parse_providers(&json)?;
    if parsed.is_empty() {
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
        models.extend(p.models.iter().cloned());
        let provider = HttpProvider::new(p.id, p.kind, &p.base_url, api_key, p.models)?;
        router = router.with(Arc::new(provider));
    }
    if models.is_empty() {
        return Ok(None);
    }
    let default_model = choose_default(&models);
    Ok(Some(ResolvedConfig {
        router,
        default_model,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const FIXTURE: &str = r#"{
        "$schema": "x",
        "provider": {
            "gw-oai": {
                "npm": "@ai-sdk/openai-compatible",
                "options": { "apiKey": "sk-test-literal", "baseURL": "https://gw.example/v1" },
                "models": { "gpt-5.5": {}, "gpt-5.4": {} }
            },
            "gw-anth": {
                "npm": "@ai-sdk/anthropic",
                "options": { "apiKey": "sk-test-literal", "baseURL": "https://gw.example/v1" },
                "models": { "claude-sonnet-4-6": {} }
            },
            "gw-google": {
                "npm": "@ai-sdk/google",
                "options": { "apiKey": "sk-test-literal", "baseURL": "https://gl.googleapis.com/v1beta" },
                "models": { "gemini-2.0-flash": {} }
            },
            "no-models": {
                "npm": "@ai-sdk/openai-compatible",
                "options": { "apiKey": "x", "baseURL": "https://y/v1" }
            }
        }
    }"#;

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
        let json = r#"{
            "provider": {
                "12th": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": { "baseURL": "https://api.example/v1" },
                    "models": { "claude-sonnet-4-6": {} }
                }
            }
        }"#;
        let parsed = parse_providers(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].api_key, None);
        assert_eq!(parsed[0].base_url, "https://api.example/v1");
    }
}
