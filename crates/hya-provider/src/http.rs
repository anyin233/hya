//! `HttpProvider` — drives a `Protocol` over reqwest + SSE into the canonical
//! `Event` stream. One provider per upstream route (OpenAI-compatible or
//! Anthropic), selected by the model id it serves.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::{Event, Message, MessageId, ModelRef, SessionId};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::ReceiverStream;

mod stream;

use crate::anthropic::AnthropicMessagesProtocol;
use crate::google::GoogleProtocol;
use crate::openai::{
    GrokBuildProtocol, OpenAiChatProtocol, OpenAiResponsesProtocol, encode_input_items,
};
use crate::{
    Capabilities, CompactedWindow, CompletionRequest, EventStream, ModelCatalogSource, Protocol,
    Provider, ProviderError, ProviderModel, ReasoningEffort, append_capabilities_identity,
    append_identity_bytes, append_identity_count, append_identity_optional_bytes,
};

const MAX_REQUEST_ATTEMPTS: usize = 3;
const BASE_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded wait for response headers on each streamed-completion attempt.
///
/// Reasoning endpoints behind large prompt contexts can legitimately take tens
/// of seconds to produce status line + headers, so 60s sits well above real
/// pre-header latency while keeping a hung route bounded and operator-visible.
/// A deadline miss becomes [`ProviderError::Transport`], which the existing
/// `is_retryable_before_stream()` classification feeds back through the normal
/// attempt/backoff/router-failover path unchanged. Configurable per route via
/// [`HttpProvider::with_response_header_timeout`].
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounded silence between SSE frames on an established stream.
///
/// The window opens as soon as headers arrive (covering the wait for the first
/// event) and resets on every delivered frame, so continuously streaming
/// responses have unlimited lifetime. Five minutes is deliberately generous:
/// live providers emit deltas, summaries, or keep-alive traffic far more often
/// even during long reasoning pauses, so normal think time cannot trip it,
/// while a wedged connection dies in bounded time. A miss is a post-stream
/// failure under the no-replay boundary: it surfaces exactly once and is never
/// retried or failed over. Configurable per route via
/// [`HttpProvider::with_idle_timeout`].
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Which upstream HTTP protocol shape an [`HttpProvider`] speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI Chat Completions (`/chat/completions`).
    OpenAiCompatible,
    /// OpenAI Responses API (`/responses`).
    OpenAiResponse,
    /// ChatGPT Codex subscription backend (`chatgpt.com/backend-api/codex`).
    OpenAiCodex,
    /// Grok Build Responses route with encrypted reasoning content.
    GrokBuild,
    /// Anthropic Messages API (`/messages`).
    Anthropic,
    /// Google Gemini generateContent.
    Google,
}

impl ProviderKind {
    /// Static default reasoning labels used without allocating during capability checks.
    fn reasoning_variant_labels(self) -> &'static [&'static str] {
        match self {
            ProviderKind::Anthropic => &["low", "medium", "high", "max"],
            ProviderKind::OpenAiCompatible => &["minimal", "low", "medium", "high", "xhigh"],
            ProviderKind::OpenAiResponse | ProviderKind::OpenAiCodex => {
                &["none", "minimal", "low", "medium", "high", "xhigh", "max"]
            }
            ProviderKind::GrokBuild => &["low", "medium", "high"],
            ProviderKind::Google => &["high", "max"],
        }
    }

    /// Default reasoning-variant menu when a model has no per-model override.
    #[must_use]
    pub fn reasoning_variants(self) -> Vec<String> {
        self.reasoning_variant_labels()
            .iter()
            .map(|level| (*level).to_string())
            .collect()
    }
}

/// Optional live bearer source for re-resolving tokens on each stream.
pub type BearerResolver = Arc<dyn Fn() -> Result<String, ProviderError> + Send + Sync>;

/// Optional forced-refresh hook for the pre-stream auth-recovery level.
///
/// Invoked at most once per streamed request when a pre-stream HTTP 401/403
/// response suggests the resolved credential expired server-side even though
/// earlier resolutions succeeded. The hook receives the credential material
/// used by the failed request; on success [`HttpProvider`] re-resolves auth
/// headers exactly once and retries inside the existing
/// [`MAX_REQUEST_ATTEMPTS`] budget. If the hook is absent, fails, or leaves
/// the credential unchanged, the original status error surfaces unchanged.
pub type AuthRefresher = Arc<dyn Fn(&str) -> Result<(), ProviderError> + Send + Sync>;

enum AuthStyle {
    Bearer(Option<SecretString>),
    /// ChatGPT Codex OAuth: optional Bearer JWT plus optional account id header.
    CodexSession {
        token: Option<SecretString>,
        account_id: Option<String>,
    },
    /// Grok Build session with optional auth and non-secret client headers.
    GrokSession {
        token: Option<SecretString>,
        client_version: String,
        client_identifier: String,
    },
    Anthropic {
        key: Option<SecretString>,
        version: String,
    },
    Google(Option<SecretString>),
}

/// One configured HTTP route: reqwest client + [`Protocol`] + SSE → [`EventStream`].
///
/// Owns SSE framing; the protocol decoder only sees data payloads. Redirects are
/// disabled so auth headers are never followed cross-origin.
pub struct HttpProvider {
    id: String,
    protocol: Box<dyn Protocol>,
    client: reqwest::Client,
    endpoint: String,
    google_base: Option<String>,
    auth: AuthStyle,
    bearer_resolver: Option<BearerResolver>,
    auth_refresher: Option<AuthRefresher>,
    models: HashSet<String>,
    model_reasoning_variants: BTreeMap<String, Vec<String>>,
    model_reasoning_defaults: BTreeMap<String, ReasoningEffort>,
    caps: Capabilities,
    kind: ProviderKind,
    catalog_source: ModelCatalogSource,
    response_header_timeout: Duration,
    stream_idle_timeout: Duration,
}

