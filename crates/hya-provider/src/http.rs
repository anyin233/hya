//! `HttpProvider` — drives a `Protocol` over reqwest + SSE into the canonical
//! `Event` stream. One provider per upstream route (OpenAI-compatible or
//! Anthropic), selected by the model id it serves.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::{Event, Message, MessageId, ModelRef, SessionId};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, RETRY_AFTER};
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::ReceiverStream;

#[cfg(test)]
mod output_limit_tests;
mod stream;

use crate::anthropic::AnthropicMessagesProtocol;
use crate::auth::{AuthStyle, request_header_value};
use crate::google::GoogleProtocol;
use crate::openai::{
    GrokBuildProtocol, OpenAiChatProtocol, OpenAiResponsesProtocol, encode_input_items_for_model,
};
use crate::{
    Capabilities, CompactedWindow, CompletionRequest, EventStream, ModelCatalogSource, Protocol,
    Provider, ProviderError, ProviderModel, ReasoningEffort,
};

const MAX_REQUEST_ATTEMPTS: usize = 3;
const BASE_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded request budget before successful response headers establish a stream.
/// Transport, HTTP 429/5xx and one forced auth refresh share this budget.
/// Established streams are never replayed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryConfig {
    /// Total request attempts per streamed completion, including the first.
    /// Clamped to at least 1.
    pub max_attempts: usize,
    /// Exponential backoff seed for the first retry (`2^attempt` growth).
    pub backoff_base: Duration,
    /// Ceiling for the exponential backoff and `Retry-After` waits.
    pub backoff_max: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: MAX_REQUEST_ATTEMPTS,
            backoff_base: BASE_RETRY_DELAY,
            backoff_max: MAX_RETRY_AFTER,
        }
    }
}

impl RetryConfig {
    /// Clamp caller-supplied values onto a usable budget.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            max_attempts: self.max_attempts.max(1),
            backoff_base: self.backoff_base,
            backoff_max: self.backoff_max.max(self.backoff_base),
        }
    }
}

/// HTTP transport, credentials and pre-stream request policy.
struct RouteCore {
    client: reqwest::Client,
    auth: AuthStyle,
    bearer_resolver: Option<BearerResolver>,
    auth_refresher: Option<AuthRefresher>,
    response_header_timeout: Duration,
    retry: RetryConfig,
}

