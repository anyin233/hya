//! Bounded startup model-catalog discovery for declared provider routes.
//!
//! This module owns the bounded request loop, URL checks, response limits, and
//! failure classification. Each [`Protocol`] supplies its model-list URL,
//! pagination, headers, and row parsing.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use thiserror::Error;

use crate::{Protocol, RequestPurpose, RouteCredential};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 8;
const MAX_MODELS: usize = 2_000;

/// Presence of authentication material on a discovery request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthPresence {
    /// A non-empty Hya credential was attached.
    Credentialed,
    /// No credential was attached.
    Unauthenticated,
}

/// Input to one provider-local catalog discovery operation.
pub struct CatalogDiscoveryRequest {
    /// Hya provider id used for diagnostics only.
    pub provider_id: String,
    /// Protocol of the provider's kind.
    pub protocol: Arc<dyn Protocol>,
    /// Hya-configured provider API root.
    pub base_url: String,
    /// Hya credential, when the provider has one.
    pub credential: Option<RouteCredential>,
}

impl CatalogDiscoveryRequest {
    /// Construct a discovery request from a provider declaration. A
    /// credential with an empty token counts as none.
    #[must_use]
    pub fn new(
        provider_id: impl Into<String>,
        protocol: Arc<dyn Protocol>,
        base_url: impl Into<String>,
        credential: Option<RouteCredential>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            protocol,
            base_url: base_url.into(),
            credential: credential.filter(|credential| !credential.token.is_empty()),
        }
    }
}

/// One normalized model returned by a provider catalog endpoint.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveredModel {
    /// Provider-owned model id with surrounding whitespace removed.
    pub id: String,
    /// Provider-suggested default reasoning effort, when present.
    pub reasoning_default: Option<String>,
    /// Supported reasoning effort labels for this model.
    pub reasoning_variants: Vec<String>,
    /// Human-readable name when the catalog publishes one (`display_name`,
    /// `displayName`, or an OpenRouter-style `name`).
    pub display_name: Option<String>,
    /// Advertised context window in tokens (`context_length`,
    /// `context_window`, or Google `inputTokenLimit`), when published.
    pub context_limit: Option<u32>,
    /// Advertised max output tokens (`max_output_tokens`,
    /// `top_provider.max_completion_tokens`, or Google `outputTokenLimit`).
    pub output_limit: Option<u32>,
}

/// Read the first non-empty string among `keys`.
fn first_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
            .map(str::to_string)
    })
}

/// Read the first positive `u32` token count among `keys` (dotted paths
/// descend one nested object).
fn first_limit(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        let value = match key.split_once('.') {
            Some((outer, inner)) => object.get(outer)?.get(inner)?,
            None => object.get(*key)?,
        };
        value
            .as_u64()
            .and_then(|tokens| u32::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
    })
}

/// Display-name keys of a model row: `name` included (OpenAI-style lists).
pub(crate) const NAME_KEYS: &[&str] = &["display_name", "displayName", "name"];
/// Display-name keys where `name` is an id or resource name, not a label.
pub(crate) const LABEL_KEYS: &[&str] = &["display_name", "displayName"];

/// Display name and token limits published next to one catalog row.
pub(crate) fn row_metadata(
    object: &serde_json::Map<String, Value>,
    name_keys: &[&str],
) -> (Option<String>, Option<u32>, Option<u32>) {
    let display_name = first_string(object, name_keys);
    let context = first_limit(
        object,
        &[
            "context_length",
            "context_window",
            "max_context_length",
            "inputTokenLimit",
            "input_token_limit",
        ],
    );
    let output = first_limit(
        object,
        &[
            "max_output_tokens",
            "max_completion_tokens",
            "top_provider.max_completion_tokens",
            "outputTokenLimit",
            "output_token_limit",
        ],
    );
    (display_name, context, output)
}

/// Reasoning default and variants a model row publishes.
pub(crate) type RowReasoning = fn(&serde_json::Map<String, Value>) -> (Option<String>, Vec<String>);

