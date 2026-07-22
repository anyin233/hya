//! `HttpProvider` — drives a `Protocol` over reqwest + SSE into the canonical
//! `Event` stream. One provider per upstream route (OpenAI-compatible or
//! Anthropic), selected by the model id it serves.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{Event, MessageId, ModelRef, SessionId};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

mod stream;

use crate::anthropic::AnthropicMessagesProtocol;
use crate::google::GoogleProtocol;
use crate::openai::{GrokBuildProtocol, OpenAiChatProtocol, OpenAiResponsesProtocol};
use crate::{
    Capabilities, CompletionRequest, EventStream, Protocol, Provider, ProviderError, ProviderModel,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompatible,
    OpenAiResponse,
    /// ChatGPT Codex subscription backend (`chatgpt.com/backend-api/codex`).
    OpenAiCodex,
    GrokBuild,
    Anthropic,
    Google,
}

impl ProviderKind {
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
}

#[async_trait]
impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.served_model_id(model).map(|_| self.caps.clone())
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