fn sensitive(value: &str) -> Result<HeaderValue, ProviderError> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::Http("invalid auth header value".to_string()))?;
    header.set_sensitive(true);
    Ok(header)
}

fn request_header_value(value: &str) -> Result<HeaderValue, ProviderError> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::Http("invalid request header value".to_string()))?;
    header.set_sensitive(true);
    Ok(header)
}
impl HttpProvider {
    /// Build a route for `kind` at `base_url` with optional Hya credential
    /// material and the models claimed by this route.
    pub fn new(
        id: impl Into<String>,
        kind: ProviderKind,
        base_url: &str,
        api_key: Option<String>,
        models: impl IntoIterator<Item = String>,
    ) -> Result<Self, ProviderError> {
        // Security: never follow redirects (reqwest keeps `x-api-key` across a
        // cross-origin 3xx). Connect-timeout only — a blanket read/total timeout
        // would abort long streaming completions. Liveness comes from the much
        // narrower RESPONSE_HEADER_TIMEOUT (per attempt) and STREAM_IDLE_TIMEOUT
        // (between SSE frames) instead.
        // Timeout overrides are not part of `configured_identity_v1`: they tune
        // liveness, not what the route serves, like the fixed connect timeout.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        let base = base_url.trim_end_matches('/');
        let key = api_key
            .filter(|value| !value.is_empty())
            .map(SecretString::new);
        let (protocol, endpoint, auth): (Box<dyn Protocol>, String, AuthStyle) = match kind {
            ProviderKind::OpenAiCompatible => (
                Box::new(OpenAiChatProtocol),
                format!("{base}/chat/completions"),
                AuthStyle::Bearer(key),
            ),
            ProviderKind::OpenAiResponse | ProviderKind::OpenAiCodex => (
                Box::new(OpenAiResponsesProtocol),
                format!("{base}/responses"),
                AuthStyle::Bearer(key),
            ),
            ProviderKind::GrokBuild => (
                Box::new(GrokBuildProtocol),
                format!("{base}/responses"),
                AuthStyle::Bearer(key),
            ),
            ProviderKind::Anthropic => (
                Box::new(AnthropicMessagesProtocol),
                format!("{base}/messages"),
                AuthStyle::Anthropic {
                    key,
                    version: "2023-06-01".to_string(),
                },
            ),
            ProviderKind::Google => (
                Box::new(GoogleProtocol),
                String::new(),
                AuthStyle::Google(key),
            ),
        };
        let google_base = if kind == ProviderKind::Google {
            Some(base.to_string())
        } else {
            None
        };
        Ok(Self {
            id: id.into(),
            protocol,
            client,
            endpoint,
            google_base,
            auth,
            bearer_resolver: None,
            auth_refresher: None,
            models: models.into_iter().collect(),
            model_reasoning_variants: BTreeMap::new(),
            model_reasoning_defaults: BTreeMap::new(),
            kind,
            catalog_source: ModelCatalogSource::Configured,
            caps: Capabilities {
                streaming_tool_calls: true,
                parallel_tool_calls: true,
                usage_reporting: true,
                reasoning_request: true,
                max_context: 200_000,
                ..Capabilities::default()
            },
            response_header_timeout: RESPONSE_HEADER_TIMEOUT,
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        })
    }

    /// Switch a ChatGPT Codex provider to OAuth session auth (account id header).
    ///
    /// No-op for non-`OpenAiCodex` kinds so callers can chain unconditionally.
    #[must_use]
    pub fn with_codex_session_auth(mut self, account_id: Option<String>) -> Self {
        if self.kind != ProviderKind::OpenAiCodex {
            return self;
        }
        let token = match &self.auth {
            AuthStyle::Bearer(key)
            | AuthStyle::CodexSession { token: key, .. }
            | AuthStyle::GrokSession { token: key, .. } => key.clone(),
            AuthStyle::Anthropic { key, .. } | AuthStyle::Google(key) => key.clone(),
        };
        self.auth = AuthStyle::CodexSession { token, account_id };
        self
    }

    /// Switch a Grok Build provider to OAuth session auth (CLI chat-proxy headers).
    ///
    /// No-op for non-`GrokBuild` kinds so callers can chain unconditionally.
    #[must_use]
    pub fn with_grok_session_auth(
        mut self,
        client_version: impl Into<String>,
        client_identifier: impl Into<String>,
    ) -> Self {
        if self.kind != ProviderKind::GrokBuild {
            return self;
        }
        let token = match &self.auth {
            AuthStyle::Bearer(key)
            | AuthStyle::CodexSession { token: key, .. }
            | AuthStyle::GrokSession { token: key, .. } => key.clone(),
            AuthStyle::Anthropic { key, .. } | AuthStyle::Google(key) => key.clone(),
        };
        self.auth = AuthStyle::GrokSession {
            token,
            client_version: client_version.into(),
            client_identifier: client_identifier.into(),
        };
        self
    }

    /// Re-resolve the bearer token on each stream (hot-reload for Grok OAuth).
    #[must_use]
    pub fn with_bearer_resolver(mut self, resolver: BearerResolver) -> Self {
        self.bearer_resolver = Some(resolver);
        self
    }

    /// Attach the single-shot forced-refresh hook (auth-recovery level).
    ///
    /// See [`AuthRefresher`] for the contract. Strictly pre-stream: once an
    /// event stream is established nothing is refreshed or replayed.
    #[must_use]
    pub fn with_auth_refresher(mut self, refresher: AuthRefresher) -> Self {
        self.auth_refresher = Some(refresher);
        self
    }

