//! OAuth credentials and the login/refresh flow a provider kind may supply.
//!
//! The host stores credentials and runs the single-flight refresh; a flow
//! only talks to its OAuth endpoints.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// OAuth credential bundle stored under `auth/<provider>.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCredential {
    /// OAuth type of the flow that produced this bundle (`openai-codex`, …).
    pub oauth_type: String,
    /// Short-lived bearer access token for API calls.
    pub access_token: String,
    /// Long-lived refresh token used to mint new access tokens.
    pub refresh_token: String,
    /// RFC3339 UTC timestamp when the access token expires.
    pub expires_at: String,
    /// Account id from token claims, when known.
    pub account_id: Option<String>,
    /// OpenID id_token (not used as the API bearer).
    pub id_token: Option<String>,
}

/// Errors from OAuth login, refresh, or credential loading.
#[derive(Debug, Error)]
pub enum OAuthError {
    /// Stored credentials are missing, expired, or revoked; user must re-login.
    #[error(
        "OAuth credentials for provider '{provider}' require re-login ({reason}). Run: hya oauth login --provider {provider} --type {oauth_type}"
    )]
    NeedsLogin {
        /// Config/auth provider id.
        provider: String,
        /// Canonical OAuth type string for the re-login command line.
        oauth_type: String,
        /// Short reason shown in the error message.
        reason: String,
    },
    /// Token is valid but the subscription cannot call the API.
    #[error(
        "provider '{provider}' OAuth grant is valid but not entitled for API access ({detail}). Re-login will not help; use an API key path or upgrade the subscription."
    )]
    Entitlement {
        /// Provider id that failed entitlement checks.
        provider: String,
        /// Upstream detail describing the entitlement failure.
        detail: String,
    },
    /// HTTP/transport failure talking to the OAuth or catalog endpoint.
    #[error("OAuth network error: {0}")]
    Network(String),
    /// Unexpected OAuth protocol response or missing fields.
    #[error("OAuth protocol error: {0}")]
    Protocol(String),
    /// Interactive flow or network wait exceeded the configured timeout.
    #[error("OAuth timed out: {0}")]
    Timeout(String),
    /// Local filesystem failure while reading/writing auth or config.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Config path or YAML update failure (non-network).
    #[error("config error: {0}")]
    Config(String),
}

impl OAuthError {
    /// Build a re-login error for `oauth_type`.
    pub fn needs_login(
        provider: impl Into<String>,
        oauth_type: &str,
        reason: impl Into<String>,
    ) -> Self {
        Self::NeedsLogin {
            provider: provider.into(),
            oauth_type: oauth_type.to_string(),
            reason: reason.into(),
        }
    }
}

/// Options of one interactive login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OAuthLoginRequest {
    /// Use a localhost callback instead of the flow's default (`--loopback`).
    pub loopback: bool,
    /// `--browser` (`Some(true)`) or `--no-browser` (`Some(false)`); `None`
    /// lets the flow pick its default.
    pub browser: Option<bool>,
    /// Overall deadline of the interactive flow.
    pub timeout: Duration,
}

/// Login and refresh for one OAuth type.
#[async_trait]
pub trait ProviderAuthFlow: Send + Sync {
    /// OAuth type id (`--type` value and the stored `oauth_type`).
    fn oauth_type(&self) -> &str;
    /// Inference base URL a login writes when none is given.
    fn default_base_url(&self) -> &str;
    /// Model id shown after login when the catalog preview is empty.
    fn default_model(&self) -> &str;
    /// Whether the flow accepts `--loopback`.
    fn supports_loopback(&self) -> bool {
        false
    }
    /// Run the interactive login and return the new credential.
    async fn login(&self, request: OAuthLoginRequest) -> Result<OAuthCredential, OAuthError>;
    /// Mint a fresh credential from `credential`'s refresh token.
    async fn refresh(&self, credential: &OAuthCredential) -> Result<OAuthCredential, OAuthError>;
}
