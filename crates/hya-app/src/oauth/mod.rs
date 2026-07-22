//! Interactive OAuth login and token refresh for subscription providers.
//!
//! Supported types: `openai-codex` (ChatGPT/Codex) and `grok-build` (xAI CLI).

mod callback;
mod ensure;
mod grok_build;
mod models_catalog;
mod openai_codex;
mod pkce;

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::auth::{AuthCredential, OAuthType};
use crate::config::{self, OAuthConfigModel};

pub use ensure::{ensure_access_token, ensure_access_token_in, oauth_status_in};
pub use grok_build::login_grok_build;
pub use models_catalog::{CatalogModel, fetch_oauth_models};
pub use openai_codex::{login_openai_codex, login_openai_codex_device};

/// Errors from OAuth login, refresh, or credential loading.
#[derive(Debug, Error)]
pub enum OAuthError {
    #[error(
        "OAuth credentials for provider '{provider}' require re-login ({reason}). Run: hya-backend oauth login --provider {provider} --type {oauth_type}"
    )]
    NeedsLogin {
        provider: String,
        oauth_type: String,
        reason: String,
    },
    #[error(
        "provider '{provider}' OAuth grant is valid but not entitled for API access ({detail}). Re-login will not help; use an API key path or upgrade the subscription."
    )]
    Entitlement { provider: String, detail: String },
    #[error("OAuth network error: {0}")]
    Network(String),
    #[error("OAuth protocol error: {0}")]
    Protocol(String),
    #[error("OAuth timed out: {0}")]
    Timeout(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
}

impl OAuthError {
    /// Build a re-login error for a known OAuth type.
    pub fn needs_login(
        provider: impl Into<String>,
        oauth_type: OAuthType,
        reason: impl Into<String>,
    ) -> Self {
        Self::NeedsLogin {
            provider: provider.into(),
            oauth_type: oauth_type.as_str().to_string(),
            reason: reason.into(),
        }
    }
}

/// Options for `oauth login`.
#[derive(Debug, Clone)]
pub struct OAuthLoginOptions {
    pub provider: String,
    pub oauth_type: OAuthType,
    /// Prefer device-code (default for openai-codex and grok-build).
    ///
    /// For openai-codex, device-code is the Codex CLI default (no local callback).
    /// Set `loopback` to use localhost PKCE instead.
    pub device: bool,
    /// Use localhost PKCE callback for openai-codex instead of device-code.
    pub loopback: bool,
    /// Do not attempt to open a system browser (print URL only).
    /// Default for openai-codex device login is true (Codex-style no-browser).
    pub no_browser: bool,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Override auth directory (tests).
    pub auth_dir: Option<PathBuf>,
    /// Override config path (tests).
    pub config_path: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for OAuthLoginOptions {
    fn default() -> Self {
        Self {
            provider: String::new(),
            oauth_type: OAuthType::GrokBuild,
            device: false,
            loopback: false,
            no_browser: false,
            model: None,
            base_url: None,
            auth_dir: None,
            config_path: None,
            timeout: Duration::from_secs(600),
        }
    }
}

/// Result of a successful OAuth login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthLoginResult {
    pub provider: String,
    pub oauth_type: OAuthType,
    pub auth_path: PathBuf,
    pub config_path: PathBuf,
    pub base_url: String,
    pub model: String,
    /// Models written into `config.yaml` (catalog fetch or single default).
    pub models: Vec<String>,
    /// True when models came from a live catalog fetch.
    pub models_from_catalog: bool,
}

/// Run the interactive OAuth login for the requested provider type (all Rust).
pub async fn login(options: OAuthLoginOptions) -> Result<OAuthLoginResult, OAuthError> {
    let provider = options.provider.trim();
    if provider.is_empty() {
        return Err(OAuthError::Protocol(
            "provider name must not be empty".into(),
        ));
    }
    validate_provider_id(provider)?;

    let credential = match options.oauth_type {
        OAuthType::OpenaiCodex => {
            // Codex CLI default: device-code, print URL/code, no local callback.
            // Loopback PKCE is opt-in via `loopback`.
            let use_device = !options.loopback;
            if use_device {
                // Device path defaults to no auto-open unless caller set no_browser=false
                // and did not force loopback — open only when explicitly allowed.
                login_openai_codex_device(options.timeout, !options.no_browser).await?
            } else {
                login_openai_codex(options.timeout, !options.no_browser).await?
            }
        }
        OAuthType::GrokBuild => login_grok_build(options.timeout, !options.no_browser).await?,
    };

    let auth_dir = options
        .auth_dir
        .clone()
        .or_else(crate::auth::auth_dir)
        .ok_or_else(|| {
            OAuthError::Config("no config directory (set HOME or XDG_CONFIG_HOME)".into())
        })?;
    let auth_cred = AuthCredential::OAuth(credential.clone());
    crate::auth::save_credential_in(&auth_dir, provider, &auth_cred)?;
    let auth_path = auth_dir.join(format!("{provider}.yaml"));

    let base_url = options
        .base_url
        .clone()
        .unwrap_or_else(|| options.oauth_type.default_base_url().to_string());
    let fallback_model = options
        .model
        .clone()
        .unwrap_or_else(|| options.oauth_type.default_model().to_string());
    let config_path = options
        .config_path
        .clone()
        .unwrap_or_else(config::expected_config_path);

    // Prefer a live catalog so config.yaml lists every entitled model.
    let (catalog_models, models_from_catalog) = match fetch_oauth_models(
        options.oauth_type,
        &credential.access_token,
        credential.account_id.as_deref(),
        &base_url,
    )
    .await
    {
        Ok(models) if !models.is_empty() => (models, true),
        Ok(_) => {
            eprintln!("hya: model catalog was empty; writing default model only");
            (Vec::new(), false)
        }
        Err(err) => {
            eprintln!("hya: could not fetch model catalog ({err}); writing default model only");
            (Vec::new(), false)
        }
    };

    let config_models: Vec<OAuthConfigModel> = if catalog_models.is_empty() {
        vec![OAuthConfigModel {
            id: fallback_model.clone(),
            reasoning_default: None,
            reasoning_variants: Vec::new(),
        }]
    } else {
        catalog_models
            .iter()
            .map(|m| OAuthConfigModel {
                id: m.id.clone(),
                reasoning_default: m.reasoning_default.clone(),
                reasoning_variants: m.reasoning_variants.clone(),
            })
            .collect()
    };
    let model = config_models
        .first()
        .map(|m| m.id.clone())
        .unwrap_or(fallback_model);
    let model_ids: Vec<String> = config_models.iter().map(|m| m.id.clone()).collect();

    config::upsert_oauth_provider(
        &config_path,
        provider,
        options.oauth_type.provider_kind(),
        &base_url,
        &config_models,
        &model,
    )
    .map_err(|e| OAuthError::Config(e.to_string()))?;

    Ok(OAuthLoginResult {
        provider: provider.to_string(),
        oauth_type: options.oauth_type,
        auth_path,
        config_path,
        base_url,
        model,
        models: model_ids,
        models_from_catalog,
    })
}

