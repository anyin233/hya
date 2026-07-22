//! OpenAI Codex / ChatGPT subscription OAuth (PKCE + device-code).
//!
//! Device-code flow matches the official Codex CLI
//! (`codex-rs/login/src/device_code_auth.rs`):
//! - JSON POST to `/api/accounts/deviceauth/usercode` with only `client_id`
//! - response fields: `device_auth_id`, `user_code`, string `interval`
//! - verification URL is fixed: `{issuer}/codex/device`
//! - poll JSON POST with `device_auth_id` + `user_code`
//! - pending is HTTP 403 (not OAuth `authorization_pending`)
//! - success returns `authorization_code` **and** server-issued PKCE pair

use std::time::Duration;

use crate::auth::{OAuthCredential, OAuthType};

use super::callback::wait_for_callback;
use super::pkce::{generate_pkce_pair, generate_state};
use super::{
    OAuthError, decode_jwt_claims, expires_at_from_secs, json_str, open_browser, post_form,
};

pub(crate) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Codex OAuth issuer (`auth.openai.com`).
pub(crate) const ISSUER: &str = "https://auth.openai.com";
pub(crate) const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(crate) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Official device-auth API base (Codex CLI uses `{issuer}/api/accounts`).
pub(crate) const DEVICE_API_BASE: &str = "https://auth.openai.com/api/accounts";
pub(crate) const DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(crate) const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const SCOPE: &str = "openid profile email offline_access";
pub(crate) const AUTH_CLAIMS_NS: &str = "https://api.openai.com/auth";

/// Codex CLI: `{issuer}/deviceauth/callback`.
#[must_use]
pub(crate) fn device_redirect_uri() -> String {
    format!("{ISSUER}/deviceauth/callback")
}

/// Codex CLI: `{issuer}/codex/device`.
#[must_use]
pub(crate) fn device_verification_url() -> String {
    format!("{ISSUER}/codex/device")
}
const DEFAULT_REDIRECT_HOST: &str = "localhost";
const DEFAULT_REDIRECT_PORT: u16 = 1455;
const DEFAULT_REDIRECT_PATH: &str = "/auth/callback";

/// Browser loopback PKCE login for openai-codex.
pub async fn login_openai_codex(
    timeout: Duration,
    open: bool,
) -> Result<OAuthCredential, OAuthError> {
    let redirect_uri =
        format!("http://{DEFAULT_REDIRECT_HOST}:{DEFAULT_REDIRECT_PORT}{DEFAULT_REDIRECT_PATH}");
    let state = generate_state();
    let (verifier, challenge) = generate_pkce_pair();
    let authorize_url = build_authorize_url(&redirect_uri, &state, &challenge);

    println!(
        "\nChatGPT / Codex sign-in: open the following URL in a browser:\n  {authorize_url}\n"
    );
    println!("Waiting for callback on {redirect_uri} ...");
    if open {
        open_browser(&authorize_url);
    }

    let params = tokio::task::spawn_blocking(move || {
        wait_for_callback(
            DEFAULT_REDIRECT_HOST,
            DEFAULT_REDIRECT_PORT,
            DEFAULT_REDIRECT_PATH,
            timeout,
        )
    })
    .await
    .map_err(|e| OAuthError::Protocol(e.to_string()))??;

    if params.get("state").map(String::as_str) != Some(state.as_str()) {
        return Err(OAuthError::Protocol(
            "OAuth callback state mismatch (possible CSRF)".into(),
        ));
    }
    if let Some(err) = params.get("error") {
        let desc = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("");
        return Err(OAuthError::Protocol(format!(
            "authorization error: {err} {desc}"
        )));
    }
    let code = params
        .get("code")
        .ok_or_else(|| OAuthError::Protocol("callback missing authorization code".into()))?;

    let client = http_client()?;
    let token_json = post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", &verifier),
        ],
    )
    .await?;
    credential_from_token_response(&token_json, None)
}

