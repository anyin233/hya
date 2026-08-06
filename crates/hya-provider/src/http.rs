//! `HttpProvider` — drives a `Protocol` over reqwest + SSE into the canonical
//! `Event` stream. One provider per upstream route (OpenAI-compatible or
//! Anthropic), selected by the model id it serves.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{Event, Message, MessageId, ModelRef, SessionId};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

mod stream;

use crate::anthropic::AnthropicMessagesProtocol;
use crate::google::GoogleProtocol;
use crate::openai::{
    GrokBuildProtocol, OpenAiChatProtocol, OpenAiResponsesProtocol, encode_input_items,
};
use crate::{
    Capabilities, CompactedWindow, CompletionRequest, EventStream, Protocol, Provider,
    ProviderError, ProviderModel, append_capabilities_identity, append_identity_bytes,
    append_identity_count, append_identity_optional_bytes,
};

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
    /// Default reasoning-variant menu when a model has no per-model override.
    #[must_use]
    pub fn reasoning_variants(self) -> Vec<String> {
        let levels: &[&str] = match self {
            ProviderKind::Anthropic => &["low", "medium", "high", "max"],
            ProviderKind::OpenAiCompatible => &["minimal", "low", "medium", "high", "xhigh"],
            ProviderKind::OpenAiResponse | ProviderKind::OpenAiCodex => {
                &["none", "minimal", "low", "medium", "high", "xhigh", "max"]
            }
            ProviderKind::GrokBuild => &["low", "medium", "high"],
            ProviderKind::Google => &["high", "max"],
        };
        levels.iter().map(|level| (*level).to_string()).collect()
    }
}

/// Optional live bearer source for re-resolving tokens on each stream.
pub type BearerResolver = Arc<dyn Fn() -> Result<String, ProviderError> + Send + Sync>;