/// Bounded response-header wait for streaming completions and native compaction.
///
/// Reasoning endpoints behind large prompt contexts can legitimately take tens
/// of seconds to produce status line + headers, so 60s sits well above real
/// pre-header latency while keeping a hung route bounded and operator-visible.
/// A deadline miss becomes [`ProviderError::Transport`]. Configurable per route
/// via [`HttpProvider::with_response_header_timeout`].
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
    fn protocol(self) -> &'static dyn Protocol {
        match self {
            Self::OpenAiCompatible => &OpenAiChatProtocol,
            Self::OpenAiResponse | Self::OpenAiCodex => &OpenAiResponsesProtocol,
            Self::GrokBuild => &GrokBuildProtocol,
            Self::Anthropic => &AnthropicMessagesProtocol,
            Self::Google => &GoogleProtocol,
        }
    }

    /// Default reasoning-variant menu when a model has no per-model override.
    #[must_use]
    pub fn reasoning_variants(self) -> Vec<String> {
        self.protocol()
            .reasoning_variant_labels()
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

#[derive(Default)]
struct RouteModel {
    served: bool,
    reasoning_variants: Option<Vec<String>>,
    reasoning_default: Option<ReasoningEffort>,
    limit: Option<ModelLimitOverride>,
    reasoning_declared: Option<bool>,
    image_input: Option<bool>,
    display_name: Option<String>,
    source: Option<ModelCatalogSource>,
}

/// One configured HTTP route: reqwest client + [`Protocol`] + SSE → [`EventStream`].
///
/// Owns SSE framing; the protocol decoder only sees data payloads. Redirects are
/// disabled so auth headers are never followed cross-origin.
pub struct HttpProvider {
    /// Whether Anthropic prompt-cache breakpoints are enabled on this route.
    prompt_caching: bool,
    id: String,
    protocol: &'static dyn Protocol,
    core: RouteCore,
    base_url: String,
    models: BTreeMap<String, RouteModel>,
    caps: Capabilities,
    kind: ProviderKind,
    catalog_source: ModelCatalogSource,
    stream_idle_timeout: Duration,
}

/// Per-model token limits attached to an HTTP route.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelLimitOverride {
    /// Context window in tokens (`0` keeps the route default).
    pub context: u32,
    /// Max output tokens (`0` means unspecified). When set, it is the default
    /// and ceiling for the request's `max_output_tokens` on this model.
    pub output: u32,
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
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        let base = base_url.trim_end_matches('/');
        let key = api_key
            .filter(|value| !value.is_empty())
            .map(SecretString::new);
        let protocol = kind.protocol();
        let auth = match kind {
            ProviderKind::Anthropic => AuthStyle::Anthropic(key),
            ProviderKind::Google => AuthStyle::Google(key),
            _ => AuthStyle::Bearer(key),
        };
        let prompt_caching = protocol.supports_prompt_cache();
        Ok(Self {
            id: id.into(),
            protocol,
            core: RouteCore {
                client,
                auth,
                bearer_resolver: None,
                auth_refresher: None,
                response_header_timeout: RESPONSE_HEADER_TIMEOUT,
                retry: RetryConfig::default(),
            },
            base_url: base.to_owned(),
            models: models
                .into_iter()
                .map(|id| {
                    (
                        id,
                        RouteModel {
                            served: true,
                            ..RouteModel::default()
                        },
                    )
                })
                .collect(),
            kind,
            catalog_source: ModelCatalogSource::Configured,
            prompt_caching,
            caps: Capabilities {
                streaming_tool_calls: true,
                usage_reporting: true,
                reasoning_request: true,
                prompt_caching,
                max_context: 200_000,
                max_output: 0,
                ..Capabilities::default()
            },
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        })
    }

    /// Enable or disable Anthropic prompt-cache breakpoints for this route.
    #[must_use]
    pub fn with_prompt_cache(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled && self.protocol.supports_prompt_cache();
        self.caps.prompt_caching = self.prompt_caching;
        self
    }

    /// Switch a ChatGPT Codex provider to OAuth session auth (account id header).
    ///
    /// No-op for non-`OpenAiCodex` kinds so callers can chain unconditionally.
    #[must_use]
    pub fn with_codex_session_auth(mut self, account_id: Option<String>) -> Self {
        if self.kind != ProviderKind::OpenAiCodex {
            return self;
        }
        let token = self.core.auth.secret().cloned();
        self.core.auth = AuthStyle::CodexSession { token, account_id };
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
        let token = self.core.auth.secret().cloned();
        self.core.auth = AuthStyle::GrokSession {
            token,
            client_version: client_version.into(),
            client_identifier: client_identifier.into(),
        };
        self
    }

    /// Re-resolve the bearer token on each stream (hot-reload for Grok OAuth).
    #[must_use]
    pub fn with_bearer_resolver(mut self, resolver: BearerResolver) -> Self {
        self.core.bearer_resolver = Some(resolver);
        self
    }

    /// Attach the single-shot forced-refresh hook (auth-recovery level).
    ///
    /// See [`AuthRefresher`] for the contract. Strictly pre-stream: once an
    /// event stream is established nothing is refreshed or replayed.
    #[must_use]
    pub fn with_auth_refresher(mut self, refresher: AuthRefresher) -> Self {
        self.core.auth_refresher = Some(refresher);
        self
    }

    /// Attach per-model reasoning variant lists (replaces kind defaults for those ids).
    #[must_use]
    pub fn with_model_reasoning_variants(
        mut self,
        variants: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.reasoning_variants = None;
        }
        for (id, value) in variants {
            self.models.entry(id).or_default().reasoning_variants = Some(value);
        }
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
        for row in self.models.values_mut() {
            row.reasoning_default = None;
        }
        for (id, value) in defaults
            .into_iter()
            .filter_map(|(id, value)| value.map(|value| (id, value)))
        {
            self.models.entry(id).or_default().reasoning_default = Some(value);
        }
        self
    }

    /// Attach per-model reasoning support declared by model metadata (remote
    /// model list or config `reasoning`). Models absent here publish
    /// [`ProviderModel::reasoning`] as unknown; this never changes which
    /// efforts a request may use.
    #[must_use]
    pub fn with_model_reasoning_declared(
        mut self,
        declared: impl IntoIterator<Item = (String, bool)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.reasoning_declared = None;
        }
        for (id, value) in declared {
            self.models.entry(id).or_default().reasoning_declared = Some(value);
        }
        self
    }

    /// Attach per-model context / max-output limits (configured `limit` blocks
    /// or the durable models cache). A non-zero `output` becomes the default
    /// and ceiling for that model's request `max_output_tokens`.
    #[must_use]
    pub fn with_model_limits(
        mut self,
        limits: impl IntoIterator<Item = (String, ModelLimitOverride)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.limit = None;
        }
        for (id, value) in limits {
            self.models.entry(id).or_default().limit = Some(value);
        }
        self
    }

    /// Attach per-model image-input support (config `modalities.input`
    /// contains `image`). Models without an entry report unknown.
    #[must_use]
    pub fn with_model_image_input(
        mut self,
        support: impl IntoIterator<Item = (String, bool)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.image_input = None;
        }
        for (id, value) in support {
            self.models.entry(id).or_default().image_input = Some(value);
        }
        self
    }

    /// Set the provenance emitted by this route's catalog rows.
    #[must_use]
    pub fn with_catalog_source(mut self, source: ModelCatalogSource) -> Self {
        self.catalog_source = source;
        self
    }

    /// Attach per-model display names (presentation only).
    #[must_use]
    pub fn with_model_display_names(
        mut self,
        names: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.display_name = None;
        }
        for (id, value) in names
            .into_iter()
            .filter(|(_, value)| !value.trim().is_empty())
        {
            self.models.entry(id).or_default().display_name = Some(value);
        }
        self
    }

    /// Attach per-model catalog provenance (`remote`, `config`, or
    /// `override`); models without an entry use the route-wide source from
    /// [`Self::with_catalog_source`].
    #[must_use]
    pub fn with_model_sources(
        mut self,
        sources: impl IntoIterator<Item = (String, ModelCatalogSource)>,
    ) -> Self {
        for row in self.models.values_mut() {
            row.source = None;
        }
        for (id, value) in sources {
            self.models.entry(id).or_default().source = Some(value);
        }
        self
    }

    /// Override the bounded response-header wait ([`RESPONSE_HEADER_TIMEOUT`])
    /// for this route — for gateways slower than the default anticipates, and
    /// for tests exercising the deadline.
    #[must_use]
    pub fn with_response_header_timeout(mut self, duration: Duration) -> Self {
        self.core.response_header_timeout = duration;
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

    /// Override the pre-stream attempt budget and backoff; see [`RetryConfig`].
    #[must_use]
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.core.retry = retry.normalized();
        self
    }
}