    /// Attach per-model reasoning variant lists (replaces kind defaults for those ids).
    #[must_use]
    pub fn with_model_reasoning_variants(
        mut self,
        variants: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        self.model_reasoning_variants = variants.into_iter().collect();
        self
    }
    /// Attach configured per-model reasoning defaults.
    ///
    /// Keys are the bare upstream model ids used by this provider. A missing
    /// value means this route has no default metadata for that model; an
    /// explicit [`ReasoningEffort::Off`] is retained as a real `none` value.
    #[must_use]
    pub fn with_model_reasoning_defaults(
        mut self,
        defaults: impl IntoIterator<Item = (String, Option<ReasoningEffort>)>,
    ) -> Self {
        self.model_reasoning_defaults = defaults
            .into_iter()
            .filter_map(|(model, effort)| effort.map(|effort| (model, effort)))
            .collect();
        self
    }
    /// Set the provenance emitted by this route's catalog rows.
    #[must_use]
    pub fn with_catalog_source(mut self, source: ModelCatalogSource) -> Self {
        self.catalog_source = source;
        self
    }

    /// Override the bounded response-header wait ([`RESPONSE_HEADER_TIMEOUT`])
    /// for this route — for gateways slower than the default anticipates, and
    /// for tests exercising the deadline.
    #[must_use]
    pub fn with_response_header_timeout(mut self, duration: Duration) -> Self {
        self.response_header_timeout = duration;
        self
    }

    /// Override the bounded SSE idle wait ([`STREAM_IDLE_TIMEOUT`]) for this
    /// route — for links with longer silences than the default assumes, and for
    /// tests exercising the deadline.
    #[must_use]
    pub fn with_idle_timeout(mut self, duration: Duration) -> Self {
        self.stream_idle_timeout = duration;
        self
    }

    fn resolve_bearer(
        &self,
        fallback: Option<&SecretString>,
    ) -> Result<Option<String>, ProviderError> {
        if let Some(resolver) = &self.bearer_resolver {
            return resolver().map(|token| (!token.is_empty()).then_some(token));
        }
        Ok(fallback
            .map(|value| value.expose_secret().clone())
            .filter(|token| !token.is_empty()))
    }

    /// Credential material this route authenticates with right now, honoring
    /// the bearer resolver when present. Feeds the forced-refresh hook so the
    /// caller can detect rotations that happened underneath a failing request.
    fn active_credential(&self) -> Result<Option<String>, ProviderError> {
        match &self.auth {
            AuthStyle::Bearer(key)
            | AuthStyle::CodexSession { token: key, .. }
            | AuthStyle::GrokSession { token: key, .. } => self.resolve_bearer(key.as_ref()),
            AuthStyle::Anthropic { key, .. } | AuthStyle::Google(key) => Ok(key
                .as_ref()
                .map(|value| value.expose_secret().clone())
                .filter(|token| !token.is_empty())),
        }
    }

