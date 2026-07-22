//! xAI Grok Build / SuperGrok OAuth via RFC 8628 device code.

use std::time::Duration;

use crate::auth::{OAuthCredential, OAuthType};

use super::{
    OAuthError, expires_at_from_jwt, expires_at_from_secs, json_str, open_browser, post_form,
};

pub(crate) const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub(crate) const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub(crate) const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub(crate) const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Interactive device-code login for grok-build.
pub async fn login_grok_build(
    timeout: Duration,
    open: bool,
) -> Result<OAuthCredential, OAuthError> {
    let client = http_client()?;
    let start = post_form(
        &client,
        DEVICE_CODE_URL,
        &[("client_id", CLIENT_ID), ("scope", SCOPE)],
    )
    .await?;

    let device_code = json_str(&start, "device_code")
        .ok_or_else(|| OAuthError::Protocol("device-code response missing device_code".into()))?
        .to_string();
    let user_code = json_str(&start, "user_code").unwrap_or("").to_string();
    let verification = json_str(&start, "verification_uri_complete")
        .or_else(|| json_str(&start, "verification_uri"))
        .unwrap_or("https://accounts.x.ai/device")
        .to_string();
    let interval = start
        .get("interval")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .max(1);
    let mut poll_interval = Duration::from_secs(interval);
    let expires_in = start
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(timeout.as_secs());
    let device_deadline =
        tokio::time::Instant::now() + Duration::from_secs(expires_in).min(timeout);
    let overall_deadline = tokio::time::Instant::now() + timeout;

    println!("\nxAI Grok Build sign-in (device code):");
    println!("  Open: {verification}");
    if !user_code.is_empty() {
        println!("  Code: {user_code}");
    }
    println!("Waiting for authorization...\n");
    if open {
        open_browser(&verification);
    }

    loop {
        let now = tokio::time::Instant::now();
        if now >= overall_deadline || now >= device_deadline {
            return Err(OAuthError::Timeout(
                "timed out waiting for xAI device authorization".into(),
            ));
        }

        let poll = client
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", DEVICE_GRANT),
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

        if status.is_success() && json_str(&json, "access_token").is_some() {
            return credential_from_token_response(&json, None);
        }

        let error = json_str(&json, "error").unwrap_or("");
        match error {
            "authorization_pending" => {
                tokio::time::sleep(poll_interval).await;
            }
            "slow_down" => {
                poll_interval =
                    (poll_interval + Duration::from_secs(5)).min(Duration::from_secs(30));
                tokio::time::sleep(poll_interval).await;
            }
            "access_denied" | "authorization_denied" | "expired_token" => {
                return Err(OAuthError::Protocol(format!(
                    "device authorization failed: {error}"
                )));
            }
            "" if !status.is_success() => {
                return Err(OAuthError::Protocol(format!(
                    "device token endpoint returned {status}: {body}"
                )));
            }
            other if !other.is_empty() => {
                return Err(OAuthError::Protocol(format!(
                    "device authorization failed: {other}"
                )));
            }
            _ => tokio::time::sleep(poll_interval).await,
        }
    }
}

/// Refresh a grok-build access token (refresh tokens rotate).
pub async fn refresh_grok_build(refresh_token: &str) -> Result<OAuthCredential, OAuthError> {
    let client = http_client()?;
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Network(e.to_string()))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({}));

    if status.as_u16() == 403 {
        let detail = json_str(&json, "error_description")
            .or_else(|| json_str(&json, "error"))
            .unwrap_or(body.as_str());
        return Err(OAuthError::Entitlement {
            provider: "?".into(),
            detail: detail.to_string(),
        });
    }
    if status.as_u16() == 400 || status.as_u16() == 401 {
        let err = json_str(&json, "error").unwrap_or("invalid_grant");
        return Err(OAuthError::needs_login(
            "?",
            OAuthType::GrokBuild,
            format!("refresh failed: {err}"),
        ));
    }
    if !status.is_success() {
        return Err(OAuthError::Protocol(format!(
            "refresh failed with {status}: {body}"
        )));
    }
    // xAI rotates refresh tokens — require a new one when present; fall back only
    // if the server reuses the old token without rotation.
    let new_refresh = json_str(&json, "refresh_token").unwrap_or(refresh_token);
    if new_refresh.is_empty() {
        return Err(OAuthError::Protocol(
            "refresh response missing refresh_token after rotation".into(),
        ));
    }
    credential_from_token_response(&json, Some(new_refresh))
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
    if refresh_token.is_empty() {
        return Err(OAuthError::Protocol(
            "token response missing refresh_token".into(),
        ));
    }
    let expires_at = if let Some(exp_in) = payload.get("expires_in").and_then(|v| v.as_i64()) {
        expires_at_from_secs(exp_in)?
    } else {
        expires_at_from_jwt(&access_token)?
    };
    let id_token = json_str(payload, "id_token").map(str::to_string);
    Ok(OAuthCredential {
        oauth_type: OAuthType::GrokBuild,
        access_token,
        refresh_token,
        expires_at,
        account_id: None,
        id_token,
    })
}

fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| OAuthError::Network(e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn parses_token_with_jwt_exp_when_expires_in_missing() {
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":1893456000}"#);
        let access = format!("h.{payload}.s");
        let resp = serde_json::json!({
            "access_token": access,
            "refresh_token": "rt-1"
        });
        let cred = credential_from_token_response(&resp, None).unwrap();
        assert_eq!(cred.refresh_token, "rt-1");
        assert!(cred.expires_at.starts_with("2030-01-01"));
        assert_eq!(cred.oauth_type, OAuthType::GrokBuild);
    }

    #[test]
    fn requires_refresh_token() {
        let resp = serde_json::json!({
            "access_token": "a",
            "expires_in": 60
        });
        assert!(credential_from_token_response(&resp, None).is_err());
    }
}