impl RouteCore {
    /// Credential material this route authenticates with right now.
    fn active_credential(&self) -> Result<Option<String>, ProviderError> {
        if self.auth.is_bearer()
            && let Some(resolver) = &self.bearer_resolver
        {
            return resolver().map(|token| (!token.is_empty()).then_some(token));
        }
        Ok(self.auth.credential().map(str::to_owned))
    }

    fn auth_headers(
        &self,
        model_override: Option<&str>,
    ) -> Result<(HeaderMap, Option<String>), ProviderError> {
        let mut headers = HeaderMap::new();
        let bearer = if self.auth.is_bearer() {
            self.active_credential()?
        } else {
            None
        };
        let credential = if self.auth.is_bearer() {
            bearer.as_deref()
        } else {
            self.auth.credential()
        };
        self.auth
            .apply_headers(&mut headers, credential, true, model_override)?;
        Ok((headers, bearer))
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

    /// Send a JSON POST with a bounded response-header wait on each attempt.
    /// `max_attempts` includes the initial request and any retries.
    async fn send_request(
        &self,
        url: &str,
        body: &Value,
        extra_headers: &BTreeMap<String, String>,
        model_override: Option<&str>,
        max_attempts: usize,
    ) -> Result<reqwest::Response, ProviderError> {
        let max_attempts = max_attempts.max(1);
        let started = Instant::now();
        // Auth-recovery level: the forced-refresh retry fires at most once per
        // request and always occupies one of the attempt slots below — it never
        // extends the budget, so broken credentials cannot degrade into a
        // refresh/retry loop. Once a response succeeds, no refresh or retry
        // exists anymore (no-replay boundary above us).
        let mut auth_recovered = false;
        for attempt in 0..max_attempts {
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
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(response);
                }
                Ok(Ok(response)) => {
                    let status = response.status().as_u16();
                    let try_refresh = !auth_recovered
                        && self.auth_refresher.is_some()
                        && (status == 401 || status == 403)
                        // Budget integrity: only fire while the retried
                        // request still fits inside the attempt cap.
                        && attempt + 1 < max_attempts;
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
                // attempts still run.
                Err(_elapsed) => ProviderError::Transport(format!(
                    "no response headers within {:#?}",
                    self.response_header_timeout
                )),
            };
            if attempt + 1 == max_attempts || !error.is_retryable_before_stream() {
                let attempts = attempt + 1;
                if attempts > 1 && error.is_retryable_before_stream() {
                    let detail = format!(
                        "after {attempts} attempts over {:.1}s",
                        started.elapsed().as_secs_f64()
                    );
                    return Err(match error {
                        ProviderError::HttpStatus {
                            status,
                            message,
                            retry_after,
                        } => ProviderError::HttpStatus {
                            status,
                            message: format!("{detail}: {message}"),
                            retry_after,
                        },
                        ProviderError::Transport(message) => {
                            ProviderError::Transport(format!("{detail}: {message}"))
                        }
                        other => other,
                    });
                }
                return Err(error);
            }
            sleep(retry_delay(&error, attempt, &self.retry)).await;
        }
        Err(ProviderError::Transport(
            "provider request exhausted without a response".to_string(),
        ))
    }
}