/// Device-code login for openai-codex (Codex CLI default / no-browser path).
///
/// Matches official Codex CLI field names and JSON bodies — not RFC 8628 form
/// encoding.
pub async fn login_openai_codex_device(
    timeout: Duration,
    open: bool,
) -> Result<OAuthCredential, OAuthError> {
    let client = http_client()?;
    // Codex only sends client_id; PKCE is issued by the server on poll success.
    let start = post_json(
        &client,
        &device_code_url(),
        &serde_json::json!({ "client_id": CLIENT_ID }),
    )
    .await?;

    let device_auth_id = json_str(&start, "device_auth_id")
        .ok_or_else(|| {
            OAuthError::Protocol(
                "device usercode response missing device_auth_id (is the body JSON?)".into(),
            )
        })?
        .to_string();
    let user_code = json_str(&start, "user_code")
        .or_else(|| json_str(&start, "usercode"))
        .ok_or_else(|| OAuthError::Protocol("device usercode response missing user_code".into()))?
        .to_string();
    let interval = parse_interval(&start).unwrap_or(5).max(1);
    let poll_interval = Duration::from_secs(interval);
    let verification = device_verification_url();
    let redirect_uri = device_redirect_uri();

    println!("\nChatGPT / Codex sign-in (device code — Codex default):\n");
    println!("1. Open this link in your browser and sign in:");
    println!("   {verification}");
    println!("2. Enter this one-time code (expires in ~15 minutes):");
    println!("   {user_code}");
    println!("\nWaiting for authorization...\n");
    if open {
        open_browser(&verification);
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let code_success = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(OAuthError::Timeout(
                "timed out waiting for ChatGPT device authorization".into(),
            ));
        }
        let poll = client
            .post(device_token_url())
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        let status = poll.status();
        let body = poll
            .text()
            .await
            .map_err(|e| OAuthError::Network(e.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({}));

        if status.is_success() {
            let authorization_code = json_str(&json, "authorization_code")
                .ok_or_else(|| {
                    OAuthError::Protocol(
                        "device token success response missing authorization_code".into(),
                    )
                })?
                .to_string();
            // Codex server returns the PKCE pair used for the subsequent token exchange.
            let code_verifier = json_str(&json, "code_verifier")
                .ok_or_else(|| {
                    OAuthError::Protocol(
                        "device token success response missing code_verifier".into(),
                    )
                })?
                .to_string();
            break (authorization_code, code_verifier);
        }

        // Codex treats 403/404 as "still pending" while the user enters the code.
        if status.as_u16() == 403 || status.as_u16() == 404 {
            let code = json
                .pointer("/error/code")
                .and_then(|v| v.as_str())
                .or_else(|| json_str(&json, "error"))
                .unwrap_or("");
            if code == "access_denied"
                || code == "authorization_denied"
                || code == "deviceauth_denied"
            {
                return Err(OAuthError::Protocol(format!(
                    "device authorization denied: {code}"
                )));
            }
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        let message = json
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| json_str(&json, "error_description"))
            .or_else(|| json_str(&json, "error"))
            .unwrap_or(body.as_str());
        return Err(OAuthError::Protocol(format!(
            "device auth failed with status {status}: {message}"
        )));
    };

    let (authorization_code, code_verifier) = code_success;
    let token_json = post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("code", &authorization_code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", CLIENT_ID),
            ("code_verifier", &code_verifier),
        ],
    )
    .await?;
    credential_from_token_response(&token_json, None)
}

/// Refresh an openai-codex access token.
pub async fn refresh_openai_codex(refresh_token: &str) -> Result<OAuthCredential, OAuthError> {
    let client = http_client()?;
    let token_json = post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ],
    )
    .await
    .map_err(|e| match e {
        OAuthError::Protocol(msg) if msg.contains("invalid_grant") => OAuthError::needs_login(
            "?",
            OAuthType::OpenaiCodex,
            "refresh token rejected (invalid_grant)",
        ),
        other => other,
    })?;
    credential_from_token_response(&token_json, Some(refresh_token))
}

pub(crate) fn build_authorize_url(redirect_uri: &str, state: &str, code_challenge: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        urlencoding(CLIENT_ID),
        urlencoding(redirect_uri),
        urlencoding(SCOPE),
        urlencoding(code_challenge),
        urlencoding(state),
    )
}

/// Parse Codex device `interval` which is a JSON string (`"5"`), not an int.
fn parse_interval(payload: &serde_json::Value) -> Option<u64> {
    if let Some(n) = payload.get("interval").and_then(|v| v.as_u64()) {
        return Some(n);
    }
    payload
        .get("interval")
        .and_then(|v| v.as_str())
        .and_then(|s| s.trim().parse().ok())
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, OAuthError> {
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| serde_json::json!({ "error": "non_json", "error_description": text }));
    if status.is_success() {
        return Ok(json);
    }
    let message = json
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| json_str(&json, "error_description"))
        .or_else(|| json_str(&json, "error"))
        .unwrap_or(text.as_str());
    Err(OAuthError::Protocol(format!(
        "{url} returned {status}: {message}"
    )))
}

fn credential_from_token_response(
    payload: &serde_json::Value,
    fallback_refresh: Option<&str>,
) -> Result<OAuthCredential, OAuthError> {
    let access_token = json_str(payload, "access_token")
        .ok_or_else(|| OAuthError::Protocol("token response missing access_token".into()))?
        .to_string();
    let refresh_token = json_str(payload, "refresh_token")
        .or(fallback_refresh)
        .ok_or_else(|| OAuthError::Protocol("token response missing refresh_token".into()))?
        .to_string();
    let expires_in = payload
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let expires_at = expires_at_from_secs(expires_in)?;
    let id_token = json_str(payload, "id_token").map(str::to_string);
    let account_id = id_token
        .as_deref()
        .and_then(extract_chatgpt_account_id)
        .or_else(|| extract_chatgpt_account_id(&access_token));
    Ok(OAuthCredential {
        oauth_type: OAuthType::OpenaiCodex,
        access_token,
        refresh_token,
        expires_at,
        account_id,
        id_token,
    })
}

