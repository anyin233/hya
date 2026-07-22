//! OpenAI Codex / ChatGPT subscription OAuth (PKCE + device-code).

use std::time::Duration;

use crate::auth::{OAuthCredential, OAuthType};

use super::callback::wait_for_callback;
use super::pkce::{generate_pkce_pair, generate_state};
use super::{
    OAuthError, decode_jwt_claims, expires_at_from_secs, json_str, open_browser, post_form,
};

pub(crate) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(crate) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(crate) const DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(crate) const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub(crate) const SCOPE: &str = "openid profile email offline_access";
pub(crate) const AUTH_CLAIMS_NS: &str = "https://api.openai.com/auth";
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

/// Device-code login for headless openai-codex sessions.
pub async fn login_openai_codex_device(timeout: Duration) -> Result<OAuthCredential, OAuthError> {
    let (verifier, challenge) = generate_pkce_pair();
    let client = http_client()?;
    let start = post_form(
        &client,
        DEVICE_CODE_URL,
        &[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ],
    )
    .await?;

    let device_code = json_str(&start, "device_code")
        .ok_or_else(|| OAuthError::Protocol("device-code response missing device_code".into()))?
        .to_string();
    let user_code = json_str(&start, "user_code").unwrap_or("").to_string();
    let verification = json_str(&start, "verification_uri_complete")
        .or_else(|| json_str(&start, "verification_uri"))
        .unwrap_or("https://auth.openai.com/codex/device")
        .to_string();
    let interval = start
        .get("interval")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .max(1);
    let mut poll_interval = Duration::from_secs(interval);

    println!("\nChatGPT / Codex device sign-in:");
    println!("  Open: {verification}");
    if !user_code.is_empty() {
        println!("  Code: {user_code}");
    }
    println!("Waiting for authorization...\n");
    open_browser(&verification);

    let deadline = tokio::time::Instant::now() + timeout;
    let authorization_code = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(OAuthError::Timeout(
                "timed out waiting for ChatGPT device authorization".into(),
            ));
        }
        let poll = client
            .post(DEVICE_TOKEN_URL)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", device_code.as_str()),
            ])
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
        if let Some(code) = json_str(&json, "authorization_code") {
            break code.to_string();
        }
        let error = json_str(&json, "error").unwrap_or("");
        if error == "authorization_pending" || (!status.is_success() && error.is_empty()) {
            tokio::time::sleep(poll_interval).await;
            continue;
        }
        if error == "slow_down" {
            poll_interval += Duration::from_secs(5);
            tokio::time::sleep(poll_interval).await;
            continue;
        }
        if !error.is_empty() {
            return Err(OAuthError::Protocol(format!(
                "device authorization failed: {error}"
            )));
        }
        if status.is_success() {
            // Unexpected success without code.
            return Err(OAuthError::Protocol(
                "device token response missing authorization_code".into(),
            ));
        }
        tokio::time::sleep(poll_interval).await;
    };

    let token_json = post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("code", &authorization_code),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", &verifier),
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
    let mut url = format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        urlencoding(CLIENT_ID),
        urlencoding(redirect_uri),
        urlencoding(SCOPE),
        urlencoding(code_challenge),
        urlencoding(state),
    );
    // Keep stable for tests / logs.
    let _ = &mut url;
    url
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
}