impl HttpProvider {
    /// The protocol this route speaks.
    ///
    /// A provider whose model entries override `kind` is partitioned into one
    /// route per effective protocol, all sharing the provider id; this getter
    /// tells the protocols apart for diagnostics and tests.
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        self.kind
    }

    /// Merge route defaults with any per-model limit overrides.
    fn caps_for_model(&self, row: Option<&RouteModel>) -> Capabilities {
        let mut caps = self.caps.clone();
        if let Some(limit) = row.and_then(|row| row.limit.as_ref()) {
            if limit.context > 0 {
                caps.max_context = limit.context;
            }
            caps.max_output = limit.output;
        }
        if let Some(image_input) = row.and_then(|row| row.image_input) {
            caps.image_input = Some(image_input);
        }
        if self
            .models
            .values()
            .any(|row| row.reasoning_variants.is_some())
            || row.is_some_and(|row| row.reasoning_default.is_some())
        {
            caps.reasoning_request = true;
        }
        caps
    }

    /// Capabilities published on a catalog row: [`Self::caps_for_model`],
    /// except that the context window is the model's known limit or 0 when
    /// unknown (the route-wide default is a runtime fallback, not metadata).
    fn catalog_caps_for_model(&self, row: &RouteModel) -> Capabilities {
        let mut caps = self.caps_for_model(Some(row));
        caps.max_context = row.limit.as_ref().map_or(0, |limit| limit.context);
        caps
    }

    /// Rewrite the request to the bare upstream model id, apply the model's
    /// known output limit, and encode its body.
    ///
    /// A known limit (`max_output > 0` from [`ModelLimitOverride`]) is the
    /// default for an unset `max_output_tokens` and clamps an explicit one.
    /// Without a known limit the request is encoded unchanged.
    fn prepare_request(
        &self,
        mut req: CompletionRequest,
    ) -> Result<(CompletionRequest, Value), ProviderError> {
        let caps = self
            .served_model_name(&req.model)
            .map(|model| self.caps_for_model(self.models.get(model)));
        let output_limit = caps
            .as_ref()
            .map(|caps| caps.max_output)
            .filter(|limit| *limit > 0);
        let images = caps.is_some_and(|caps| self.sends_tool_images(&caps));
        if let Some(model_id) = self.served_model_id(&req.model) {
            req.model = ModelRef::new(model_id);
        }
        if let Some(limit) = output_limit {
            req.max_output_tokens = Some(req.max_output_tokens.map_or(limit, |max| max.min(limit)));
        }
        let mut body = self.protocol.encode_for_model(&req, output_limit, images)?;
        self.protocol
            .post_encode(&mut body, &mut req.headers, self.prompt_caching);
        Ok((req, body))
    }

    /// Return the claimed bare model id without allocating.
    fn served_model_name<'a>(&'a self, model: &'a ModelRef) -> Option<&'a str> {
        let base = match model.as_str().rsplit_once('#') {
            Some((head, variant)) if !variant.is_empty() => head,
            _ => model.as_str(),
        };
        if self.models.get(base).is_some_and(|row| row.served) {
            return Some(base);
        }
        if let Some((provider_id, model_id)) = base.split_once('/')
            && provider_id == self.id
            && self.models.get(model_id).is_some_and(|row| row.served)
        {
            return Some(model_id);
        }
        None
    }

    /// Resolve a claimed model to the bare upstream id for request encoding.
    fn served_model_id(&self, model: &ModelRef) -> Option<String> {
        self.served_model_name(model).map(str::to_owned)
    }
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