enum AuthStyle {
    Bearer(SecretString),
    /// ChatGPT Codex OAuth: Bearer JWT plus optional account id header.
    CodexSession {
        token: SecretString,
        account_id: Option<String>,
    },
    /// Grok Build OAuth session: Bearer JWT plus CLI chat-proxy session headers.
    GrokSession {
        token: SecretString,
        client_version: String,
        client_identifier: String,
    },
    Anthropic {
        key: SecretString,
        version: String,
    },
    Google(SecretString),
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
    models: HashSet<String>,
    model_reasoning_variants: BTreeMap<String, Vec<String>>,
    caps: Capabilities,
    kind: ProviderKind,
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
    /// Build a route for `kind` at `base_url` with static `api_key` and served model ids.
    pub fn new(
        id: impl Into<String>,
        kind: ProviderKind,
        base_url: &str,
        api_key: String,
        models: impl IntoIterator<Item = String>,
    ) -> Result<Self, ProviderError> {
        // Security: never follow redirects (reqwest keeps `x-api-key` across a
        // cross-origin 3xx). Connect-timeout only — a read timeout would abort
        // long streaming completions.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        let base = base_url.trim_end_matches('/');
        let key = SecretString::new(api_key);
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
            models: models.into_iter().collect(),
            model_reasoning_variants: BTreeMap::new(),
            kind,
            caps: Capabilities {
                streaming_tool_calls: true,
                parallel_tool_calls: true,
                usage_reporting: true,
                reasoning_request: true,
                max_context: 200_000,
                ..Capabilities::default()
            },
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

    /// Attach per-model reasoning variant lists (replaces kind defaults for those ids).
    #[must_use]
    pub fn with_model_reasoning_variants(
        mut self,
        variants: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        self.model_reasoning_variants = variants.into_iter().collect();
        self
    }

    fn resolve_bearer(&self, fallback: &SecretString) -> Result<String, ProviderError> {
        if let Some(resolver) = &self.bearer_resolver {
            return resolver();
        }
        Ok(fallback.expose_secret().clone())
    }

    fn auth_headers(&self, model_override: Option<&str>) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        match &self.auth {
            AuthStyle::Bearer(key) => {
                let token = self.resolve_bearer(key)?;
                headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
            }
            AuthStyle::CodexSession { token, account_id } => {
                let token = self.resolve_bearer(token)?;
                headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
                if let Some(account_id) = account_id.as_deref().filter(|s| !s.is_empty()) {
                    headers.insert(
                        HeaderName::from_static("chatgpt-account-id"),
                        sensitive(account_id)?,
                    );
                }
            }
            AuthStyle::GrokSession {
                token,
                client_version,
                client_identifier,
            } => {
                let token = self.resolve_bearer(token)?;
                headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
                headers.insert(
                    HeaderName::from_static("x-xai-token-auth"),
                    sensitive("xai-grok-cli")?,
                );
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
            }
            AuthStyle::Anthropic { key, version } => {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    sensitive(key.expose_secret())?,
                );
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_str(version)
                        .map_err(|_| ProviderError::Http("invalid version header".to_string()))?,
                );
            }
            AuthStyle::Google(key) => {
                headers.insert(
                    HeaderName::from_static("x-goog-api-key"),
                    sensitive(key.expose_secret())?,
                );
            }
        }
        Ok(headers)
    }

    fn request_headers(
        &self,
        extra: &BTreeMap<String, String>,
        model_override: Option<&str>,
    ) -> Result<HeaderMap, ProviderError> {
        let mut headers = self.auth_headers(model_override)?;
        for (name, value) in extra {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ProviderError::Http("invalid request header name".to_string()))?;
            headers.insert(header_name, request_header_value(value)?);
        }
        Ok(headers)
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

    // Compat addresses models as `providerID/modelID` (+ optional `#variant`);
    // the upstream route wants the bare `modelID`. Maps a served ref to that id.
    fn served_model_id(&self, model: &ModelRef) -> Option<String> {
        let base = match model.as_str().rsplit_once('#') {
            Some((head, variant)) if !variant.is_empty() => head,
            _ => model.as_str(),
        };
        if self.models.contains(base) {
            return Some(base.to_string());
        }
        if let Some((provider_id, model_id)) = base.split_once('/')
            && provider_id == self.id
            && self.models.contains(model_id)
        {
            return Some(model_id.to_string());
        }
        None
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
        for model in models {
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
            output.push(u8::from(!key.expose_secret().is_empty()));
        }
        AuthStyle::CodexSession { token, account_id } => {
            append_identity_bytes(output, b"codex-session")?;
            output.push(u8::from(!token.expose_secret().is_empty()));
            append_identity_optional_bytes(output, account_id.as_deref())?;
        }
        AuthStyle::GrokSession {
            token,
            client_version,
            client_identifier,
        } => {
            append_identity_bytes(output, b"grok-session")?;
            output.push(u8::from(!token.expose_secret().is_empty()));
            append_identity_bytes(output, client_version.as_bytes())?;
            append_identity_bytes(output, client_identifier.as_bytes())?;
        }
        AuthStyle::Anthropic { key, version } => {
            append_identity_bytes(output, b"anthropic")?;
            output.push(u8::from(!key.expose_secret().is_empty()));
            append_identity_bytes(output, version.as_bytes())?;
        }
        AuthStyle::Google(key) => {
            append_identity_bytes(output, b"google")?;
            output.push(u8::from(!key.expose_secret().is_empty()));
        }
    }
    Some(())
}

#[async_trait]
impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.served_model_id(model).map(|_| self.caps.clone())
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
        let decoder = self.protocol.decoder(session, message);
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
            .client
            .post(&url)
            .headers(self.request_headers(&req.headers, model_override)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = text.get(..500).unwrap_or(text.as_str());
            return Err(ProviderError::Http(format!("{status}: {snippet}")));
        }
        let (tx, rx) = mpsc::channel::<Result<Event, ProviderError>>(64);
        tokio::spawn(stream::pump(resp, decoder, tx));
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
        let resp = self
            .client
            .post(&url)
            .headers(self.request_headers(&BTreeMap::new(), model_override)?)
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
    use super::*;
    use crate::{DevProvider, FakeProvider, ProviderRouter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn provider() -> Result<HttpProvider, ProviderError> {
        HttpProvider::new(
            "12th",
            ProviderKind::OpenAiCompatible,
            "https://example/v1",
            "key".to_string(),
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
                key.to_string(),
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
}