    fn auth_headers(
        &self,
        model_override: Option<&str>,
    ) -> Result<(HeaderMap, Option<String>), ProviderError> {
        let mut headers = HeaderMap::new();
        let refresh_credential = match &self.auth {
            AuthStyle::Bearer(key) => {
                let token = self.resolve_bearer(key.as_ref())?;
                if let Some(token) = token.as_deref() {
                    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
                }
                token
            }
            AuthStyle::CodexSession { token, account_id } => {
                let token = self.resolve_bearer(token.as_ref())?;
                if let Some(token) = token.as_deref() {
                    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
                    if let Some(account_id) = account_id.as_deref().filter(|s| !s.is_empty()) {
                        headers.insert(
                            HeaderName::from_static("chatgpt-account-id"),
                            sensitive(account_id)?,
                        );
                    }
                }
                token
            }
            AuthStyle::GrokSession {
                token,
                client_version,
                client_identifier,
            } => {
                let token = self.resolve_bearer(token.as_ref())?;
                if let Some(token) = token.as_deref() {
                    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
                    headers.insert(
                        HeaderName::from_static("x-xai-token-auth"),
                        sensitive("xai-grok-cli")?,
                    );
                }
                headers.insert(
                    HeaderName::from_static("x-grok-client-version"),
                    request_header_value(client_version)?,
                );
                headers.insert(
                    HeaderName::from_static("x-grok-client-identifier"),
                    request_header_value(client_identifier)?,
                );
                if let Some(model) = model_override {
                    headers.insert(
                        HeaderName::from_static("x-grok-model-override"),
                        request_header_value(model)?,
                    );
                }
                token
            }
            AuthStyle::Anthropic { key, version } => {
                if let Some(key) = key.as_ref() {
                    headers.insert(
                        HeaderName::from_static("x-api-key"),
                        sensitive(key.expose_secret())?,
                    );
                }
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_str(version)
                        .map_err(|_| ProviderError::Http("invalid version header".to_string()))?,
                );
                None
            }
            AuthStyle::Google(key) => {
                if let Some(key) = key.as_ref() {
                    headers.insert(
                        HeaderName::from_static("x-goog-api-key"),
                        sensitive(key.expose_secret())?,
                    );
                }
                None
            }
        };
        Ok((headers, refresh_credential))
    }

    fn request_headers(
        &self,
        extra: &BTreeMap<String, String>,
        model_override: Option<&str>,
    ) -> Result<(HeaderMap, Option<String>), ProviderError> {
        let (mut headers, mut refresh_credential) = self.auth_headers(model_override)?;
        for (name, value) in extra {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ProviderError::Http("invalid request header name".to_string()))?;
            if header_name == AUTHORIZATION {
                // A call-scoped Authorization override did not use the route's
                // bearer, so the route must not refresh its stored credential.
                refresh_credential = None;
            }
            headers.insert(header_name, request_header_value(value)?);
        }
        Ok((headers, refresh_credential))
    }

    async fn send_stream_request(
        &self,
        url: &str,
        body: &Value,
        extra_headers: &BTreeMap<String, String>,
        model_override: Option<&str>,
    ) -> Result<reqwest::Response, ProviderError> {
        // Auth-recovery level: the forced-refresh retry fires at most once per
        // request and always occupies one of the MAX_REQUEST_ATTEMPTS slots
        // below — it never extends the budget, so broken credentials cannot
        // degrade into a refresh/retry loop. Once a response succeeds, no
        // refresh or retry exists anymore (no-replay boundary above us).
        let mut auth_recovered = false;
        for attempt in 0..MAX_REQUEST_ATTEMPTS {
            // Resolve auth exactly once per attempt. The captured value is the
            // credential this request sent, even if another concurrent request
            // rotates storage before this response is handled.
            let (headers, attempted_credential) =
                self.request_headers(extra_headers, model_override)?;
            let result = timeout(
                self.response_header_timeout,
                self.client.post(url).headers(headers).json(body).send(),
            )
            .await;
            let error = match result {
                Ok(Ok(response)) if response.status().is_success() => return Ok(response),
                Ok(Ok(response)) => {
                    let status = response.status().as_u16();
                    let try_refresh = !auth_recovered
                        && self.auth_refresher.is_some()
                        && (status == 401 || status == 403)
                        // Budget integrity: only fire while the retried
                        // request still fits inside the attempt cap.
                        && attempt + 1 < MAX_REQUEST_ATTEMPTS;
                    if try_refresh && let Some(refresher) = &self.auth_refresher {
                        let original = response_error(response).await;
                        let stale = attempted_credential;
                        let hooked = stale
                            .as_deref()
                            .is_some_and(|token| refresher(token).is_ok());
                        let rotated = hooked
                            && stale.is_some_and(|before| {
                                self.active_credential().is_ok_and(|current| {
                                    current.as_deref() != Some(before.as_str())
                                })
                            });
                        if rotated {
                            auth_recovered = true;
                            continue;
                        }
                        // Hook absent, failing, or yielding the same
                        // credential: surface the original status unchanged.
                        return Err(original);
                    }
                    response_error(response).await
                }
                Ok(Err(error)) => ProviderError::Transport(error.to_string()),
                // Headers never arrived within the deadline: classify exactly
                // like other pre-stream transport failures so the remaining
                // attempts and router failover still run.
                Err(_elapsed) => ProviderError::Transport(format!(
                    "no response headers within {:#?}",
                    self.response_header_timeout
                )),
            };
            if attempt + 1 == MAX_REQUEST_ATTEMPTS || !error.is_retryable_before_stream() {
                return Err(error);
            }
            sleep(retry_delay(&error, attempt)).await;
        }
        Err(ProviderError::Transport(
            "provider request exhausted without a response".to_string(),
        ))
    }

    /// Whether this route speaks the Responses wire and has `/responses/compact`.
    fn supports_responses_compact(&self) -> bool {
        matches!(
            self.kind,
            ProviderKind::OpenAiResponse | ProviderKind::OpenAiCodex | ProviderKind::GrokBuild
        )
    }

    fn compact_endpoint(&self) -> Option<String> {
        if !self.supports_responses_compact() {
            return None;
        }
        // endpoint is `{base}/responses` for create; compact sits alongside it.
        Some(if self.endpoint.ends_with("/responses") {
            format!("{}/compact", self.endpoint)
        } else {
            format!("{}/responses/compact", self.endpoint.trim_end_matches('/'))
        })
    }

    /// Return the claimed bare model id without allocating.
    fn served_model_name<'a>(&'a self, model: &'a ModelRef) -> Option<&'a str> {
        let base = match model.as_str().rsplit_once('#') {
            Some((head, variant)) if !variant.is_empty() => head,
            _ => model.as_str(),
        };
        if self.models.contains(base) {
            return Some(base);
        }
        if let Some((provider_id, model_id)) = base.split_once('/')
            && provider_id == self.id
            && self.models.contains(model_id)
        {
            return Some(model_id);
        }
        None
    }

    /// Resolve a claimed model to the bare upstream id for request encoding.
    fn served_model_id(&self, model: &ModelRef) -> Option<String> {
        self.served_model_name(model).map(str::to_owned)
    }

    fn configured_identity_bytes_v1(&self) -> Option<Vec<u8>> {
        let mut identity = Vec::new();
        append_identity_bytes(&mut identity, b"hya.provider.http.configured.v1")?;
        append_identity_bytes(&mut identity, env!("CARGO_PKG_VERSION").as_bytes())?;
        append_identity_bytes(&mut identity, self.id.as_bytes())?;
        append_identity_bytes(&mut identity, provider_kind_identity(self.kind))?;
        append_identity_bytes(&mut identity, self.endpoint.as_bytes())?;
        append_identity_optional_bytes(&mut identity, self.google_base.as_deref())?;
        append_identity_bytes(&mut identity, b"alias/bare-model-id")?;
        append_identity_bytes(&mut identity, b"alias/provider-prefixed-model-id")?;
        append_identity_bytes(&mut identity, b"alias/nonempty-variant-suffix")?;

        let mut models = self.models.iter().collect::<Vec<_>>();
        models.sort_unstable();
        append_identity_count(&mut identity, models.len())?;
        for model in &models {
            append_identity_bytes(&mut identity, model.as_bytes())?;
        }

        append_identity_count(&mut identity, self.model_reasoning_variants.len())?;
        for (model, variants) in &self.model_reasoning_variants {
            append_identity_bytes(&mut identity, model.as_bytes())?;
            append_identity_count(&mut identity, variants.len())?;
            for variant in variants {
                append_identity_bytes(&mut identity, variant.as_bytes())?;
            }
        }

        append_identity_bytes(&mut identity, b"model-reasoning-defaults")?;
        append_identity_count(&mut identity, models.len())?;
        for model in &models {
            append_identity_bytes(&mut identity, model.as_bytes())?;
            let default = self
                .model_reasoning_defaults
                .get(*model)
                .copied()
                .unwrap_or(ReasoningEffort::Off);
            append_identity_bytes(&mut identity, default.as_str().as_bytes())?;
        }

        append_capabilities_identity(&mut identity, &self.caps)?;
        append_auth_identity(&mut identity, &self.auth)?;
        append_identity_bytes(&mut identity, b"bearer-resolver-slot")?;
        match &self.bearer_resolver {
            Some(_) => {
                identity.push(1);
                append_identity_bytes(&mut identity, self.id.as_bytes())?;
            }
            None => identity.push(0),
        }
        append_identity_bytes(&mut identity, b"auth-refresher-slot")?;
        match &self.auth_refresher {
            Some(_) => {
                identity.push(1);
                append_identity_bytes(&mut identity, self.id.as_bytes())?;
            }
            None => identity.push(0),
        }
        Some(identity)
    }
}

