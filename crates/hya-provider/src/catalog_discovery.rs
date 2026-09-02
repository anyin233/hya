//! Bounded startup model-catalog discovery for declared provider routes.
//!
//! This module owns provider list endpoint construction, optional authentication,
//! response parsing, pagination, and failure classification. Runtime composition
//! supplies only Hya-owned provider declarations and credentials.

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt as _;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use thiserror::Error;

use crate::ProviderKind;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 8;
const MAX_MODELS: usize = 2_000;
const CODEX_MODELS_CLIENT_VERSION: &str = "0.144.0";

/// Presence of authentication material on a discovery request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthPresence {
    /// A non-empty Hya credential was attached.
    Credentialed,
    /// No credential was attached.
    Unauthenticated,
}

/// Optional Hya-owned authentication and protocol session metadata.
///
/// The enum deliberately does not implement [`Debug`] so secret token values
/// cannot appear in diagnostics. Empty token strings are treated as absent.
pub enum CatalogAuth {
    /// Send no credential-derived headers.
    None,
    /// Send an API key for Anthropic or Google.
    ApiKey(String),
    /// Send an optional bearer and optional Codex account id.
    Bearer {
        /// Bearer token, when present.
        token: Option<String>,
        /// ChatGPT account identity, sent only with a bearer token.
        account_id: Option<String>,
    },
    /// Send Grok Build client identity and an optional bearer session.
    Grok {
        /// Bearer token, when present.
        token: Option<String>,
        /// Non-secret client version header.
        client_version: String,
        /// Non-secret client identifier header.
        client_identifier: String,
    },
}

impl CatalogAuth {
    /// Construct an unauthenticated discovery request.
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::None
    }

    /// Construct an API-key-authenticated discovery request.
    #[must_use]
    pub fn api_key(key: impl Into<String>) -> Self {
        let key = key.into();
        if key.is_empty() {
            Self::None
        } else {
            Self::ApiKey(key)
        }
    }

    /// Construct a bearer-authenticated discovery request.
    #[must_use]
    pub fn bearer(token: impl Into<String>, account_id: Option<String>) -> Self {
        let token = token.into();
        if token.is_empty() {
            Self::None
        } else {
            Self::Bearer {
                token: Some(token),
                account_id,
            }
        }
    }

    /// Construct a Grok Build session request with optional auth.
    #[must_use]
    pub fn grok(
        token: Option<String>,
        client_version: impl Into<String>,
        client_identifier: impl Into<String>,
    ) -> Self {
        Self::Grok {
            token: token.filter(|token| !token.is_empty()),
            client_version: client_version.into(),
            client_identifier: client_identifier.into(),
        }
    }

    fn presence(&self) -> AuthPresence {
        let credentialed = match self {
            Self::None => false,
            Self::ApiKey(key) => !key.is_empty(),
            Self::Bearer { token, .. } | Self::Grok { token, .. } => {
                token.as_deref().is_some_and(|token| !token.is_empty())
            }
        };
        if credentialed {
            AuthPresence::Credentialed
        } else {
            AuthPresence::Unauthenticated
        }
    }
}

/// Input to one provider-local catalog discovery operation.
pub struct CatalogDiscoveryRequest {
    /// Hya provider id used for diagnostics only.
    pub provider_id: String,
    /// Declared provider protocol kind.
    pub kind: ProviderKind,
    /// Hya-configured provider API root.
    pub base_url: String,
    /// Optional Hya credential/session material.
    pub auth: CatalogAuth,
}

impl CatalogDiscoveryRequest {
    /// Construct a discovery request from a provider declaration.
    #[must_use]
    pub fn new(
        provider_id: impl Into<String>,
        kind: ProviderKind,
        base_url: impl Into<String>,
        auth: CatalogAuth,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind,
            base_url: base_url.into(),
            auth,
        }
    }
}

/// One normalized model returned by a provider catalog endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredModel {
    /// Provider-owned model id with surrounding whitespace removed.
    pub id: String,
    /// Provider-suggested default reasoning effort, when present.
    pub reasoning_default: Option<String>,
    /// Supported reasoning effort labels for this model.
    pub reasoning_variants: Vec<String>,
}