/// Shape of an id-keyed model list (`{"data": [{"id": …}, …]}` and kin).
pub(crate) struct RowShape {
    /// Keys that may hold the row array; the first present one is used.
    pub(crate) arrays: &'static [&'static str],
    /// Keys that may hold the row id; the first present one is used.
    pub(crate) ids: &'static [&'static str],
    /// Display-name keys ([`NAME_KEYS`] or [`LABEL_KEYS`]).
    pub(crate) names: &'static [&'static str],
    /// Whether a bare string row is a model id.
    pub(crate) string_rows: bool,
    /// Reasoning default and variants published on a row.
    pub(crate) reasoning: RowReasoning,
}

impl RowShape {
    /// OpenAI-compatible `{"data": [{"id": …}]}` without reasoning metadata.
    pub(crate) const DATA: Self = Self {
        arrays: &["data"],
        ids: &["id"],
        names: NAME_KEYS,
        string_rows: false,
        reasoning: |_| (None, Vec::new()),
    };
}

fn first_present<'a>(
    object: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| object.get(*key))
}

/// Parse the rows of an id-keyed model list.
pub(crate) fn parse_rows(
    payload: &Value,
    shape: &RowShape,
) -> Result<Vec<DiscoveredModel>, CatalogFailure> {
    let array = payload
        .as_object()
        .and_then(|object| first_present(object, shape.arrays))
        .and_then(Value::as_array)
        .ok_or(CatalogFailure::Schema)?;
    let mut models = Vec::new();
    for row in array {
        if shape.string_rows
            && let Some(id) = row.as_str()
        {
            models.push(DiscoveredModel {
                id: id.to_string(),
                ..DiscoveredModel::default()
            });
            continue;
        }
        let object = row.as_object().ok_or(CatalogFailure::Schema)?;
        let id = first_present(object, shape.ids)
            .and_then(Value::as_str)
            .ok_or(CatalogFailure::Schema)?;
        let (reasoning_default, reasoning_variants) = (shape.reasoning)(object);
        let (display_name, context_limit, output_limit) = row_metadata(object, shape.names);
        models.push(DiscoveredModel {
            id: id.to_string(),
            reasoning_default,
            reasoning_variants,
            display_name: display_name.filter(|name| name.trim() != id.trim()),
            context_limit,
            output_limit,
        });
    }
    Ok(models)
}

/// Parse one catalog page with `protocol` and normalize its model ids.
///
/// Public for deterministic fixture tests; network requests and pagination
/// remain owned by [`discover_models`].
pub fn parse_catalog_payload(
    protocol: &dyn Protocol,
    payload: &Value,
) -> Result<Vec<DiscoveredModel>, CatalogFailure> {
    protocol.parse_models(payload).map(normalize_models)
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
        kind: String,
    },
}