fn provider_kind_identity(kind: ProviderKind) -> &'static [u8] {
    match kind {
        ProviderKind::OpenAiCompatible => b"openai-compatible",
        ProviderKind::OpenAiResponse => b"openai-response",
        ProviderKind::OpenAiCodex => b"openai-codex",
        ProviderKind::GrokBuild => b"grok-build",
        ProviderKind::Anthropic => b"anthropic",
        ProviderKind::Google => b"google",
    }
}

fn append_auth_identity(output: &mut Vec<u8>, auth: &AuthStyle) -> Option<()> {
    match auth {
        AuthStyle::Bearer(key) => {
            append_identity_bytes(output, b"bearer")?;
            output.push(u8::from(
                key.as_ref()
                    .is_some_and(|value| !value.expose_secret().is_empty()),
            ));
        }
        AuthStyle::CodexSession { token, account_id } => {
            append_identity_bytes(output, b"codex-session")?;
            output.push(u8::from(
                token
                    .as_ref()
                    .is_some_and(|value| !value.expose_secret().is_empty()),
            ));
            append_identity_optional_bytes(output, account_id.as_deref())?;
        }
        AuthStyle::GrokSession {
            token,
            client_version,
            client_identifier,
        } => {
            append_identity_bytes(output, b"grok-session")?;
            output.push(u8::from(
                token
                    .as_ref()
                    .is_some_and(|value| !value.expose_secret().is_empty()),
            ));
            append_identity_bytes(output, client_version.as_bytes())?;
            append_identity_bytes(output, client_identifier.as_bytes())?;
        }
        AuthStyle::Anthropic { key, version } => {
            append_identity_bytes(output, b"anthropic")?;
            output.push(u8::from(
                key.as_ref()
                    .is_some_and(|value| !value.expose_secret().is_empty()),
            ));
            append_identity_bytes(output, version.as_bytes())?;
        }
        AuthStyle::Google(key) => {
            append_identity_bytes(output, b"google")?;
            output.push(u8::from(
                key.as_ref()
                    .is_some_and(|value| !value.expose_secret().is_empty()),
            ));
        }
    }
    Some(())
}

async fn response_error(response: reqwest::Response) -> ProviderError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let text = timeout(ERROR_BODY_TIMEOUT, response.text())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let message = text.chars().take(500).collect();
    ProviderError::HttpStatus {
        status,
        message,
        retry_after,
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .min(MAX_RETRY_AFTER),
    )
}

fn retry_delay(error: &ProviderError, attempt: usize) -> Duration {
    if let Some(delay) = error.retry_after() {
        return delay.min(MAX_RETRY_AFTER);
    }
    let exponent = u32::try_from(attempt).unwrap_or(u32::MAX).min(8);
    let base = BASE_RETRY_DELAY.saturating_mul(2_u32.saturating_pow(exponent));
    let jitter_percent = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(100, |elapsed| 75 + elapsed.subsec_nanos() % 51);
    base.saturating_mul(jitter_percent) / 100
}