/// Parse a provider catalog payload and normalize its model ids and metadata.
///
/// This parser is public for deterministic fixture tests; network requests and
/// pagination remain owned by [`discover_models`].
pub fn parse_catalog_payload(
    kind: ProviderKind,
    payload: &Value,
) -> Result<Vec<DiscoveredModel>, CatalogFailure> {
    let mut models = Vec::new();
    match kind {
        ProviderKind::Google => parse_google(payload, &mut models)?,
        ProviderKind::OpenAiCompatible | ProviderKind::OpenAiResponse | ProviderKind::Anthropic => {
            parse_data_models(payload, kind, &mut models, false)?
        }
        ProviderKind::OpenAiCodex | ProviderKind::GrokBuild => {
            parse_data_models(payload, kind, &mut models, payload.get("data").is_none())?;
        }
    }
    let models = normalize_models(models);
    if kind == ProviderKind::GrokBuild {
        return Ok(models
            .into_iter()
            .filter(|model| is_grok_executable_model_id(&model.id))
            .collect());
    }
    Ok(models)
}

/// Return whether a Grok Build catalog row names an executable text model.
///
/// This matches the established OAuth catalog adapter and rejects the media-only
/// rows that Hya's completion protocol cannot execute.
#[must_use]
pub fn is_grok_executable_model_id(id: &str) -> bool {
    !id.contains("imagine") && !id.contains("image") && !id.contains("video")
}

fn parse_data_models(
    payload: &Value,
    kind: ProviderKind,
    models: &mut Vec<DiscoveredModel>,
    allow_models_field: bool,
) -> Result<(), CatalogFailure> {
    let array = payload
        .get("data")
        .or_else(|| allow_models_field.then(|| payload.get("models")).flatten())
        .and_then(Value::as_array)
        .ok_or(CatalogFailure::Schema)?;
    for row in array {
        if kind == ProviderKind::GrokBuild
            && let Some(id) = row.as_str()
        {
            models.push(DiscoveredModel {
                id: id.to_string(),
                reasoning_default: None,
                reasoning_variants: Vec::new(),
            });
            continue;
        }
        let object = row.as_object().ok_or(CatalogFailure::Schema)?;
        let id = match kind {
            ProviderKind::OpenAiCodex => object.get("slug").or_else(|| object.get("id")),
            ProviderKind::GrokBuild => object
                .get("id")
                .or_else(|| object.get("model"))
                .or_else(|| object.get("name")),
            ProviderKind::OpenAiCompatible
            | ProviderKind::OpenAiResponse
            | ProviderKind::Anthropic
            | ProviderKind::Google => object.get("id"),
        }
        .and_then(Value::as_str)
        .ok_or(CatalogFailure::Schema)?;
        let (reasoning_default, reasoning_variants) = match kind {
            ProviderKind::OpenAiCodex => parse_codex_reasoning(object),
            ProviderKind::GrokBuild => parse_grok_reasoning(object),
            ProviderKind::OpenAiCompatible
            | ProviderKind::OpenAiResponse
            | ProviderKind::Anthropic
            | ProviderKind::Google => (None, Vec::new()),
        };
        models.push(DiscoveredModel {
            id: id.to_string(),
            reasoning_default,
            reasoning_variants,
        });
    }
    Ok(())
}

fn parse_google(payload: &Value, models: &mut Vec<DiscoveredModel>) -> Result<(), CatalogFailure> {
    let array = payload
        .get("models")
        .and_then(Value::as_array)
        .ok_or(CatalogFailure::Schema)?;
    for row in array {
        let object = row.as_object().ok_or(CatalogFailure::Schema)?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(CatalogFailure::Schema)?;
        let Some(id) = name.strip_prefix("models/") else {
            return Err(CatalogFailure::Schema);
        };
        let methods = object
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .ok_or(CatalogFailure::Schema)?;
        if methods
            .iter()
            .any(|method| method.as_str() == Some("generateContent"))
        {
            models.push(DiscoveredModel {
                id: id.to_string(),
                reasoning_default: None,
                reasoning_variants: Vec::new(),
            });
        }
    }
    Ok(())
}

fn parse_codex_reasoning(object: &serde_json::Map<String, Value>) -> (Option<String>, Vec<String>) {
    let default = object
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let variants = object
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    let level = level.as_object()?;
                    level
                        .get("effort")
                        .or_else(|| level.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|value| !value.is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    (default, variants)
}

fn parse_grok_reasoning(object: &serde_json::Map<String, Value>) -> (Option<String>, Vec<String>) {
    let default = object
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let variants = object
        .get("reasoning_efforts")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    let level = level.as_object()?;
                    level
                        .get("value")
                        .or_else(|| level.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|value| !value.is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    (default, variants)
}

fn normalize_models(models: Vec<DiscoveredModel>) -> Vec<DiscoveredModel> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter_map(|mut model| {
            let trimmed = model.id.trim();
            if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
                return None;
            }
            if !seen.insert(trimmed.to_string()) {
                return None;
            }
            model.id = trimmed.to_string();
            Some(model)
        })
        .collect()
}

