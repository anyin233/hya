//! Credential storage and header encoding shared by completion and discovery.

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};

use crate::ProviderError;

pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub(crate) enum AuthStyle {
    Bearer(Option<SecretString>),
    CodexSession {
        token: Option<SecretString>,
        account_id: Option<String>,
    },
    GrokSession {
        token: Option<SecretString>,
        client_version: String,
        client_identifier: String,
    },
    Anthropic(Option<SecretString>),
    Google(Option<SecretString>),
}

impl AuthStyle {
    pub(crate) fn is_bearer(&self) -> bool {
        matches!(
            self,
            Self::Bearer(_) | Self::CodexSession { .. } | Self::GrokSession { .. }
        )
    }

    pub(crate) fn secret(&self) -> Option<&SecretString> {
        match self {
            Self::Bearer(secret)
            | Self::Anthropic(secret)
            | Self::Google(secret)
            | Self::CodexSession { token: secret, .. }
            | Self::GrokSession { token: secret, .. } => secret.as_ref(),
        }
    }

    pub(crate) fn credential(&self) -> Option<&str> {
        self.secret()
            .map(|secret| secret.expose_secret().as_str())
            .filter(|value| !value.is_empty())
    }

    pub(crate) fn apply_headers(
        &self,
        headers: &mut HeaderMap,
        credential: Option<&str>,
        completion: bool,
        model_override: Option<&str>,
    ) -> Result<(), ProviderError> {
        if self.is_bearer()
            && let Some(token) = credential
        {
            headers.insert(
                AUTHORIZATION,
                header_value(
                    &format!("Bearer {token}"),
                    true,
                    "invalid auth header value",
                )?,
            );
        }
        match self {
            Self::Bearer(_) => {}
            Self::CodexSession { account_id, .. } => {
                if credential.is_some()
                    && let Some(id) = account_id.as_deref().filter(|id| !id.is_empty())
                {
                    headers.insert(
                        "chatgpt-account-id",
                        header_value(id, true, "invalid auth header value")?,
                    );
                }
            }
            Self::GrokSession {
                client_version,
                client_identifier,
                ..
            } => {
                if credential.is_some() {
                    let mut marker = HeaderValue::from_static("xai-grok-cli");
                    marker.set_sensitive(completion);
                    headers.insert("x-xai-token-auth", marker);
                }
                for (header, value) in [
                    ("x-grok-client-version", client_version.as_str()),
                    ("x-grok-client-identifier", client_identifier.as_str()),
                ] {
                    headers.insert(
                        header,
                        header_value(value, completion, "invalid request header value")?,
                    );
                }
                if let Some(model) = model_override {
                    headers.insert("x-grok-model-override", request_header_value(model)?);
                }
            }
            Self::Anthropic(_) | Self::Google(_) => {
                if let Some(key) = credential {
                    let name = if matches!(self, Self::Anthropic(_)) {
                        "x-api-key"
                    } else {
                        "x-goog-api-key"
                    };
                    headers.insert(name, header_value(key, true, "invalid auth header value")?);
                }
                if matches!(self, Self::Anthropic(_)) {
                    headers.insert(
                        "anthropic-version",
                        HeaderValue::from_static(ANTHROPIC_VERSION),
                    );
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn request_header_value(value: &str) -> Result<HeaderValue, ProviderError> {
    header_value(value, true, "invalid request header value")
}

fn header_value(value: &str, sensitive: bool, error: &str) -> Result<HeaderValue, ProviderError> {
    let mut header =
        HeaderValue::from_str(value).map_err(|_| ProviderError::Http(error.to_owned()))?;
    header.set_sensitive(sensitive);
    Ok(header)
}