#[async_trait]
impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.served_model_name(model).map(|_| self.caps.clone())
    }

    fn reasoning_default(&self, model: &ModelRef) -> Option<ReasoningEffort> {
        let served_model = self.served_model_name(model)?;
        self.model_reasoning_defaults.get(served_model).copied()
    }

    fn supports_reasoning_effort(&self, model: &ModelRef, effort: ReasoningEffort) -> Option<bool> {
        let served_model = self.served_model_name(model)?;
        if effort == ReasoningEffort::Off {
            return Some(true);
        }
        if !self.caps.reasoning_request {
            return Some(false);
        }
        if let Some(variants) = self.model_reasoning_variants.get(served_model) {
            return Some(
                variants
                    .iter()
                    .any(|variant| ReasoningEffort::parse(variant) == Some(effort)),
            );
        }
        Some(
            self.kind
                .reasoning_variant_labels()
                .iter()
                .any(|variant| ReasoningEffort::parse(variant) == Some(effort)),
        )
    }

    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        self.configured_identity_bytes_v1()
    }

    fn catalog(&self) -> Vec<ProviderModel> {
        let variants = if self.caps.reasoning_request {
            self.kind.reasoning_variants()
        } else {
            Vec::new()
        };
        self.models
            .iter()
            .map(|model| ProviderModel {
                provider_id: self.id.clone(),
                model_id: model.clone(),
                capabilities: self.caps.clone(),
                reasoning_variants: self
                    .model_reasoning_variants
                    .get(model)
                    .cloned()
                    .unwrap_or_else(|| variants.clone()),
                reasoning_default: self.model_reasoning_defaults.get(model).copied(),
                source: self.catalog_source,
            })
            .collect()
    }

    async fn stream(
        &self,
        mut req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if let Some(model_id) = self.served_model_id(&req.model) {
            req.model = ModelRef::new(model_id);
        }
        let body = self.protocol.encode(&req)?;
        let decoder = self.protocol.decoder(session, message, req.reasoning);
        let url = match &self.google_base {
            Some(base) => format!(
                "{base}/v1beta/models/{}:streamGenerateContent?alt=sse",
                req.model.as_str()
            ),
            None => self.endpoint.clone(),
        };
        let model_override =
            matches!(self.auth, AuthStyle::GrokSession { .. }).then_some(req.model.as_str());
        let resp = self
            .send_stream_request(&url, &body, &req.headers, model_override)
            .await?;
        let (tx, rx) = mpsc::channel::<Result<Event, ProviderError>>(64);
        tokio::spawn(stream::pump(resp, decoder, tx, self.stream_idle_timeout));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn compact_responses(
        &self,
        model: &ModelRef,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        let Some(url) = self.compact_endpoint() else {
            return Ok(None);
        };
        let model_id = self
            .served_model_id(model)
            .unwrap_or_else(|| model.as_str().to_string());
        // Encode the same input window the create path would send.
        let mut input = encode_input_items(messages)?;
        if let Some(system) = system.filter(|s| !s.is_empty()) {
            // Prefer instructions-equivalent as a system item at the front when
            // the compact endpoint is given an explicit system prompt.
            input.insert(0, json!({"role": "system", "content": system}));
        }
        let body = json!({
            "model": model_id,
            "input": input,
        });
        let model_override =
            matches!(self.auth, AuthStyle::GrokSession { .. }).then_some(model_id.as_str());
        let (headers, _) = self.request_headers(&BTreeMap::new(), model_override)?;
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = text.get(..500).unwrap_or(text.as_str());
            return Err(ProviderError::Http(format!("compact {status}: {snippet}")));
        }
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        let items = payload
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                ProviderError::Decode("responses compact reply missing output array".to_string())
            })?;
        Ok(Some(CompactedWindow { items }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[test]
    fn credentialless_openai_route_omits_authorization() {
        let provider = HttpProvider::new(
            "openai",
            ProviderKind::OpenAiCompatible,
            "https://example/v1",
            None,
            ["gpt-5".to_string()],
        )
        .unwrap();
        let (headers, _) = provider.request_headers(&BTreeMap::new(), None).unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
    }
    #[test]
    fn credentialless_auth_styles_never_emit_empty_secret_headers() {
        let cases = [
            (
                ProviderKind::OpenAiCompatible,
                "https://example/v1",
                None,
                None,
            ),
            (
                ProviderKind::OpenAiResponse,
                "https://example/v1",
                None,
                None,
            ),
            (ProviderKind::Anthropic, "https://example/v1", None, None),
            (ProviderKind::Google, "https://example", None, None),
            (
                ProviderKind::OpenAiCodex,
                "https://example/v1",
                None,
                Some("account"),
            ),
        ];
        for (kind, base, key, account) in cases {
            let provider = HttpProvider::new("provider", kind, base, key, ["model".to_string()])
                .unwrap()
                .with_codex_session_auth(account.map(str::to_string));
            let (headers, _) = provider.request_headers(&BTreeMap::new(), None).unwrap();
            assert!(!headers.contains_key(AUTHORIZATION));
            assert!(!headers.contains_key(HeaderName::from_static("x-api-key")));
            assert!(!headers.contains_key(HeaderName::from_static("x-goog-api-key")));
            assert!(!headers.contains_key(HeaderName::from_static("chatgpt-account-id")));
        }

        let grok = HttpProvider::new(
            "grok",
            ProviderKind::GrokBuild,
            "https://example/v1",
            None,
            ["model".to_string()],
        )
        .unwrap()
        .with_grok_session_auth("client-v1", "grok-cli");
        let (headers, _) = grok.request_headers(&BTreeMap::new(), None).unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key(HeaderName::from_static("x-xai-token-auth")));
        assert_eq!(
            headers
                .get(HeaderName::from_static("x-grok-client-version"))
                .and_then(|value| value.to_str().ok()),
            Some("client-v1")
        );
    }
    use crate::{DevProvider, FakeProvider, ProviderRouter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn retry_after_seconds_are_parsed_and_bounded() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("600"), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("not-a-delay"), None);
    }

    fn provider() -> Result<HttpProvider, ProviderError> {
        HttpProvider::new(
            "12th",
            ProviderKind::OpenAiCompatible,
            "https://example/v1",
            Some("key".to_string()),
            ["claude-opus-4-8".to_string(), "gpt-5.5".to_string()],
        )
    }

    #[test]
    fn resolves_bare_prefixed_and_variant_model_refs() -> Result<(), ProviderError> {
        let p = provider()?;
        assert_eq!(
            p.served_model_id(&ModelRef::new("claude-opus-4-8"))
                .as_deref(),
            Some("claude-opus-4-8"),
        );
        assert_eq!(
            p.served_model_id(&ModelRef::new("12th/claude-opus-4-8"))
                .as_deref(),
            Some("claude-opus-4-8"),
        );
        assert_eq!(
            p.served_model_id(&ModelRef::new("12th/claude-opus-4-8#high"))
                .as_deref(),
            Some("claude-opus-4-8"),
        );
        assert!(p.capabilities(&ModelRef::new("12th/gpt-5.5")).is_some());
        Ok(())
    }

    #[test]
    fn rejects_unknown_and_foreign_provider_refs() -> Result<(), ProviderError> {
        let p = provider()?;
        assert!(
            p.served_model_id(&ModelRef::new("other/claude-opus-4-8"))
                .is_none()
        );
        assert!(
            p.served_model_id(&ModelRef::new("claude-sonnet-4-6"))
                .is_none()
        );
        assert!(
            p.capabilities(&ModelRef::new("12th/unknown-model"))
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn reasoning_variants_are_family_specific() {
        assert_eq!(
            ProviderKind::Anthropic.reasoning_variants(),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(
            ProviderKind::OpenAiCompatible.reasoning_variants(),
            ["minimal", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(ProviderKind::Google.reasoning_variants(), ["high", "max"]);
    }

    #[test]
    fn configured_provider_identity_covers_routes_without_secrets_or_live_state() {
        fn configured_identities(router: &ProviderRouter) -> Option<Vec<Vec<u8>>> {
            router.configured_identities_v1()
        }

        let route = |id: &str, kind: ProviderKind, base: &str, key: &str, models: &[&str]| {
            match HttpProvider::new(
                id,
                kind,
                base,
                Some(key.to_string()),
                models.iter().map(|model| (*model).to_string()),
            ) {
                Ok(provider) => provider,
                Err(error) => panic!("test HTTP provider route: {error}"),
            }
        };
        let router = |provider: HttpProvider| ProviderRouter::new().with(Arc::new(provider));

        let empty = ProviderRouter::new();
        assert_eq!(configured_identities(&empty), Some(Vec::new()));

        let equivalent_a = router(route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1/",
            "secret-a",
            &["model-b", "model-a"],
        ));
        let equivalent_b = router(route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1",
            "secret-b",
            &["model-a", "model-b"],
        ));
        assert!(
            configured_identities(&equivalent_a)
                .as_ref()
                .is_some_and(|identities| !identities.is_empty())
        );
        assert_eq!(
            configured_identities(&equivalent_a),
            configured_identities(&equivalent_b)
        );

        let empty_key = router(route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1",
            "",
            &["model-a", "model-b"],
        ));
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&empty_key)
        );

        let insertion_a = route(
            "first",
            ProviderKind::OpenAiCompatible,
            "https://first.test/v1",
            "key",
            &["model"],
        );
        let insertion_b = route(
            "second",
            ProviderKind::OpenAiCompatible,
            "https://second.test/v1",
            "key",
            &["model"],
        );
        let insertion_ab = ProviderRouter::new()
            .with(Arc::new(insertion_a))
            .with(Arc::new(insertion_b));
        let insertion_ba = ProviderRouter::new()
            .with(Arc::new(route(
                "second",
                ProviderKind::OpenAiCompatible,
                "https://second.test/v1",
                "key",
                &["model"],
            )))
            .with(Arc::new(route(
                "first",
                ProviderKind::OpenAiCompatible,
                "https://first.test/v1",
                "key",
                &["model"],
            )));
        assert_ne!(
            configured_identities(&insertion_ab),
            configured_identities(&insertion_ba)
        );

        let id_changed = router(route(
            "gateway-renamed",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1",
            "secret-a",
            &["model-a", "model-b"],
        ));
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&id_changed)
        );

        let kind_changed = router(route(
            "gateway",
            ProviderKind::OpenAiResponse,
            "https://example.test/v1",
            "secret-a",
            &["model-a", "model-b"],
        ));
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&kind_changed)
        );

        let endpoint_changed = router(route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://other.test/v1",
            "secret-a",
            &["model-a", "model-b"],
        ));
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&endpoint_changed)
        );

        let models_changed = router(route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1",
            "secret-a",
            &["model-a", "model-c"],
        ));
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&models_changed)
        );

        let mut caps_changed_route = route(
            "gateway",
            ProviderKind::OpenAiCompatible,
            "https://example.test/v1",
            "secret-a",
            &["model-a", "model-b"],
        );
        caps_changed_route.caps.json_output = true;
        let caps_changed = router(caps_changed_route);
        assert_ne!(
            configured_identities(&equivalent_a),
            configured_identities(&caps_changed)
        );

        let codex_account_absent = router(
            route(
                "codex",
                ProviderKind::OpenAiCodex,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_codex_session_auth(None),
        );
        let codex_account_present = router(
            route(
                "codex",
                ProviderKind::OpenAiCodex,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_codex_session_auth(Some("account-a".to_string())),
        );
        let codex_account_changed = router(
            route(
                "codex",
                ProviderKind::OpenAiCodex,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_codex_session_auth(Some("account-b".to_string())),
        );
        assert_ne!(
            configured_identities(&codex_account_absent),
            configured_identities(&codex_account_present)
        );
        assert_ne!(
            configured_identities(&codex_account_present),
            configured_identities(&codex_account_changed)
        );

        let grok_client_a = router(
            route(
                "grok",
                ProviderKind::GrokBuild,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_grok_session_auth("client-v1", "client-a"),
        );
        let grok_client_b = router(
            route(
                "grok",
                ProviderKind::GrokBuild,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_grok_session_auth("client-v1", "client-b"),
        );
        assert_ne!(
            configured_identities(&grok_client_a),
            configured_identities(&grok_client_b)
        );

        assert!(
            configured_identities(&equivalent_a).is_some_and(|identities| {
                identities.iter().any(|identity| {
                    identity
                        .windows("hya.provider.http.configured.v1".len())
                        .any(|bytes| bytes == b"hya.provider.http.configured.v1")
                        && identity
                            .windows(env!("CARGO_PKG_VERSION").len())
                            .any(|bytes| bytes == env!("CARGO_PKG_VERSION").as_bytes())
                })
            })
        );

        let resolver_count_a = Arc::new(AtomicUsize::new(0));
        let resolver_count_b = Arc::new(AtomicUsize::new(0));
        let resolver_route_a = {
            let counter = Arc::clone(&resolver_count_a);
            route(
                "resolver",
                ProviderKind::OpenAiCompatible,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_bearer_resolver(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok("live-token-a".to_string())
            }))
        };
        let resolver_route_b = {
            let counter = Arc::clone(&resolver_count_b);
            route(
                "resolver",
                ProviderKind::OpenAiCompatible,
                "https://example.test/v1",
                "secret",
                &["model"],
            )
            .with_bearer_resolver(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok("live-token-b".to_string())
            }))
        };
        let resolver_router_a = router(resolver_route_a);
        let resolver_router_b = router(resolver_route_b);
        assert_eq!(
            configured_identities(&resolver_router_a),
            configured_identities(&resolver_router_b)
        );
        assert_eq!(resolver_count_a.load(Ordering::SeqCst), 0);
        assert_eq!(resolver_count_b.load(Ordering::SeqCst), 0);
        assert_ne!(
            configured_identities(&resolver_router_a),
            configured_identities(&router(route(
                "resolver",
                ProviderKind::OpenAiCompatible,
                "https://example.test/v1",
                "secret",
                &["model"],
            )))
        );

        let fake_router = ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new())));
        assert_eq!(configured_identities(&fake_router), None);

        let dev_router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
        assert!(
            configured_identities(&dev_router)
                .as_ref()
                .is_some_and(|identities| !identities.is_empty())
        );

        assert!(equivalent_a.resolve(&ModelRef::new("model-a")).is_some());
        assert!(
            equivalent_a
                .resolve(&ModelRef::new("gateway/model-a"))
                .is_some()
        );
        assert!(
            equivalent_a
                .resolve(&ModelRef::new("foreign/model-a"))
                .is_none()
        );
    }
    #[test]
    fn per_model_reasoning_defaults_resolve_bare_and_prefixed_refs() -> Result<(), ProviderError> {
        let provider = provider()?.with_model_reasoning_defaults([
            ("claude-opus-4-8".to_string(), Some(ReasoningEffort::High)),
            ("gpt-5.5".to_string(), Some(ReasoningEffort::Off)),
        ]);
        assert_eq!(
            provider.reasoning_default(&ModelRef::new("claude-opus-4-8")),
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            provider.reasoning_default(&ModelRef::new("12th/claude-opus-4-8#high")),
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            provider.reasoning_default(&ModelRef::new("12th/gpt-5.5")),
            Some(ReasoningEffort::Off)
        );
        assert_eq!(
            provider.reasoning_default(&ModelRef::new("12th/missing")),
            None
        );
        Ok(())
    }

    #[test]
    fn provider_identity_covers_defaults_stably_and_without_secrets() -> Result<(), ProviderError> {
        let make = |defaults: Vec<(&str, Option<ReasoningEffort>)>| {
            provider()?
                .with_model_reasoning_defaults(
                    defaults
                        .into_iter()
                        .map(|(id, effort)| (id.to_string(), effort)),
                )
                .configured_identity_v1()
                .ok_or_else(|| ProviderError::Http("missing configured identity".to_string()))
        };
        let low = make(vec![("gpt-5.5", Some(ReasoningEffort::Low))])?;
        let high = make(vec![("gpt-5.5", Some(ReasoningEffort::High))])?;
        assert_ne!(low, high, "changing only a model default changes identity");
        let ordered = make(vec![
            ("gpt-5.5", Some(ReasoningEffort::Low)),
            ("claude-opus-4-8", Some(ReasoningEffort::Off)),
        ])?;
        let reversed = make(vec![
            ("claude-opus-4-8", Some(ReasoningEffort::Off)),
            ("gpt-5.5", Some(ReasoningEffort::Low)),
        ])?;
        assert_eq!(
            ordered, reversed,
            "default row insertion order is not semantic"
        );
        Ok(())
    }
    #[test]
    fn router_default_uses_first_claiming_provider() -> Result<(), ProviderError> {
        let first = provider()?;
        let second = provider()?.with_model_reasoning_defaults([(
            "claude-opus-4-8".to_string(),
            Some(ReasoningEffort::High),
        )]);
        let router = ProviderRouter::new()
            .with(Arc::new(first))
            .with(Arc::new(second));
        assert_eq!(
            router.reasoning_default(&ModelRef::new("12th/claude-opus-4-8")),
            None,
            "a later provider must not override the first claiming route"
        );
        assert_eq!(
            router.reasoning_default(&ModelRef::new("12th/unknown")),
            None
        );
        Ok(())
    }
    #[test]
    fn provider_effort_capability_is_route_specific_and_allocation_free()
    -> Result<(), ProviderError> {
        let provider = provider()?
            .with_model_reasoning_variants([("gpt-5.5".to_string(), vec!["low".to_string()])]);
        assert_eq!(
            provider
                .supports_reasoning_effort(&ModelRef::new("12th/gpt-5.5"), ReasoningEffort::Low,),
            Some(true)
        );
        assert_eq!(
            provider
                .supports_reasoning_effort(&ModelRef::new("12th/gpt-5.5"), ReasoningEffort::High,),
            Some(false)
        );
        assert_eq!(
            provider
                .supports_reasoning_effort(&ModelRef::new("12th/gpt-5.5"), ReasoningEffort::Off,),
            Some(true)
        );
        assert_eq!(
            provider
                .supports_reasoning_effort(&ModelRef::new("12th/missing"), ReasoningEffort::Low,),
            None
        );
        Ok(())
    }

    #[test]
    fn router_effort_capability_uses_first_claiming_provider() -> Result<(), ProviderError> {
        let first = provider()?;
        let second = provider()?.with_model_reasoning_variants([(
            "claude-opus-4-8".to_string(),
            vec!["high".to_string()],
        )]);
        let router = ProviderRouter::new()
            .with(Arc::new(first))
            .with(Arc::new(second));
        assert_eq!(
            router.supports_reasoning_effort(
                &ModelRef::new("12th/claude-opus-4-8"),
                ReasoningEffort::Low,
            ),
            Some(true),
            "later claiming routes must not override first-match capability"
        );
        Ok(())
    }
    /// Providers without optional metadata still derive effort support from capabilities.
    #[test]
    fn provider_without_metadata_uses_general_reasoning_capability() {
        let provider = DevProvider::new();
        assert_eq!(
            Provider::reasoning_default(&provider, &ModelRef::new("hya/offline")),
            None
        );
        assert_eq!(
            Provider::supports_reasoning_effort(
                &provider,
                &ModelRef::new("hya/offline"),
                ReasoningEffort::Low,
            ),
            Some(false)
        );
    }
}