/// Safe bounded reason that a provider catalog could not be used.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum CatalogFailure {
    /// Provider URL syntax or scheme is not supported.
    #[error("invalid provider URL")]
    InvalidUrl,
    /// Provider URL contains userinfo or another rejected URL component.
    #[error("unsafe provider URL")]
    UnsafeUrl,
    /// A redirect response was rejected before credentials could be forwarded.
    #[error("provider catalog redirected")]
    Redirect,
    /// Network transport failed before a complete response was received.
    #[error("provider catalog transport failed")]
    Transport,
    /// Provider response exceeded the bounded request deadline.
    #[error("provider catalog timed out")]
    Timeout,
    /// Provider returned a non-success status other than auth rejection.
    #[error("provider catalog returned HTTP {status}")]
    HttpStatus {
        /// Numeric response status.
        status: u16,
    },
    /// Provider response exceeded the one-megabyte page limit.
    #[error("provider catalog response too large")]
    BodyTooLarge,
    /// Provider response was not valid JSON.
    #[error("provider catalog response was not valid JSON")]
    Decode,
    /// Provider response did not match its declared protocol shape.
    #[error("provider catalog response schema was incompatible")]
    Schema,
    /// Pagination cursor was malformed, repeated, or exceeded the bound.
    #[error("provider catalog pagination exceeded bounds")]
    PaginationLimit,
}

/// Typed result of one provider catalog request sequence.
pub enum ProviderDiscoveryOutcome {
    /// At least one normalized provider model was discovered.
    Discovered {
        /// Normalized model rows in provider response order.
        models: Vec<DiscoveredModel>,
        /// Authentication state for the request sequence.
        auth: AuthPresence,
    },
    /// A valid provider response contained no usable model rows.
    Empty {
        /// Authentication state for the request sequence.
        auth: AuthPresence,
    },
    /// An unauthenticated provider endpoint required credentials.
    AuthRequired,
    /// A supplied Hya credential was rejected by the endpoint.
    AuthRejected,
    /// The provider's response or transport failed safely.
    Failed {
        /// Bounded failure class; no response body is retained.
        error: CatalogFailure,
    },
    /// Hya has no safe adapter for this provider kind.
    Unsupported {
        /// Declared kind with no adapter.
        kind: ProviderKind,
    },
}

/// Discover models from one declared provider using bounded HTTP semantics.
pub async fn discover_models(request: CatalogDiscoveryRequest) -> ProviderDiscoveryOutcome {
    let auth = request.auth.presence();
    let endpoint = match models_endpoint(&request.base_url, request.kind) {
        Ok(endpoint) => endpoint,
        Err(error) => return ProviderDiscoveryOutcome::Failed { error },
    };
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return ProviderDiscoveryOutcome::Failed {
                error: CatalogFailure::Transport,
            };
        }
    };

    let mut models = Vec::new();
    let mut cursor = None;
    let mut cursors = HashSet::new();
    for page in 0..MAX_PAGES {
        let url = match page_url(&endpoint, request.kind, cursor.as_deref()) {
            Ok(url) => url,
            Err(error) => return ProviderDiscoveryOutcome::Failed { error },
        };
        let response = match send_page(&client, &url, request.kind, &request.auth).await {
            Ok(response) => response,
            Err(error) => {
                return classify_failure(error, auth);
            }
        };
        let payload: Value = match serde_json::from_slice(&response) {
            Ok(payload) => payload,
            Err(_) => {
                return ProviderDiscoveryOutcome::Failed {
                    error: CatalogFailure::Decode,
                };
            }
        };
        let page_models = match parse_catalog_payload(request.kind, &payload) {
            Ok(models) => models,
            Err(error) => return ProviderDiscoveryOutcome::Failed { error },
        };
        models.extend(page_models);
        if models.len() > MAX_MODELS {
            return ProviderDiscoveryOutcome::Failed {
                error: CatalogFailure::PaginationLimit,
            };
        }
        let next = match next_cursor(request.kind, &payload) {
            Ok(next) => next,
            Err(error) => return ProviderDiscoveryOutcome::Failed { error },
        };
        let Some(next) = next else {
            let models = normalize_models(models);
            return if models.is_empty() {
                ProviderDiscoveryOutcome::Empty { auth }
            } else {
                ProviderDiscoveryOutcome::Discovered { models, auth }
            };
        };
        if page + 1 == MAX_PAGES || !cursors.insert(next.clone()) {
            return ProviderDiscoveryOutcome::Failed {
                error: CatalogFailure::PaginationLimit,
            };
        }
        cursor = Some(next);
    }
    ProviderDiscoveryOutcome::Failed {
        error: CatalogFailure::PaginationLimit,
    }
}