fn extract_chatgpt_account_id(token: &str) -> Option<String> {
    let claims = decode_jwt_claims(token);
    claims
        .get(AUTH_CLAIMS_NS)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
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

/// Pure parsing helpers exercised by unit tests (no network).
#[cfg(test)]
pub(crate) fn parse_device_usercode_response(
    payload: &serde_json::Value,
) -> Result<(String, String, u64), OAuthError> {
    let device_auth_id = json_str(payload, "device_auth_id")
        .ok_or_else(|| OAuthError::Protocol("missing device_auth_id".into()))?
        .to_string();
    let user_code = json_str(payload, "user_code")
        .or_else(|| json_str(payload, "usercode"))
        .ok_or_else(|| OAuthError::Protocol("missing user_code".into()))?
        .to_string();
    let interval = parse_interval(payload).unwrap_or(5).max(1);
    Ok((device_auth_id, user_code, interval))
}

#[cfg(test)]
pub(crate) fn parse_device_token_success(
    payload: &serde_json::Value,
) -> Result<(String, String), OAuthError> {
    let authorization_code = json_str(payload, "authorization_code")
        .ok_or_else(|| OAuthError::Protocol("missing authorization_code".into()))?
        .to_string();
    let code_verifier = json_str(payload, "code_verifier")
        .ok_or_else(|| OAuthError::Protocol("missing code_verifier".into()))?
        .to_string();
    Ok((authorization_code, code_verifier))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn authorize_url_contains_pkce_fields() {
        let url = build_authorize_url(
            "http://localhost:1455/auth/callback",
            "state123",
            "challenge456",
        );
        assert!(url.contains("code_challenge=challenge456"));
        assert!(url.contains("state=state123"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn parses_token_response_and_account_id() {
        let auth = serde_json::json!({
            "chatgpt_account_id": "acct-99",
            "chatgpt_plan_type": "plus"
        });
        let payload_json = serde_json::json!({
            AUTH_CLAIMS_NS: auth
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(payload_json.to_string().as_bytes());
        let id_token = format!("aaa.{payload_b64}.sig");
        let resp = serde_json::json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
            "id_token": id_token
        });
        let cred = credential_from_token_response(&resp, None).unwrap();
        assert_eq!(cred.access_token, "access");
        assert_eq!(cred.refresh_token, "refresh");
        assert_eq!(cred.account_id.as_deref(), Some("acct-99"));
        assert_eq!(cred.oauth_type, OAuthType::OpenaiCodex);
    }

    #[test]
    fn parses_codex_device_usercode_json_shape() {
        // Live shape from auth.openai.com (interval is a string).
        let payload = serde_json::json!({
            "device_auth_id": "deviceauth_abc",
            "user_code": "ABCD-EFGH",
            "interval": "5",
            "expires_at": "2026-07-22T10:32:41.091782+00:00"
        });
        let (id, code, interval) = parse_device_usercode_response(&payload).unwrap();
        assert_eq!(id, "deviceauth_abc");
        assert_eq!(code, "ABCD-EFGH");
        assert_eq!(interval, 5);
        // Must not look for RFC 8628 device_code / verification_uri.
        assert!(payload.get("device_code").is_none());
        assert!(payload.get("verification_uri").is_none());
    }

    #[test]
    fn parses_codex_device_token_success_with_server_pkce() {
        let payload = serde_json::json!({
            "authorization_code": "auth-code-1",
            "code_challenge": "chal",
            "code_verifier": "verif-xyz"
        });
        let (code, verifier) = parse_device_token_success(&payload).unwrap();
        assert_eq!(code, "auth-code-1");
        assert_eq!(verifier, "verif-xyz");
    }

    #[test]
    fn verification_url_matches_codex_cli() {
        assert_eq!(device_verification_url(), format!("{ISSUER}/codex/device"));
        assert_eq!(
            device_redirect_uri(),
            format!("{ISSUER}/deviceauth/callback")
        );
        assert_eq!(DEVICE_API_BASE, format!("{ISSUER}/api/accounts"));
        assert_eq!(
            device_code_url(),
            format!("{DEVICE_API_BASE}/deviceauth/usercode")
        );
        assert_eq!(
            device_token_url(),
            format!("{DEVICE_API_BASE}/deviceauth/token")
        );
    }
}