/// Discover models from one declared provider using bounded HTTP semantics.
pub async fn discover_models(request: CatalogDiscoveryRequest) -> ProviderDiscoveryOutcome {
    let protocol = request.protocol.as_ref();
    let auth = if request.credential.is_some() {
        AuthPresence::Credentialed
    } else {
        AuthPresence::Unauthenticated
    };
    let endpoint = match protocol
        .models_url(&request.base_url)
        .and_then(|url| checked_url(&url).map(String::from))
    {
        Ok(endpoint) => endpoint,
        Err(error) => return ProviderDiscoveryOutcome::Failed { error },
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    let auth_headers =
        protocol.auth_headers(request.credential.as_ref(), RequestPurpose::Catalog, None);
    if crate::http::insert_headers(&mut headers, auth_headers).is_err() {
        return ProviderDiscoveryOutcome::Failed {
            error: CatalogFailure::UnsafeUrl,
        };
    }
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
        let url = match protocol
            .models_page_url(&endpoint, cursor.as_deref())
            .and_then(|url| checked_url(&url))
        {
            Ok(url) => url,
            Err(error) => return ProviderDiscoveryOutcome::Failed { error },
        };
        let response = match send_page(&client, url, &headers).await {
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
        let page_models = match parse_catalog_payload(protocol, &payload) {
            Ok(models) => models,
            Err(error) => return ProviderDiscoveryOutcome::Failed { error },
        };
        models.extend(page_models);
        if models.len() > MAX_MODELS {
            return ProviderDiscoveryOutcome::Failed {
                error: CatalogFailure::PaginationLimit,
            };
        }
        let next = match protocol.models_next_cursor(&payload) {
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

/// `<base>/models` (unless the path already ends in `/models`), after
/// dropping `strip_suffix` from the path; query and fragment are cleared.
pub(crate) fn models_resource_url(
    base: &str,
    strip_suffix: Option<&str>,
) -> Result<reqwest::Url, CatalogFailure> {
    let mut url = reqwest::Url::parse(base).map_err(|_| CatalogFailure::InvalidUrl)?;
    let mut path = url.path().trim_end_matches('/').to_string();
    if let Some(suffix) = strip_suffix
        && path.ends_with(suffix)
    {
        path.truncate(path.len() - suffix.len());
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
    Ok(url)
}

/// `url` with `key=value` appended to its query.
pub(crate) fn append_query(url: &str, key: &str, value: &str) -> Result<String, CatalogFailure> {
    let mut url = reqwest::Url::parse(url).map_err(|_| CatalogFailure::InvalidUrl)?;
    url.query_pairs_mut().append_pair(key, value);
    Ok(url.into())
}

/// Parse a protocol-built URL and reject non-HTTP schemes and userinfo.
fn checked_url(url: &str) -> Result<reqwest::Url, CatalogFailure> {
    let url = reqwest::Url::parse(url).map_err(|_| CatalogFailure::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CatalogFailure::InvalidUrl);
    }
    if url.username() != "" || url.password().is_some() {
        return Err(CatalogFailure::UnsafeUrl);
    }
    Ok(url)
}

async fn send_page(
    client: &reqwest::Client,
    url: reqwest::Url,
    headers: &HeaderMap,
) -> Result<Vec<u8>, CatalogFailure> {
    let response = client
        .get(url)
        .headers(headers.clone())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::{
        AnthropicMessagesProtocol, GrokBuildProtocol, OpenAiCodexProtocol, OpenAiResponsesProtocol,
    };

    #[test]
    fn responses_and_codex_catalog_urls_use_model_resources() {
        let responses = OpenAiResponsesProtocol
            .models_url("https://gateway.example/v1/responses")
            .unwrap();
        assert_eq!(responses, "https://gateway.example/v1/models");

        let codex = OpenAiCodexProtocol
            .models_url("https://chatgpt.com/backend-api/codex")
            .unwrap();
        assert_eq!(
            codex,
            "https://chatgpt.com/backend-api/codex/models?client_version=0.144.0"
        );
    }

    #[test]
    fn anthropic_pagination_keeps_limit_and_cursor() {
        let endpoint = AnthropicMessagesProtocol
            .models_url("https://api.anthropic.com/v1")
            .unwrap();
        let first = AnthropicMessagesProtocol
            .models_page_url(&endpoint, None)
            .unwrap();
        assert_eq!(first, "https://api.anthropic.com/v1/models?limit=1000");
        let next = AnthropicMessagesProtocol
            .models_page_url(&endpoint, Some("cursor-1"))
            .unwrap();
        assert_eq!(
            next,
            "https://api.anthropic.com/v1/models?after_id=cursor-1"
        );
    }

    #[test]
    fn codex_headers_keep_protocol_metadata_and_omit_absent_auth() {
        let headers = OpenAiCodexProtocol.auth_headers(None, RequestPurpose::Catalog, None);
        let names: Vec<_> = headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("openai-beta", "responses=experimental"),
                ("user-agent", "codex_cli_rs")
            ]
        );
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
            &GrokBuildProtocol,
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
                ..DiscoveredModel::default()
            }],
        );
    }
}