fn classify_failure(error: CatalogFailure, auth: AuthPresence) -> ProviderDiscoveryOutcome {
    match error {
        CatalogFailure::HttpStatus { status: 401 | 403 } => match auth {
            AuthPresence::Credentialed => ProviderDiscoveryOutcome::AuthRejected,
            AuthPresence::Unauthenticated => ProviderDiscoveryOutcome::AuthRequired,
        },
        error => ProviderDiscoveryOutcome::Failed { error },
    }
}

fn models_endpoint(base_url: &str, kind: ProviderKind) -> Result<reqwest::Url, CatalogFailure> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| CatalogFailure::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CatalogFailure::InvalidUrl);
    }
    if url.username() != "" || url.password().is_some() {
        return Err(CatalogFailure::UnsafeUrl);
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    if kind == ProviderKind::OpenAiResponse && path.ends_with("/responses") {
        path.truncate(path.len() - "/responses".len());
    }
    if !path.ends_with("/models") {
        path = if path.is_empty() {
            "/models".to_string()
        } else {
            format!("{path}/models")
        };
        url.set_path(&path);
    }
    url.set_query(None);
    url.set_fragment(None);
    if kind == ProviderKind::OpenAiCodex {
        url.query_pairs_mut()
            .append_pair("client_version", CODEX_MODELS_CLIENT_VERSION);
    }
    Ok(url)
}

fn page_url(
    endpoint: &reqwest::Url,
    kind: ProviderKind,
    cursor: Option<&str>,
) -> Result<reqwest::Url, CatalogFailure> {
    let mut url = endpoint.clone();
    if cursor.is_none() && kind == ProviderKind::Anthropic {
        url.query_pairs_mut().append_pair("limit", "1000");
    }
    if let Some(cursor) = cursor {
        let key = if kind == ProviderKind::Google {
            "pageToken"
        } else {
            "after_id"
        };
        url.query_pairs_mut().append_pair(key, cursor);
    }
    Ok(url)
}

fn next_cursor(kind: ProviderKind, payload: &Value) -> Result<Option<String>, CatalogFailure> {
    match kind {
        ProviderKind::Anthropic => {
            let has_more = payload
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_more {
                return Ok(None);
            }
            payload
                .get("last_id")
                .or_else(|| payload.get("after_id"))
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned)
                .ok_or(CatalogFailure::PaginationLimit)
                .map(Some)
        }
        ProviderKind::Google => payload
            .get("nextPageToken")
            .map(|token| {
                token
                    .as_str()
                    .filter(|cursor| !cursor.is_empty())
                    .map(str::to_owned)
                    .ok_or(CatalogFailure::PaginationLimit)
            })
            .transpose(),
        ProviderKind::OpenAiCompatible
        | ProviderKind::OpenAiResponse
        | ProviderKind::OpenAiCodex
        | ProviderKind::GrokBuild => Ok(None),
    }
}