fn validate_provider_id(provider: &str) -> Result<(), OAuthError> {
    if provider.contains('/')
        || provider.contains('\\')
        || provider.contains("..")
        || provider.contains(char::is_whitespace)
    {
        return Err(OAuthError::Protocol(format!(
            "invalid provider id '{provider}'"
        )));
    }
    Ok(())
}

/// Format an RFC3339 UTC expiry from `expires_in` seconds.
pub(crate) fn expires_at_from_secs(expires_in: i64) -> Result<String, OAuthError> {
    if expires_in <= 0 {
        return Err(OAuthError::Protocol(
            "token response missing or non-positive expires_in".into(),
        ));
    }
    let now = time::OffsetDateTime::now_utc();
    let expires = now + time::Duration::seconds(expires_in);
    expires
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| OAuthError::Protocol(e.to_string()))
}

/// Decode a JWT payload without verifying the signature (claim extraction only).
pub(crate) fn decode_jwt_claims(token: &str) -> serde_json::Value {
    let mut parts = token.split('.');
    let (_hdr, payload, _sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => (h, p, s),
        _ => return serde_json::Value::Null,
    };
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        // Some JWTs include padding.
        let Ok(bytes) = base64::engine::general_purpose::URL_SAFE.decode(payload) else {
            return serde_json::Value::Null;
        };
        return serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    };
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Derive RFC3339 expiry from a JWT `exp` claim.
pub(crate) fn expires_at_from_jwt(access_token: &str) -> Result<String, OAuthError> {
    let claims = decode_jwt_claims(access_token);
    let exp = claims
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| OAuthError::Protocol("access token JWT missing exp claim".into()))?;
    let expires = time::OffsetDateTime::from_unix_timestamp(exp)
        .map_err(|e| OAuthError::Protocol(e.to_string()))?;
    expires
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| OAuthError::Protocol(e.to_string()))
}

/// Parse stored RFC3339 expiry.
pub(crate) fn parse_expires_at(raw: &str) -> Result<time::OffsetDateTime, OAuthError> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_err(|e| OAuthError::Protocol(format!("invalid expires_at '{raw}': {e}")))
}

/// True when the access token should be refreshed (within `skew` of expiry).
pub(crate) fn is_expired(expires_at: &str, skew: Duration) -> Result<bool, OAuthError> {
    let expires = parse_expires_at(expires_at)?;
    let skew = time::Duration::try_from(skew)
        .map_err(|_| OAuthError::Protocol("invalid refresh skew".into()))?;
    let threshold = expires - skew;
    Ok(time::OffsetDateTime::now_utc() >= threshold)
}

/// Best-effort open URL in a system browser.
pub(crate) fn open_browser(url: &str) {
    let cmds: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["open", url]]
    } else if cfg!(target_os = "windows") {
        &[&["cmd", "/C", "start", "", url]]
    } else {
        &[&["xdg-open", url], &["gio", "open", url]]
    };
    for cmd in cmds {
        if std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

/// Shared form POST helper for OAuth endpoints.
pub(crate) async fn post_form(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<serde_json::Value, OAuthError> {
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(form)
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| serde_json::json!({ "error": "non_json", "error_description": body }));
    if status.is_success() {
        return Ok(json);
    }
    let error = json
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_error");
    let description = json
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or(body.as_str());
    Err(OAuthError::Protocol(format!(
        "{url} returned {status}: {error}: {description}"
    )))
}

/// Extract string field from JSON object.
pub(crate) fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn jwt_exp_to_expires_at() {
        // header.payload.sig — payload is {"exp":1893456000} (2030-01-01)
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"exp":1893456000,"sub":"u"}"#);
        let jwt = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
        let expires = expires_at_from_jwt(&jwt).unwrap();
        assert!(expires.starts_with("2030-01-01"));
    }

    #[test]
    fn expires_in_positive() {
        let s = expires_at_from_secs(3600).unwrap();
        assert!(parse_expires_at(&s).is_ok());
    }
}
