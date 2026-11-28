//! Interactive OAuth login and token refresh for subscription providers.
//!
//! Each OAuth type is a [`hya_provider::ProviderAuthFlow`] registered with its
//! provider kind; this module runs a flow's login and stores the credential.

mod callback;
mod ensure;
mod grok_build;
mod models_catalog;
mod openai_codex;
mod pkce;

use std::path::PathBuf;
use std::time::Duration;

use hya_provider::{OAuthLoginRequest, ProviderKinds};

use crate::auth::AuthCredential;
use crate::config;

pub use ensure::{
    ensure_access_token, ensure_access_token_in, force_refresh_access_token,
    force_refresh_access_token_in, oauth_status_in,
};
pub(crate) use grok_build::GrokAuthFlow;
pub use models_catalog::fetch_oauth_models;
pub(crate) use openai_codex::CodexAuthFlow;

pub use hya_provider::OAuthError;

/// Options for `oauth login`.
#[derive(Debug, Clone)]
pub struct OAuthLoginOptions {
    /// Provider id written under `providers.<id>` and `auth/<id>.yaml`.
    pub provider: String,
    /// OAuth type of the registered flow to run (`--type`).
    pub oauth_type: String,
    /// Use a localhost callback instead of the flow's default (`--loopback`).
    pub loopback: bool,
    /// `--browser` (`Some(true)`) or `--no-browser` (`Some(false)`); `None`
    /// keeps the flow's default.
    pub browser: Option<bool>,
    /// Optional model id to register on the provider after login.
    pub model: Option<String>,
    /// Optional inference base URL override for the provider route.
    pub base_url: Option<String>,
    /// Override auth directory (tests).
    pub auth_dir: Option<PathBuf>,
    /// Override config path (tests).
    pub config_path: Option<PathBuf>,
    /// Overall interactive flow deadline.
    pub timeout: Duration,
}

/// Result of a successful OAuth login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthLoginResult {
    /// Provider id that was written.
    pub provider: String,
    /// OAuth type of the flow that completed.
    pub oauth_type: String,
    /// Config `kind` written for the provider.
    pub kind: String,
    /// Path of the saved auth credential file.
    pub auth_path: PathBuf,
    /// Path of the updated `config.yaml`.
    pub config_path: PathBuf,
    /// Inference base URL registered for the provider.
    pub base_url: String,
    /// Preferred model for display after login; never persisted by login.
    pub model: String,
    /// Live catalog rows shown to the caller; never written into `config.yaml`.
    pub models: Vec<String>,
    /// True when `models` came from a live catalog fetch.
    pub models_from_catalog: bool,
}

/// Run the interactive login of the flow `kinds` registers for
/// `options.oauth_type`, save the credential, and declare the provider.
pub async fn login(
    options: OAuthLoginOptions,
    kinds: &ProviderKinds,
) -> Result<OAuthLoginResult, OAuthError> {
    let provider = options.provider.trim();
    if provider.is_empty() {
        return Err(OAuthError::Protocol(
            "provider name must not be empty".into(),
        ));
    }
    validate_provider_id(provider)?;
    let flow = kinds.auth_flow(&options.oauth_type).ok_or_else(|| {
        let supported = kinds
            .labels()
            .filter(|label| kinds.supports_oauth(label))
            .collect::<Vec<_>>()
            .join(", ");
        OAuthError::Protocol(format!(
            "unknown OAuth type `{}`; supported: {supported}",
            options.oauth_type
        ))
    })?;
    // `ProviderKinds` maps an OAuth type to its flow only; the flows
    // registered today are named after the kind they log in to.
    let protocol = kinds.protocol(flow.oauth_type()).cloned().ok_or_else(|| {
        OAuthError::Config(format!(
            "OAuth type `{}` has no provider kind of the same name",
            flow.oauth_type()
        ))
    })?;
    if options.loopback && !flow.supports_loopback() {
        return Err(OAuthError::Protocol(format!(
            "--loopback is not supported for --type {}",
            flow.oauth_type()
        )));
    }

    let credential = flow
        .login(OAuthLoginRequest {
            loopback: options.loopback,
            browser: options.browser,
            timeout: options.timeout,
        })
        .await?;

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
        .unwrap_or_else(|| flow.default_base_url().to_string());
    let preferred_model = options
        .model
        .clone()
        .unwrap_or_else(|| flow.default_model().to_string());
    let config_path = options
        .config_path
        .clone()
        .unwrap_or_else(config::expected_config_path);

    // A login-time fetch may improve the confirmation output, but it is never
    // persisted. An empty provider list remains eligible for startup discovery.
    let (model_ids, models_from_catalog) = match fetch_oauth_models(
        std::sync::Arc::clone(&protocol),
        &credential.access_token,
        credential.account_id.as_deref(),
        &base_url,
    )
    .await
    {
        Ok(models) if !models.is_empty() => (
            models.into_iter().map(|model| model.id).collect::<Vec<_>>(),
            true,
        ),
        Ok(_) => {
            eprintln!("hya: model catalog was empty; startup will discover it again");
            (Vec::new(), false)
        }
        Err(err) => {
            eprintln!("hya: could not fetch model catalog ({err}); startup will retry discovery");
            (Vec::new(), false)
        }
    };
    let model = model_ids.first().cloned().unwrap_or(preferred_model);

    let kind = protocol.kind().to_string();
    config::upsert_oauth_provider(&config_path, provider, &kind, &base_url)
        .map_err(|e| OAuthError::Config(e.to_string()))?;

    Ok(OAuthLoginResult {
        provider: provider.to_string(),
        oauth_type: flow.oauth_type().to_string(),
        kind,
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