async fn send_page(
    client: &reqwest::Client,
    url: &reqwest::Url,
    kind: ProviderKind,
    auth: &CatalogAuth,
) -> Result<Vec<u8>, CatalogFailure> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    apply_auth_headers(&mut headers, kind, auth)?;
    let response = client
        .get(url.clone())
        .headers(headers)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                CatalogFailure::Timeout
            } else {
                CatalogFailure::Transport
            }
        })?;
    let status = response.status();
    if status.is_redirection() {
        return Err(CatalogFailure::Redirect);
    }
    if !status.is_success() {
        return Err(CatalogFailure::HttpStatus {
            status: status.as_u16(),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(CatalogFailure::BodyTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                CatalogFailure::Timeout
            } else {
                CatalogFailure::Transport
            }
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(CatalogFailure::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn apply_auth_headers(
    headers: &mut HeaderMap,
    kind: ProviderKind,
    auth: &CatalogAuth,
) -> Result<(), CatalogFailure> {
    match auth {
        CatalogAuth::None => {}
        CatalogAuth::ApiKey(key) if !key.is_empty() => {
            let name = if kind == ProviderKind::Anthropic {
                "x-api-key"
            } else {
                "x-goog-api-key"
            };
            let mut value = HeaderValue::from_str(key).map_err(|_| CatalogFailure::UnsafeUrl)?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static(name), value);
        }
        CatalogAuth::Bearer {
            token: Some(token),
            account_id,
        } if !token.is_empty() => {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| CatalogFailure::UnsafeUrl)?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
            if kind == ProviderKind::OpenAiCodex
                && let Some(account_id) = account_id.as_deref().filter(|id| !id.is_empty())
            {
                let mut value =
                    HeaderValue::from_str(account_id).map_err(|_| CatalogFailure::UnsafeUrl)?;
                value.set_sensitive(true);
                headers.insert(HeaderName::from_static("chatgpt-account-id"), value);
            }
        }
        CatalogAuth::Grok {
            token,
            client_version,
            client_identifier,
        } => {
            if let Some(token) = token.as_deref().filter(|token| !token.is_empty()) {
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| CatalogFailure::UnsafeUrl)?;
                value.set_sensitive(true);
                headers.insert(AUTHORIZATION, value);
                headers.insert(
                    HeaderName::from_static("x-xai-token-auth"),
                    HeaderValue::from_static("xai-grok-cli"),
                );
            }
            let version =
                HeaderValue::from_str(client_version).map_err(|_| CatalogFailure::UnsafeUrl)?;
            let identifier =
                HeaderValue::from_str(client_identifier).map_err(|_| CatalogFailure::UnsafeUrl)?;
            headers.insert(HeaderName::from_static("x-grok-client-version"), version);
            headers.insert(
                HeaderName::from_static("x-grok-client-identifier"),
                identifier,
            );
        }
        CatalogAuth::ApiKey(_) | CatalogAuth::Bearer { .. } => {}
    }
    if kind == ProviderKind::Anthropic {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    if kind == ProviderKind::OpenAiCodex {
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static("codex_cli_rs"),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn responses_and_codex_catalog_urls_use_model_resources() {
        let responses = models_endpoint(
            "https://gateway.example/v1/responses",
            ProviderKind::OpenAiResponse,
        )
        .unwrap();
        assert_eq!(responses.as_str(), "https://gateway.example/v1/models");

        let codex = models_endpoint(
            "https://chatgpt.com/backend-api/codex",
            ProviderKind::OpenAiCodex,
        )
        .unwrap();
        assert_eq!(codex.path(), "/backend-api/codex/models");
        assert_eq!(
            codex
                .query_pairs()
                .find(|(key, _)| key == "client_version")
                .map(|(_, value)| value),
            Some(CODEX_MODELS_CLIENT_VERSION.into())
        );
    }

    #[test]
    fn anthropic_pagination_keeps_limit_and_cursor() {
        let endpoint =
            models_endpoint("https://api.anthropic.com/v1", ProviderKind::Anthropic).unwrap();
        let first = page_url(&endpoint, ProviderKind::Anthropic, None).unwrap();
        assert!(
            first
                .query_pairs()
                .any(|(key, value)| key == "limit" && value == "1000")
        );
        let next = page_url(&endpoint, ProviderKind::Anthropic, Some("cursor-1")).unwrap();
        assert!(
            next.query_pairs()
                .any(|(key, value)| key == "after_id" && value == "cursor-1")
        );
    }

    #[test]
    fn codex_headers_keep_protocol_metadata_and_omit_absent_auth() {
        let mut headers = HeaderMap::new();
        apply_auth_headers(
            &mut headers,
            ProviderKind::OpenAiCodex,
            &CatalogAuth::Bearer {
                token: None,
                account_id: Some("must-not-leak".to_string()),
            },
        )
        .unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key("chatgpt-account-id"));
        assert_eq!(headers["openai-beta"], "responses=experimental");
        assert_eq!(headers["user-agent"], "codex_cli_rs");
    }

    #[test]
    fn auth_status_split_and_grok_media_filter_fail_closed() {
        assert!(matches!(
            classify_failure(
                CatalogFailure::HttpStatus { status: 401 },
                AuthPresence::Unauthenticated,
            ),
            ProviderDiscoveryOutcome::AuthRequired
        ));
        assert!(matches!(
            classify_failure(
                CatalogFailure::HttpStatus { status: 403 },
                AuthPresence::Credentialed,
            ),
            ProviderDiscoveryOutcome::AuthRejected
        ));
        let models = parse_catalog_payload(
            ProviderKind::GrokBuild,
            &serde_json::json!({
                "models": [
                    { "id": "grok-4.5" },
                    { "id": "grok-imagine" },
                    { "id": "video-generator" }
                ]
            }),
        )
        .unwrap();
        assert_eq!(
            models,
            vec![DiscoveredModel {
                id: "grok-4.5".to_string(),
                reasoning_default: None,
                reasoning_variants: Vec::new(),
            }],
        );
    }
}