fn retry_delay(error: &ProviderError, attempt: usize, retry: &RetryConfig) -> Duration {
    if let Some(delay) = error.retry_after() {
        return delay.min(MAX_RETRY_AFTER).min(retry.backoff_max);
    }
    let exponent = u32::try_from(attempt).unwrap_or(u32::MAX).min(8);
    let base = retry
        .backoff_base
        .saturating_mul(2_u32.saturating_pow(exponent))
        .min(retry.backoff_max);
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
        let served = self.served_model_name(model)?;
        Some(self.caps_for_model(self.models.get(served)))
    }

    fn sends_tool_images(&self, caps: &Capabilities) -> bool {
        self.protocol.supports_tool_images() && caps.image_input == Some(true)
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        self.protocol.reasoning_replay_policy()
    }

    fn reasoning_default(&self, model: &ModelRef) -> Option<ReasoningEffort> {
        let served_model = self.served_model_name(model)?;
        self.models
            .get(served_model)
            .and_then(|row| row.reasoning_default.as_ref())
            .copied()
    }

    fn supports_reasoning_effort(&self, model: &ModelRef, effort: ReasoningEffort) -> Option<bool> {
        let served_model = self.served_model_name(model)?;
        if effort == ReasoningEffort::Off {
            return Some(true);
        }
        if !self.caps.reasoning_request {
            return Some(false);
        }
        if let Some(variants) = self
            .models
            .get(served_model)
            .and_then(|row| row.reasoning_variants.as_ref())
        {
            return Some(
                variants
                    .iter()
                    .any(|variant| ReasoningEffort::parse(variant) == Some(effort)),
            );
        }
        Some(
            self.protocol
                .reasoning_variant_labels()
                .iter()
                .any(|variant| ReasoningEffort::parse(variant) == Some(effort)),
        )
    }

    fn catalog(&self) -> Vec<ProviderModel> {
        let variants = if self.caps.reasoning_request {
            self.kind.reasoning_variants()
        } else {
            Vec::new()
        };
        self.models
            .iter()
            .filter(|(_, row)| row.served)
            .map(|(model, row)| ProviderModel {
                provider_id: self.id.clone(),
                model_id: model.clone(),
                capabilities: self.catalog_caps_for_model(row),
                reasoning_variants: row
                    .reasoning_variants
                    .clone()
                    .unwrap_or_else(|| variants.clone()),
                reasoning_default: row.reasoning_default,
                reasoning: row.reasoning_declared,
                display_name: row.display_name.clone(),
                source: row.source.unwrap_or(self.catalog_source),
            })
            .collect()
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let (req, body) = self.prepare_request(req)?;
        let url = self
            .protocol
            .endpoint_url(&self.base_url, req.model.as_str());
        let model_override = self.protocol.session_model_override(req.model.as_str());
        let resp = self
            .core
            .send_request(
                &url,
                &body,
                &req.headers,
                model_override,
                self.core.retry.max_attempts,
            )
            .await?;
        let decoder = self.protocol.decoder(session, message, req.reasoning);
        let idle_timeout = self.stream_idle_timeout;
        let (tx, rx) = mpsc::channel::<Result<Event, ProviderError>>(64);
        tokio::spawn(async move {
            stream::pump(resp, decoder, tx, idle_timeout).await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn compact_responses(
        &self,
        model: &ModelRef,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        let Some(url) = self.protocol.compact_endpoint(&self.base_url) else {
            return Ok(None);
        };
        let model_id = self
            .served_model_id(model)
            .unwrap_or_else(|| model.as_str().to_string());
        let images = self
            .capabilities(model)
            .is_some_and(|caps| self.sends_tool_images(&caps));
        // Encode the same input window the create path would send.
        let target = ModelRef::new(&model_id);
        let mut input = encode_input_items_for_model(messages, images, Some(&target))?;
        if let Some(system) = system.filter(|s| !s.is_empty()) {
            // Prefer instructions-equivalent as a system item at the front when
            // the compact endpoint is given an explicit system prompt.
            input.insert(0, json!({"role": "system", "content": system}));
        }
        let body = json!({
            "model": model_id,
            "input": input,
        });
        let model_override = self.protocol.session_model_override(model_id.as_str());
        let resp = self
            .core
            .send_request(&url, &body, &BTreeMap::new(), model_override, 1)
            .await?;
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
        let (headers, _) = provider
            .core
            .request_headers(&BTreeMap::new(), None)
            .unwrap();
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
            let (headers, _) = provider
                .core
                .request_headers(&BTreeMap::new(), None)
                .unwrap();
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
        let (headers, _) = grok.core.request_headers(&BTreeMap::new(), None).unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key(HeaderName::from_static("x-xai-token-auth")));
        assert_eq!(
            headers
                .get(HeaderName::from_static("x-grok-client-version"))
                .and_then(|value| value.to_str().ok()),
            Some("client-v1")
        );
    }
    use crate::{DevProvider, ProviderRouter};

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
    fn catalog_rows_carry_per_model_display_names_and_sources() -> Result<(), ProviderError> {
        let provider = provider()?
            .with_catalog_source(ModelCatalogSource::Discovered)
            .with_model_display_names([("gpt-5.5".to_string(), "GPT 5.5".to_string())])
            .with_model_sources([(
                "claude-opus-4-8".to_string(),
                ModelCatalogSource::Overridden,
            )]);
        let rows = Provider::catalog(&provider);
        let gpt = rows.iter().find(|row| row.model_id == "gpt-5.5").unwrap();
        assert_eq!(gpt.display_name.as_deref(), Some("GPT 5.5"));
        assert_eq!(gpt.source, ModelCatalogSource::Discovered);
        let claude = rows
            .iter()
            .find(|row| row.model_id == "claude-opus-4-8")
            .unwrap();
        assert_eq!(claude.display_name, None);
        assert_eq!(claude.source, ModelCatalogSource::Overridden);
        Ok(())
    }

    #[test]
    fn metadata_builders_filter_empty_values_without_registering_models()
    -> Result<(), ProviderError> {
        let provider = provider()?
            .with_model_display_names([
                ("gpt-5.5".to_string(), "GPT".to_string()),
                ("gpt-5.5".to_string(), " ".to_string()),
                ("unserved".to_string(), "Other".to_string()),
            ])
            .with_model_reasoning_defaults([
                ("gpt-5.5".to_string(), Some(ReasoningEffort::High)),
                ("gpt-5.5".to_string(), None),
            ]);
        let rows = provider.catalog();
        let row = rows.iter().find(|row| row.model_id == "gpt-5.5").unwrap();
        assert_eq!(row.display_name.as_deref(), Some("GPT"));
        assert_eq!(row.reasoning_default, Some(ReasoningEffort::High));
        assert!(provider.capabilities(&ModelRef::new("unserved")).is_none());
        assert!(!rows.iter().any(|row| row.model_id == "unserved"));
        Ok(())
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
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            ProviderKind::OpenAiCompatible.reasoning_variants(),
            ["minimal", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            ProviderKind::Google.reasoning_variants(),
            ["minimal", "low", "medium", "high", "max"]
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
