//! Fetch provider model catalogs after OAuth login.

use crate::auth::OAuthType;

use super::{OAuthError, json_str};

/// One model entry suitable for writing into `config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    pub reasoning_default: Option<String>,
    pub reasoning_variants: Vec<String>,
}

/// Client version query for Codex `/codex/models` (empty list for unknown versions).
const CODEX_MODELS_CLIENT_VERSION: &str = "0.144.0";

/// Fetch models for an OAuth provider using the just-obtained access token.
///
/// Network failures bubble as `OAuthError::Network` / `Protocol`. Callers may
/// fall back to a single default model when this returns an error.
pub async fn fetch_oauth_models(
    oauth_type: OAuthType,
    access_token: &str,
    account_id: Option<&str>,
    base_url: &str,
) -> Result<Vec<CatalogModel>, OAuthError> {
    match oauth_type {
        OAuthType::OpenaiCodex => fetch_openai_codex_models(access_token, account_id).await,
        OAuthType::GrokBuild => fetch_openai_compatible_models(base_url, access_token, true).await,
    }
}

async fn fetch_openai_codex_models(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<Vec<CatalogModel>, OAuthError> {
    let client = http_client()?;
    let url = format!(
        "https://chatgpt.com/backend-api/codex/models?client_version={}",
        urlencoding(CODEX_MODELS_CLIENT_VERSION)
    );
    let mut req = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("OpenAI-Beta", "responses=experimental")
        .header("User-Agent", "codex_cli_rs");
    if let Some(account_id) = account_id.filter(|s| !s.is_empty()) {
        req = req.header("ChatGPT-Account-Id", account_id);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(OAuthError::Protocol(format!(
            "codex models list failed with {status}: {}",
            body.get(..400).unwrap_or(body.as_str())
        )));
    }
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| OAuthError::Protocol(format!("codex models JSON: {e}")))?;
    let models = parse_codex_models_catalog(&json)?;
    if models.is_empty() {
        return Err(OAuthError::Protocol(
            "codex models list returned no models".into(),
        ));
    }
    Ok(models)
}

async fn fetch_openai_compatible_models(
    base_url: &str,
    access_token: &str,
    grok_session: bool,
) -> Result<Vec<CatalogModel>, OAuthError> {
    let client = http_client()?;
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/models");
    let mut req = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {access_token}"));
    if grok_session {
        req = req
            .header("x-xai-token-auth", "xai-grok-cli")
            .header("x-grok-client-version", env!("CARGO_PKG_VERSION"))
            .header("x-grok-client-identifier", "grok-cli");
    }
    let resp = req
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(OAuthError::Protocol(format!(
            "models list failed with {status}: {}",
            body.get(..400).unwrap_or(body.as_str())
        )));
    }
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| OAuthError::Protocol(format!("models JSON: {e}")))?;
    let models = parse_openai_list_catalog(&json)?;
    if models.is_empty() {
        return Err(OAuthError::Protocol(
            "models list returned no models".into(),
        ));
    }
    Ok(models)
}

/// Parse Codex `GET /backend-api/codex/models` body (`{ "models": [ { "slug": ... } ] }`).
pub(crate) fn parse_codex_models_catalog(
    payload: &serde_json::Value,
) -> Result<Vec<CatalogModel>, OAuthError> {
    let Some(arr) = payload.get("models").and_then(|v| v.as_array()) else {
        return Err(OAuthError::Protocol(
            "codex models response missing models array".into(),
        ));
    };
    let mut out = Vec::new();
    for item in arr {
        let id = json_str(item, "slug")
            .or_else(|| json_str(item, "id"))
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            continue;
        }
        let (default, variants) = parse_codex_reasoning(item);
        out.push(CatalogModel {
            id: id.to_string(),
            reasoning_default: default,
            reasoning_variants: variants,
        });
    }
    Ok(out)
}

fn parse_codex_reasoning(item: &serde_json::Value) -> (Option<String>, Vec<String>) {
    let default = json_str(item, "default_reasoning_level")
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let variants = item
        .get("supported_reasoning_levels")
        .and_then(|v| v.as_array())
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    json_str(level, "effort")
                        .or_else(|| json_str(level, "id"))
                        .map(str::to_string)
                        .filter(|s| !s.is_empty())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (default, variants)
}

/// Parse OpenAI-compatible `GET /models` body (`{ "data": [ { "id": ... } ] }`).
pub(crate) fn parse_openai_list_catalog(
    payload: &serde_json::Value,
) -> Result<Vec<CatalogModel>, OAuthError> {
    let arr = payload
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| payload.get("models").and_then(|v| v.as_array()))
        .ok_or_else(|| OAuthError::Protocol("models response missing data array".into()))?;
    let mut out = Vec::new();
    for item in arr {
        let id = if let Some(s) = item.as_str() {
            s.trim().to_string()
        } else {
            json_str(item, "id")
                .or_else(|| json_str(item, "model"))
                .or_else(|| json_str(item, "name"))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        if id.is_empty() {
            continue;
        }
        // Skip pure media models for the agent catalog when clearly tagged.
        if id.contains("imagine") || id.contains("image") || id.contains("video") {
            continue;
        }
        let (default, variants) = parse_grok_reasoning(item);
        out.push(CatalogModel {
            id,
            reasoning_default: default,
            reasoning_variants: variants,
        });
    }
    Ok(out)
}

fn parse_grok_reasoning(item: &serde_json::Value) -> (Option<String>, Vec<String>) {
    let default = json_str(item, "reasoning_effort")
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let variants = item
        .get("reasoning_efforts")
        .and_then(|v| v.as_array())
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    json_str(level, "value")
                        .or_else(|| json_str(level, "id"))
                        .map(str::to_string)
                        .filter(|s| !s.is_empty())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (default, variants)
}

fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| OAuthError::Network(e.to_string()))
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_catalog_with_reasoning() {
        let payload = serde_json::json!({
            "models": [
                {
                    "slug": "gpt-5.6-sol",
                    "default_reasoning_level": "low",
                    "supported_reasoning_levels": [
                        {"effort": "low"},
                        {"effort": "medium"},
                        {"effort": "high"}
                    ]
                },
                {"slug": "gpt-5.3-codex-spark"}
            ]
        });
        let models = parse_codex_models_catalog(&payload).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert_eq!(models[0].reasoning_default.as_deref(), Some("low"));
        assert_eq!(models[0].reasoning_variants, vec!["low", "medium", "high"]);
        assert_eq!(models[1].id, "gpt-5.3-codex-spark");
        assert!(models[1].reasoning_variants.is_empty());
    }

    #[test]
    fn parses_openai_list_and_skips_media() {
        let payload = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "grok-4.5", "reasoning_effort": "high", "reasoning_efforts": [
                    {"id": "high", "value": "high"},
                    {"id": "medium", "value": "medium"}
                ]},
                {"id": "grok-imagine-image"},
                {"id": "grok-build-0.1"}
            ]
        });
        let models = parse_openai_list_catalog(&payload).unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["grok-4.5", "grok-build-0.1"]);
        assert_eq!(models[0].reasoning_default.as_deref(), Some("high"));
        assert_eq!(models[0].reasoning_variants, vec!["high", "medium"]);
    }
}
